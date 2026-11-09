//! Starts `nibwarden-gui prompt` and talks to it over its stdin/stdout (phase2.md E3,
//! §5.2). Only a process the daemon started itself can answer, so a program connected to
//! the control socket cannot pose as the unlock or confirmation window.
//!
//! One window at a time. Requests that only need the vault unlocked join the open unlock
//! window's waiting list (phase2.md §4.1) and all get its outcome; requests that need a
//! confirmation get a window of their own (phase2.md §5.3).

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use nibwarden_ipc::{
    ErrorBody, ErrorCode, FrameError, LockState,
    prompt::{Access, Answer, Prompt, QuickUnlock, Requester, RetryReason, Scope},
    read_frame, write_frame,
};
use nibwarden_platform::peer::Peer;
use tokio::{
    process::{Child, ChildStdin, Command},
    sync::{Mutex as AsyncMutex, Notify, mpsc, watch},
    task::JoinHandle,
};

use crate::{approvals::requester, daemon::Daemon};

/// How long a window may stay open before the requests waiting on it fail.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(120);

type Outcome = Result<(), ErrorBody>;

/// Result of unlocking for one request.
pub enum Unlocked {
    /// The user unlocked in a window that showed the request, which allows it;
    /// `all_applications` if they ticked "Approve for all applications".
    AndAllowed { all_applications: bool },
    /// The vault was already unlocked (e.g. by someone else meanwhile): the request still
    /// needs its own confirmation.
    Already,
}

pub struct PromptBroker {
    /// The `nibwarden-gui` binary; `None` if it could not be located.
    gui: Option<PathBuf>,
    /// Held while any window is open.
    window: Arc<AsyncMutex<()>>,
    /// The unlock window currently open for requests that only need the vault.
    current: Arc<Mutex<Option<Arc<UnlockRound>>>>,
}

/// One unlock window and everyone waiting on it.
struct UnlockRound {
    waiting: Mutex<Vec<Requester>>,
    joined: Notify,
    outcome: watch::Sender<Option<Outcome>>,
}

impl PromptBroker {
    pub fn new(gui: Option<PathBuf>) -> Self {
        Self {
            gui,
            window: Arc::new(AsyncMutex::new(())),
            current: Arc::new(Mutex::new(None)),
        }
    }

    /// The GUI to start for `peer`, or why no window can be shown for it: no display, no
    /// GUI, or a remote session (phase2.md §6).
    fn gui_for(&self, peer: &Peer) -> Result<&Path, ErrorBody> {
        let unavailable = |message: &str| Err(ErrorBody::new(ErrorCode::Unavailable, message));
        if peer.remote_session {
            return unavailable("the request comes from a remote session");
        }
        if !nibwarden_platform::display::available() {
            return unavailable("there is no graphical session to show a window in");
        }
        match &self.gui {
            Some(gui) => Ok(gui),
            None => unavailable("nibwarden-gui is not installed"),
        }
    }

    /// Whether `peer` is our own GUI (quick access): the user acts in it directly, so what
    /// it reads needs no confirmation window on top (phase2.md §7).
    pub fn is_own_gui(&self, peer: &Peer) -> bool {
        let canonical = |p: &Path| std::fs::canonicalize(p).ok();
        match (&peer.exe, &self.gui) {
            (Some(exe), Some(gui)) => canonical(gui).is_some_and(|gui| *exe == gui),
            _ => false,
        }
    }

    pub fn can_prompt(&self, peer: &Peer) -> bool {
        self.gui_for(peer).is_ok()
    }

