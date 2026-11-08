//! `nibwarden-gui prompt`: one prompt from the daemon, answered over stdin/stdout.
//!
//! Threads: the main thread runs the Slint event loop; an I/O thread reads prompts from
//! stdin (forwarding them to the UI with `invoke_from_event_loop`) and writes answers
//! from a channel to stdout. When the daemon closes stdin, the window goes away.

use std::{
    cell::RefCell,
    io::IsTerminal as _,
    process::ExitCode,
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};

use nibwarden_ipc::{
    prompt::{
        Access, Action, Answer, BrowserUse, Prompt, Requester, RetryReason, Scope, SignPurpose,
    },
    read_frame, write_frame,
};
use slint::{ComponentHandle, Global};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{
    ActionKind, ApprovalScope, ApproveWindow, PurposeKind, Theme, UnlockError, UnlockMode,
    UnlockWindow, appearance,
};

thread_local! {
    /// The open unlock window, for events posted from the I/O thread (they run on this
    /// thread).
    static WINDOW: RefCell<Option<UnlockWindow>> = const { RefCell::new(None) };
}

/// What the I/O thread tells the UI.
enum Event {
    Retry(RetryReason),
    Waiting(Vec<Requester>),
    /// The daemon closed the pipe: the prompt is over (answered, timed out or abandoned).
    Closed,
}