    /// Waits until the vault is unlocked through the unlock window, opening one or joining
    /// the open one. Fails with `unavailable` if no window can be shown for `requester`,
    /// and `denied` if the user cancelled or nobody answered in time.
    pub async fn unlock(&self, daemon: &Arc<Daemon>, peer: &Peer) -> Outcome {
        match daemon.lock_state().await {
            LockState::Unlocked => return Ok(()),
            LockState::LoggedOut => return Err(logged_out()),
            LockState::Locked => {}
        }
        let gui = self.gui_for(peer)?.to_owned();
        let me = requester(peer);
        let mut outcome = {
            let mut current = self.current.lock().expect("poisoned");
            match &*current {
                Some(round) => {
                    round.waiting.lock().expect("poisoned").push(me);
                    round.joined.notify_one();
                    round.outcome.subscribe()
                }
                None => {
                    let round = Arc::new(UnlockRound {
                        waiting: Mutex::new(vec![me]),
                        joined: Notify::new(),
                        outcome: watch::channel(None).0,
                    });
                    let receiver = round.outcome.subscribe();
                    *current = Some(round.clone());
                    tokio::spawn(drive_round(
                        gui,
                        self.window.clone(),
                        self.current.clone(),
                        daemon.clone(),
                        round,
                    ));
                    receiver
                }
            }
        };
        match outcome.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.clone().expect("checked"),
            Err(_) => Err(ErrorBody::new(
                ErrorCode::Internal,
                "unlock window vanished",
            )),
        }
    }

    /// "Unlock and allow" (phase2.md §5.3): one window that shows the request and takes
    /// the master password, which both unlocks and allows it.
    pub async fn unlock_for(
        &self,
        daemon: &Arc<Daemon>,
        peer: &Peer,
        access: Access,
        application: Option<Requester>,
        scope: Scope,
    ) -> Result<Unlocked, ErrorBody> {
        let gui = self.gui_for(peer)?;
        let _window = self.window.lock().await;
        match daemon.lock_state().await {
            LockState::Unlocked => return Ok(Unlocked::Already),
            LockState::LoggedOut => return Err(logged_out()),
            LockState::Locked => {}
        }
        let first = Prompt::Unlock {
            email: daemon.status().await.email.unwrap_or_default(),
            waiting: vec![access.requester.clone()],
            access: Some(access),
            application,
            scope,
            quick: quick(daemon).await,
        };
        let mut window = Window::open(gui, &first).await?;
        let pid = window.child.id();
        let result = window
            .run(|answer| async move { try_unlock(daemon, answer, pid).await })
            .await;
        window.close().await;
        result.map(|all_applications| Unlocked::AndAllowed { all_applications })
    }

    /// Asks whether to allow a request on the unlocked vault; `Ok(all_applications)` if
    /// allowed.
    pub async fn approve(
        &self,
        peer: &Peer,
        access: Access,
        application: Option<Requester>,
        scope: Scope,
    ) -> Result<bool, ErrorBody> {
        let gui = self.gui_for(peer)?;
        let _window = self.window.lock().await;
        let first = Prompt::Approve {
            access,
            application,
            scope,
        };
        let mut window = Window::open(gui, &first).await?;
        let result = window
            .run(|answer| async move {
                match answer {
                    Answer::Allow { all_applications } => Step::Done(Ok(all_applications)),
                    _ => Step::Done(Err(ErrorBody::new(ErrorCode::Denied, "request denied"))),
                }
            })
            .await;
        window.close().await;
        result
    }

    /// Asks for the master password again, for an item that requires it (reprompt).
    pub async fn reprompt(&self, daemon: &Arc<Daemon>, peer: &Peer, access: Access) -> Outcome {
        let gui = self.gui_for(peer).map_err(|e| {
            ErrorBody::new(
                ErrorCode::Denied,
                format!(
                    "this item asks for the master password again, but {}",
                    e.message
                ),
            )
        })?;
        let _window = self.window.lock().await;
        let first = Prompt::Reprompt {
            email: daemon.status().await.email.unwrap_or_default(),
            access,
        };
        let mut window = Window::open(gui, &first).await?;
        let result = window
            .run(|answer| async move {
                let Answer::Password { password, .. } = answer else {
                    return Step::Done(Err(cancelled()));
                };
                match daemon.verify_password(&password).await {
                    Ok(true) => Step::Done(Ok(())),
                    Ok(false) => Step::Retry(RetryReason::WrongPassword),
                    Err(e) => Step::Done(Err(e)),
                }
            })
            .await;
        window.close().await;
        result
    }
}

/// Runs one shared unlock window to its end and hands the outcome to every waiter.
async fn drive_round(
    gui: PathBuf,
    window_lock: Arc<AsyncMutex<()>>,
    current: Arc<Mutex<Option<Arc<UnlockRound>>>>,
    daemon: Arc<Daemon>,
    round: Arc<UnlockRound>,
) {
    let result = {
        let _window = window_lock.lock().await;
        match tokio::time::timeout(WINDOW_TIMEOUT, run_round(&gui, &daemon, &round)).await {
            Ok(result) => result,
            Err(_) => Err(timed_out()),
        }
    };
    tracing::info!(ok = result.is_ok(), "unlock window closed");
    // Detach first: a request arriving from now on starts afresh (and finds the vault
    // unlocked, or opens a new window).
    *current.lock().expect("poisoned") = None;
    round.outcome.send_replace(Some(result));
}

async fn run_round(gui: &Path, daemon: &Arc<Daemon>, round: &UnlockRound) -> Outcome {
    // Unlocked from elsewhere (e.g. the terminal) while waiting for the window slot.
    match daemon.lock_state().await {
        LockState::Unlocked => return Ok(()),
        LockState::LoggedOut => return Err(logged_out()),
        LockState::Locked => {}
    }
    let snapshot = || round.waiting.lock().expect("poisoned").clone();
    tracing::info!(waiting = ?snapshot(), "unlock window opened");
    let first = Prompt::Unlock {
        email: daemon.status().await.email.unwrap_or_default(),
        waiting: snapshot(),
        access: None,
        application: None,
        scope: Scope::default(),
        quick: quick(daemon).await,
    };
    let mut window = Window::open(gui, &first).await?;
    let pid = window.child.id();
    let Window {
        to_gui, answers, ..
    } = &mut window;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let result = loop {
        tokio::select! {
            answer = answers.recv() => {
                match try_unlock(daemon, to_answer(answer), pid).await {
                    Step::Done(result) => break result.map(drop),
                    Step::Retry(reason) => {
                        if let Err(e) = send(to_gui, &Prompt::Retry { reason }).await {
                            break Err(e);
                        }
                    }
                }
            }
            () = round.joined.notified() => {
                if let Err(e) = send(to_gui, &Prompt::Waiting { waiting: snapshot() }).await {
                    break Err(e);
                }
            }
            _ = tick.tick() => {
                // Every waiter gave up (e.g. ssh was interrupted): close the window.
                if round.outcome.receiver_count() == 0 {
                    break Err(ErrorBody::new(ErrorCode::Denied, "nobody is waiting"));
                }
                // Unlocked another way while the window was open.
                if daemon.lock_state().await == LockState::Unlocked {
                    break Ok(());
                }
            }
        }
    };
    window.close().await;
    result
}

/// What to do after an answer.
enum Step<T> {
    Done(Result<T, ErrorBody>),
    /// A wrong answer: say why and keep the window open.
    Retry(RetryReason),
}

/// Unlocks with an unlock window's answer, the master password, the PIN or the system's
/// confirmation (asked for the window's process `window`).
async fn try_unlock(daemon: &Arc<Daemon>, answer: Answer, window: Option<u32>) -> Step<bool> {
    let (result, all_applications, wrong) = match answer {
        Answer::Password {
            password,
            all_applications,
        } => (
            daemon.unlock(&password).await,
            all_applications,
            RetryReason::WrongPassword,
        ),
        Answer::Pin {
            pin,
            all_applications,
        } => match daemon.unlock_with_pin(&pin).await {
            Err(e) if e.code == ErrorCode::Unavailable => {
                return Step::Retry(RetryReason::PinSuspended);
            }
            result => (result, all_applications, RetryReason::WrongPin),
        },
        Answer::System { all_applications } => {
            let Some(pid) = window else {
                return Step::Retry(RetryReason::NotConfirmed);
            };
            match daemon.unlock_with_system(pid).await {
                Err(e) if e.code == ErrorCode::Unavailable => {
                    tracing::info!(reason = e.message, "system authentication failed");
                    return Step::Retry(RetryReason::NotConfirmed);
                }
                result => (result, all_applications, RetryReason::NotConfirmed),
            }
        }
        _ => return Step::Done(Err(cancelled())),
    };
    match result {
        Ok(()) => Step::Done(Ok(all_applications)),
        Err(e) if e.code == ErrorCode::Denied => Step::Retry(wrong),
        Err(e) => Step::Done(Err(e)),
    }
}