pub fn run(started: Instant) -> ExitCode {
    if std::io::stdin().is_terminal() {
        eprintln!("nibwarden-gui prompt is started by nibwardend, not by hand");
        return ExitCode::from(2);
    }
    let (answers, answer_rx) = mpsc::unbounded_channel::<Answer>();
    let (first_tx, first_rx) = std_mpsc::channel::<Prompt>();
    let (flushed_tx, flushed_rx) = std_mpsc::channel::<()>();
    std::thread::spawn(move || io_thread(first_tx, answer_rx, flushed_tx));

    let Ok(first) = first_rx.recv() else {
        eprintln!("nibwarden-gui: no prompt from the daemon");
        return ExitCode::FAILURE;
    };
    let appearance = nibwarden_config::load().appearance;
    let result = match first {
        Prompt::Unlock {
            email,
            waiting,
            access,
            application,
            scope,
        } => unlock_window(&appearance, started, answers, |ui| {
            show_unlock(ui, &email, &waiting);
            match &access {
                Some(access) => {
                    ui.set_mode(UnlockMode::UnlockAllow);
                    show_access_unlock(ui, access);
                }
                None => ui.set_mode(UnlockMode::Unlock),
            }
            ui.set_scope(approval_scope(scope));
            ui.set_app(app_name(application.as_ref()).into());
        }),
        Prompt::Reprompt { email, access } => unlock_window(&appearance, started, answers, |ui| {
            ui.set_mode(UnlockMode::Reprompt);
            show_unlock(ui, &email, &[]);
            show_access_unlock(ui, &access);
        }),
        Prompt::Approve {
            access,
            application,
            scope,
        } => approve_window(&appearance, &access, application.as_ref(), scope, answers),
        Prompt::Retry { .. } | Prompt::Waiting { .. } => {
            eprintln!("nibwarden-gui: unexpected first prompt");
            return ExitCode::FAILURE;
        }
    };
    // Let a final answer (e.g. a cancel) reach the daemon before the process ends.
    let _ = flushed_rx.recv_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Theme and language from the user's settings.
pub fn apply_appearance<C>(ui: &C, appearance: &nibwarden_config::Appearance)
where
    C: ComponentHandle,
    for<'a> Theme<'a>: Global<'a, C>,
{
    appearance::select_language(appearance);
    ui.global::<Theme>()
        .set_mode(appearance::theme_mode(appearance));
}

/// Fills the window's display properties from a prompt.
pub fn show_unlock(ui: &UnlockWindow, email: &str, waiting: &[Requester]) {
    ui.set_email(email.into());
    let initial: String = email
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    ui.set_initial(initial.into());
    show_waiting(ui, waiting);
}

/// The first waiting program in full, and how many more there are.
pub fn show_waiting(ui: &UnlockWindow, waiting: &[Requester]) {
    if let Some(first) = waiting.first() {
        ui.set_requester(first.name.as_str().into());
        ui.set_requester_detail(detail(first).into());
    }
    ui.set_more_waiting(waiting.len().saturating_sub(1) as i32);
}

fn detail(requester: &Requester) -> String {
    match (&requester.path, requester.pid) {
        (Some(path), Some(pid)) => format!("{path} · pid {pid}"),
        (Some(path), None) => path.clone(),
        (None, Some(pid)) => format!("pid {pid}"),
        (None, None) => String::new(),
    }
}

/// The access card's properties for a request.
struct AccessFields {
    action: ActionKind,
    target: String,
    field: String,
    purpose: PurposeKind,
    purpose_arg: String,
}

fn access_fields(access: &Access) -> AccessFields {
    match &access.action {
        Action::ReadSecret { item, field } => AccessFields {
            action: ActionKind::ReadSecret,
            target: item.clone(),
            field: field.to_lowercase(),
            purpose: PurposeKind::Unknown,
            purpose_arg: String::new(),
        },
        Action::Send { send, what } => AccessFields {
            action: ActionKind::Send,
            target: send.clone(),
            field: what.as_str().to_owned(),
            purpose: PurposeKind::Unknown,
            purpose_arg: String::new(),
        },
        Action::Export { format } => AccessFields {
            action: ActionKind::Export,
            target: String::new(),
            field: format.clone(),
            purpose: PurposeKind::Unknown,
            purpose_arg: String::new(),
        },
        Action::Import { source, items } => AccessFields {
            action: ActionKind::Import,
            target: source.clone(),
            field: String::new(),
            purpose: PurposeKind::Unknown,
            purpose_arg: items.map(|n| n.to_string()).unwrap_or_default(),
        },
        Action::ChangeItem { item, change } => AccessFields {
            action: ActionKind::ChangeItem,
            target: item.clone(),
            field: change.as_str().to_owned(),
            purpose: PurposeKind::Unknown,
            purpose_arg: String::new(),
        },
        Action::Browser { what } => AccessFields {
            action: ActionKind::Browser,
            target: String::new(),
            field: match what {
                BrowserUse::Unlock => "unlock",
                BrowserUse::Verify => "verify",
            }
            .to_owned(),
            purpose: PurposeKind::Unknown,
            purpose_arg: String::new(),
        },
        Action::SshSign { key, purpose } => {
            let (purpose, purpose_arg) = match purpose {
                SignPurpose::Login { user } => (PurposeKind::Login, user.clone()),
                SignPurpose::Git => (PurposeKind::Git, String::new()),
                SignPurpose::File { namespace } => (PurposeKind::File, namespace.clone()),
                SignPurpose::Unknown => (PurposeKind::Unknown, String::new()),
            };
            AccessFields {
                action: ActionKind::SshSign,
                target: key.clone(),
                field: String::new(),
                purpose,
                purpose_arg,
            }
        }
    }
}

pub fn show_access_unlock(ui: &UnlockWindow, access: &Access) {
    let f = access_fields(access);
    ui.set_requester(access.requester.name.as_str().into());
    ui.set_requester_detail(detail(&access.requester).into());
    ui.set_action(f.action);
    ui.set_target(f.target.into());
    ui.set_field_name(f.field.into());
    ui.set_purpose(f.purpose);
    ui.set_purpose_arg(f.purpose_arg.into());
}

pub fn approval_scope(scope: Scope) -> ApprovalScope {
    match scope {
        Scope::Request => ApprovalScope::Request,
        Scope::Application => ApprovalScope::Application,
        Scope::Terminal => ApprovalScope::Terminal,
    }
}

fn app_name(application: Option<&Requester>) -> String {
    application.map(|a| a.name.clone()).unwrap_or_default()
}

pub fn show_access_approve(
    ui: &ApproveWindow,
    access: &Access,
    application: Option<&Requester>,
    scope: Scope,
) {
    let f = access_fields(access);
    ui.set_requester(access.requester.name.as_str().into());
    ui.set_requester_detail(detail(&access.requester).into());
    ui.set_action(f.action);
    ui.set_target(f.target.into());
    ui.set_field_name(f.field.into());
    ui.set_purpose(f.purpose);
    ui.set_purpose_arg(f.purpose_arg.into());
    ui.set_scope(approval_scope(scope));
    ui.set_app(app_name(application).into());
}

/// The unlock window in any mode; `setup` fills it from the prompt.
fn unlock_window(
    appearance: &nibwarden_config::Appearance,
    started: Instant,
    answers: mpsc::UnboundedSender<Answer>,
    setup: impl FnOnce(&UnlockWindow),
) -> Result<(), slint::PlatformError> {
    let ui = UnlockWindow::new()?;
    apply_appearance(&ui, appearance);
    setup(&ui);

    let weak = ui.as_weak();
    let tx = answers.clone();
    ui.on_submit(move || {
        let Some(ui) = weak.upgrade() else { return };
        let password = Zeroizing::new(ui.get_password().to_string());
        if password.is_empty() || ui.get_busy() {
            return;
        }
        ui.set_password("".into());
        ui.set_error(UnlockError::None);
        ui.set_busy(true);
        let all_applications =
            ui.get_mode() == UnlockMode::UnlockAllow && ui.get_all_applications();
        let _ = tx.send(Answer::Password {
            password,
            all_applications,
        });
    });
    let tx = answers.clone();
    ui.on_cancel(move || {
        let _ = tx.send(Answer::Cancel);
        let _ = slint::quit_event_loop();
    });
    let tx = answers;
    ui.window().on_close_requested(move || {
        let _ = tx.send(Answer::Cancel);
        slint::CloseRequestResponse::HideWindow
    });
    if std::env::var_os("NIBWARDEN_GUI_TIMING").is_some() {
        report_first_frame(&ui, started);
    }

    WINDOW.with(|w| *w.borrow_mut() = Some(ui.clone_strong()));
    // Follow theme and language changes made in the settings window.
    let _follow = {
        let weak = ui.as_weak();
        crate::appearance::follow_settings(move |appearance| {
            if let Some(ui) = weak.upgrade() {
                apply_appearance(&ui, appearance);
            }
        })
    };
    let result = ui.run();
    // Drops the callbacks and with them the last answer senders, so the writer finishes.
    WINDOW.with(|w| w.borrow_mut().take());
    drop(ui);
    result
}

/// The confirmation window. Answering ends it right away; the daemon needs nothing more.
fn approve_window(
    appearance: &nibwarden_config::Appearance,
    access: &Access,
    application: Option<&Requester>,
    scope: Scope,
    answers: mpsc::UnboundedSender<Answer>,
) -> Result<(), slint::PlatformError> {
    let ui = ApproveWindow::new()?;
    apply_appearance(&ui, appearance);
    show_access_approve(&ui, access, application, scope);

    let weak = ui.as_weak();
    let tx = answers.clone();
    ui.on_allow(move || {
        let all_applications = weak.upgrade().is_some_and(|ui| ui.get_all_applications());
        let _ = tx.send(Answer::Allow { all_applications });
        let _ = slint::quit_event_loop();
    });
    let tx = answers.clone();
    ui.on_deny(move || {
        let _ = tx.send(Answer::Cancel);
        let _ = slint::quit_event_loop();
    });
    let tx = answers;
    ui.window().on_close_requested(move || {
        let _ = tx.send(Answer::Cancel);
        slint::CloseRequestResponse::HideWindow
    });
    // Follow theme and language changes made in the settings window.
    let _follow = {
        let weak = ui.as_weak();
        crate::appearance::follow_settings(move |appearance| {
            if let Some(ui) = weak.upgrade() {
                apply_appearance(&ui, appearance);
            }
        })
    };
    let result = ui.run();
    drop(ui);
    result
}

/// Prints the time from process start to the first rendered frame (phase2.md Q1). The
/// software renderer has no rendering notifier; there the first event loop turn after
/// the window was shown is the closest mark.
fn report_first_frame(ui: &UnlockWindow, started: Instant) {
    let mut reported = false;
    let notifier = ui.window().set_rendering_notifier(move |state, _| {
        if !reported && matches!(state, slint::RenderingState::AfterRendering) {
            reported = true;
            eprintln!("nibwarden-gui: first frame after {:?}", started.elapsed());
        }
    });
    if notifier.is_err() {
        slint::Timer::single_shot(Duration::ZERO, move || {
            eprintln!("nibwarden-gui: window shown after {:?}", started.elapsed());
        });
    }
}

fn on_event(event: Event) {
    if let Event::Closed = event {
        let _ = slint::quit_event_loop();
        return;
    }
    WINDOW.with(|w| {
        let window = w.borrow();
        let Some(ui) = window.as_ref() else { return };
        match event {
            Event::Retry(reason) => {
                ui.set_error(match reason {
                    RetryReason::WrongPassword => UnlockError::WrongPassword,
                });
                ui.set_busy(false);
            }
            Event::Waiting(waiting) => show_waiting(ui, &waiting),
            Event::Closed => {}
        }
    });
}

fn post(event: Event) {
    // Fails only once the event loop is gone, when nobody is left to tell.
    let _ = slint::invoke_from_event_loop(move || on_event(event));
}

fn io_thread(
    first: std_mpsc::Sender<Prompt>,
    mut answers: mpsc::UnboundedReceiver<Answer>,
    flushed: std_mpsc::Sender<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("nibwarden-gui: {e}");
            return;
        }
    };
    runtime.block_on(async move {
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        match read_frame::<Prompt>(&mut stdin).await {
            Ok(prompt) => {
                let _ = first.send(prompt);
            }
            Err(e) => {
                eprintln!("nibwarden-gui: reading the prompt: {e}");
                return;
            }
        }
        drop(first);
        // Two independent loops; neither is cancelled mid-frame.
        let reader = async {
            loop {
                match read_frame::<Prompt>(&mut stdin).await {
                    Ok(Prompt::Retry { reason }) => post(Event::Retry(reason)),
                    Ok(Prompt::Waiting { waiting }) => post(Event::Waiting(waiting)),
                    Ok(_) => eprintln!("nibwarden-gui: ignoring a second prompt"),
                    Err(_) => {
                        post(Event::Closed);
                        break;
                    }
                }
            }
        };
        let writer = async {
            while let Some(answer) = answers.recv().await {
                if write_frame(&mut stdout, &answer).await.is_err() {
                    post(Event::Closed);
                    break;
                }
            }
            let _ = flushed.send(());
        };
        tokio::join!(reader, writer);
    });
}