async fn quick(daemon: &Daemon) -> QuickUnlock {
    let (pin, system) = daemon.quick_unlock().await;
    QuickUnlock { pin, system }
}

/// One running `nibwarden-gui prompt` process.
struct Window {
    child: Child,
    to_gui: ChildStdin,
    /// Answers, read in their own task so no frame is ever cut in half by a `select!`.
    answers: mpsc::Receiver<Result<Answer, FrameError>>,
    reader: JoinHandle<()>,
}

impl Window {
    async fn open(gui: &Path, first: &Prompt) -> Result<Self, ErrorBody> {
        let mut child = Command::new(gui)
            .arg("prompt")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ErrorBody::new(
                    ErrorCode::Unavailable,
                    format!("cannot start {}: {e}", gui.display()),
                )
            })?;
        let mut to_gui = child.stdin.take().expect("piped");
        let mut from_gui = child.stdout.take().expect("piped");
        let (answer_tx, answers) = mpsc::channel(4);
        let reader = tokio::spawn(async move {
            loop {
                let answer = read_frame::<Answer>(&mut from_gui).await;
                let end = answer.is_err();
                if answer_tx.send(answer).await.is_err() || end {
                    break;
                }
            }
        });
        send(&mut to_gui, first).await?;
        Ok(Self {
            child,
            to_gui,
            answers,
            reader,
        })
    }

    /// Feeds answers to `step` until it is done, retrying on a wrong password, within the
    /// window timeout.
    async fn run<T, F, Fut>(&mut self, mut step: F) -> Result<T, ErrorBody>
    where
        F: FnMut(Answer) -> Fut,
        Fut: Future<Output = Step<T>>,
    {
        let conversation = async {
            loop {
                match step(to_answer(self.answers.recv().await)).await {
                    Step::Done(result) => return result,
                    Step::Retry(reason) => {
                        send(&mut self.to_gui, &Prompt::Retry { reason }).await?;
                    }
                }
            }
        };
        match tokio::time::timeout(WINDOW_TIMEOUT, conversation).await {
            Ok(result) => result,
            Err(_) => Err(timed_out()),
        }
    }

    /// Closing its stdin tells the window to go away; make sure it does.
    async fn close(mut self) {
        drop(self.to_gui);
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        self.reader.abort();
    }
}

/// A closed window or a broken pipe counts as "cancel".
fn to_answer(answer: Option<Result<Answer, FrameError>>) -> Answer {
    match answer {
        Some(Ok(answer)) => answer,
        Some(Err(FrameError::Closed)) | None => Answer::Cancel,
        Some(Err(e)) => {
            tracing::warn!(error = %e, "window answered garbage");
            Answer::Cancel
        }
    }
}

async fn send(to_gui: &mut ChildStdin, prompt: &Prompt) -> Outcome {
    write_frame(to_gui, prompt).await.map_err(|e| {
        tracing::warn!(error = %e, "window failed");
        ErrorBody::new(ErrorCode::Unavailable, format!("window failed: {e}"))
    })
}

fn cancelled() -> ErrorBody {
    ErrorBody::new(ErrorCode::Denied, "cancelled in the window")
}

fn timed_out() -> ErrorBody {
    ErrorBody::new(ErrorCode::Denied, "the window timed out")
}

fn logged_out() -> ErrorBody {
    ErrorBody::new(ErrorCode::LoggedOut, "not logged in")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_its_own_gui_only() {
        let exe = std::env::current_exe().unwrap();
        let broker = PromptBroker::new(Some(exe.clone()));
        assert!(broker.is_own_gui(&Peer::synthetic(1, Some(exe))));
        assert!(!broker.is_own_gui(&Peer::synthetic(1, Some("/usr/bin/env".into()))));
        assert!(!broker.is_own_gui(&Peer::synthetic(1, None)));
        assert!(
            !PromptBroker::new(None).is_own_gui(&Peer::synthetic(1, Some("/usr/bin/env".into())))
        );
    }
}
