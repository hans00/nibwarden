//! Account state machine and lock controller (phase1.md §5.1).

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nibwarden_core::{
    Account, ItemDetails, ItemQuery, ItemRef, ItemSummary, Secret, Server, SshKeyInfo, Store, Totp,
    TwoFactor,
};
use nibwarden_ipc::{ErrorBody, ErrorCode, LockState, StatusResult};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use zeroize::Zeroizing;

use crate::{approvals::Approvals, error::to_ipc, prompt_broker::PromptBroker};

pub struct Config {
    /// Lock after this much inactivity, from the command line; `None` = from the settings
    /// file (`[lock] idle_minutes`).
    pub lock_timeout: Option<Option<Duration>>,
    /// Background sync period; `None` = only on demand.
    pub sync_interval: Option<Duration>,
    /// Where the SSH agent listens; `None` = agent disabled. Reported by `status`.
    pub ssh_agent_socket: Option<PathBuf>,
    /// The `nibwarden-gui` binary for unlock windows; `None` = not available.
    pub gui: Option<PathBuf>,
}

pub struct Daemon {
    store: Store,
    account: RwLock<Option<Arc<Account>>>,
    /// Serialises login and logout, so the slow network login does not hold `account`.
    auth: AsyncMutex<()>,
    /// Woken on login, so live sync connects right away.
    pub logged_in: tokio::sync::Notify,
    /// Boot-clock time of the last request that used the unlocked vault.
    last_activity: Mutex<Duration>,
    last_sync: Mutex<Option<SystemTime>>,
    /// Whose login the server ended (`Error::SessionEnded`), until the next login.
    ended: Mutex<Option<String>>,
    /// Public SSH keys as of the last time the vault was unlocked, so the agent can offer
    /// them while locked and ask for the master password only when a key is actually
    /// used (phase2.md §5.3). Public keys are not secret; kept in memory only.
    ssh_keys_seen: Mutex<Option<Vec<SshKeyInfo>>>,
    pub config: Config,
    /// `[lock]` from the settings file; reloaded with `config.reload`.
    lock_settings: Mutex<nibwarden_config::Lock>,
    pub approvals: Approvals,
    pub prompts: PromptBroker,
    pub browser: crate::browser::BrowserChannels,
    /// Website icons fetched so far, by domain (`None`: the service has none); kept until
    /// the daemon ends.
    icons: AsyncMutex<std::collections::HashMap<String, Option<Arc<Vec<u8>>>>>,
    /// Wrong PINs in a row.
    pin_failures: Mutex<u32>,
}

impl Daemon {
    /// Restores a persisted session, if any. It always comes back locked: no key material
    /// is kept on disk (phase1.md §5.1).
    pub async fn start(
        store: Store,
        config: Config,
        settings: nibwarden_config::Config,
    ) -> Result<Arc<Self>, nibwarden_core::Error> {
        let account = Account::load(store.clone()).await?.map(Arc::new);
        if let Some(account) = &account {
            tracing::info!(email = account.email(), "restored session (locked)");
        }
        Ok(Arc::new(Self {
            store,
            account: RwLock::new(account),
            auth: AsyncMutex::new(()),
            logged_in: tokio::sync::Notify::new(),
            last_activity: Mutex::new(nibwarden_platform::clock::since_boot()),
            last_sync: Mutex::new(None),
            ended: Mutex::new(None),
            ssh_keys_seen: Mutex::new(None),
            prompts: PromptBroker::new(config.gui.clone()),
            config,
            lock_settings: Mutex::new(settings.lock),
            approvals: Approvals::new(settings.approval),
            browser: Default::default(),
            icons: Default::default(),
            pin_failures: Mutex::new(0),
        }))
    }

    /// Reads the settings file again (phase2.md §5.6).
    pub fn reload_config(&self) {
        let settings = nibwarden_config::load();
        *self.lock_settings.lock().expect("poisoned") = settings.lock;
        self.approvals.set_config(settings.approval);
        tracing::info!("settings reloaded");
    }

    pub fn lock_settings(&self) -> nibwarden_config::Lock {
        *self.lock_settings.lock().expect("poisoned")
    }

    /// The idle timeout in effect: the command line's, else the settings file's.
    fn lock_timeout(&self) -> Option<Duration> {
        self.config
            .lock_timeout
            .unwrap_or_else(|| self.lock_settings().idle_timeout())
    }

    async fn current(&self) -> Result<Arc<Account>, ErrorBody> {
        self.account
            .read()
            .await
            .clone()
            .ok_or_else(|| ErrorBody::new(ErrorCode::LoggedOut, "not logged in"))
    }

    /// The icon of `domain`, from the icon service of the account's server.
    pub async fn icon(&self, domain: &str) -> Result<Option<Arc<Vec<u8>>>, ErrorBody> {
        let domain = domain.to_ascii_lowercase();
        if !nibwarden_core::valid_domain(&domain) {
            return Err(ErrorBody::new(ErrorCode::BadRequest, "not a domain"));
        }
        if let Some(known) = self.icons.lock().await.get(&domain) {
            return Ok(known.clone());
        }
        let base =
            nibwarden_core::icons_url(&self.current().await?.api_url().await.map_err(to_ipc)?);
        let icon = match nibwarden_core::fetch_icon(&base, &domain).await {
            Ok(icon) => icon.map(Arc::new),
            // Unreachable now; ask again next time.
            Err(e) => {
                tracing::debug!(domain, error = %e, "no icon");
                return Ok(None);
            }
        };
        self.icons.lock().await.insert(domain, icon.clone());
        Ok(icon)
    }

    /// The logged-in account, locked or not.
    pub async fn account(&self) -> Option<Arc<Account>> {
        self.account.read().await.clone()
    }

    fn touch(&self) {
        *self.last_activity.lock().expect("poisoned") = nibwarden_platform::clock::since_boot();
    }

    pub async fn status(&self) -> StatusResult {
        let account = self.account.read().await.clone();
        let last_sync = self
            .last_sync
            .lock()
            .expect("poisoned")
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let lock_timeout = self.lock_timeout().map(|d| d.as_secs());
        let ssh_agent_socket = self
            .config
            .ssh_agent_socket
            .as_ref()
            .map(|p| p.display().to_string());
        let Some(account) = account else {
            let ended = self.ended.lock().expect("poisoned").clone();
            return StatusResult {
                state: LockState::LoggedOut,
                session_ended: ended.is_some(),
                email: ended,
                server: None,
                last_sync: None,
                lock_timeout,
                ssh_agent_socket,
                pin_unlock: false,
                system_unlock: false,
                pin_set: false,
            };
        };
        StatusResult {
            state: if account.is_unlocked() {
                LockState::Unlocked
            } else {
                LockState::Locked
            },
            email: Some(account.email().to_owned()),
            server: account.api_url().await.ok(),
            last_sync,
            lock_timeout,
            ssh_agent_socket,
            pin_unlock: account.pin_ready(),
            system_unlock: account.key_kept() && nibwarden_platform::polkit::available().await,
            pin_set: account.has_pin(),
            session_ended: false,
        }
    }

    pub async fn login(
        &self,
        server: &Server,
        email: &str,
        password: &str,
        two_factor: Option<TwoFactor>,
    ) -> Result<StatusResult, ErrorBody> {
        let _auth = self.auth.lock().await;
        if let Some(existing) = self.account.read().await.as_ref() {
            return Err(ErrorBody::new(
                ErrorCode::AlreadyLoggedIn,
                format!("already logged in as {}; log out first", existing.email()),
            ));
        }
        let account =
            Account::login_password(self.store.clone(), server, email, password, two_factor)
                .await
                .map_err(to_ipc)?;
        tracing::info!(email, "logged in");
        *self.ended.lock().expect("poisoned") = None;
        self.logged_in.notify_waiters();
        *self.last_sync.lock().expect("poisoned") = Some(SystemTime::now());
        *self.account.write().await = Some(Arc::new(account));
        self.touch();
        Ok(self.status().await)
    }

    pub async fn logout(&self) -> Result<(), ErrorBody> {
        let _auth = self.auth.lock().await;
        let Some(account) = self.account.write().await.take() else {
            return Ok(());
        };
        *self.last_sync.lock().expect("poisoned") = None;
        *self.ssh_keys_seen.lock().expect("poisoned") = None;
        self.approvals.clear();
        self.browser.clear();
        account.logout().await.map_err(to_ipc)?;
        tracing::info!("logged out");
        Ok(())
    }

    /// Unlocks, then syncs in the background (failures there must not fail an offline
    /// unlock).
    pub async fn unlock(self: &Arc<Self>, password: &str) -> Result<(), ErrorBody> {
        let account = self.current().await?;
        account
            .unlock_with_password(password)
            .await
            .map_err(to_ipc)?;
        *self.pin_failures.lock().expect("poisoned") = 0;
        // Ready for the system's authentication from now on, if that is on.
        if nibwarden_config::load().unlock.system
            && let Err(e) = account.keep_key(true).await
        {
            tracing::warn!(error = %e, "cannot keep the key for system authentication");
        }
        self.unlocked();
        Ok(())
    }

    /// Unlocks with the PIN. Five wrong ones in a row and it stops working until the
    /// master password unlocks again, as in Bitwarden.
    pub async fn unlock_with_pin(self: &Arc<Self>, pin: &str) -> Result<(), ErrorBody> {
        const ATTEMPTS: u32 = 5;
        let account = self.current().await?;
        if !account.pin_ready() {
            return Err(ErrorBody::new(
                ErrorCode::Unavailable,
                "the PIN needs the master password first",
            ));
        }
        match account.unlock_with_pin(pin).await {
            Ok(()) => {
                *self.pin_failures.lock().expect("poisoned") = 0;
                self.unlocked();
                Ok(())
            }
            Err(nibwarden_core::Error::WrongPassword) => {
                let failures = {
                    let mut failures = self.pin_failures.lock().expect("poisoned");
                    *failures += 1;
                    *failures
                };
                if failures >= ATTEMPTS {
                    account.suspend_pin();
                    tracing::info!("PIN suspended after {ATTEMPTS} wrong ones");
                }
                Err(ErrorBody::new(ErrorCode::Denied, "wrong PIN"))
            }
            Err(e) => Err(to_ipc(e)),
        }
    }

    /// Unlocks once the system confirmed the user, asking it for the process `pid` (the
    /// window the user is in).
    pub async fn unlock_with_system(self: &Arc<Self>, pid: u32) -> Result<(), ErrorBody> {
        let account = self.current().await?;
        if !account.key_kept() {
            return Err(ErrorBody::new(
                ErrorCode::Unavailable,
                "this needs the master password first",
            ));
        }
        match nibwarden_platform::polkit::authenticate(pid).await {
            Ok(true) => {
                account.unlock_with_kept_key().await.map_err(to_ipc)?;
                self.unlocked();
                Ok(())
            }
            Ok(false) => Err(ErrorBody::new(ErrorCode::Denied, "not confirmed")),
            Err(e) => {
                tracing::warn!(error = %e, "system authentication failed");
                Err(ErrorBody::new(
                    ErrorCode::Unavailable,
                    format!("the system cannot confirm you: {e}"),
                ))
            }
        }
    }

    /// The ways to unlock besides the master password that work now: (PIN, system).
    pub async fn quick_unlock(&self) -> (bool, bool) {
        let Some(account) = self.account().await else {
            return (false, false);
        };
        let system = account.key_kept() && nibwarden_platform::polkit::available().await;
        (account.pin_ready(), system)
    }

    /// After the settings changed: keeps or forgets the key for system authentication.
    pub async fn apply_unlock_settings(&self) {
        let Some(account) = self.account().await else {
            return;
        };
        let on = nibwarden_config::load().unlock.system;
        if on == account.key_kept() || (on && !account.is_unlocked()) {
            return;
        }
        if let Err(e) = account.keep_key(on).await {
            tracing::warn!(error = %e, "cannot change system authentication");
        }
    }

    fn unlocked(self: &Arc<Self>) {
        self.touch();
        tracing::info!("unlocked");
        let daemon = self.clone();
        tokio::spawn(async move {
            if let Err(e) = daemon.sync(false).await {
                tracing::warn!(error = %e, "sync after unlock failed");
            }
            // Refresh the public keys the agent offers while locked.
            if let Err(e) = daemon.ssh_keys().await {
                tracing::debug!(error = %e, "cannot list SSH keys after unlock");
            }
        });
    }

    pub async fn lock(&self) -> Result<(), ErrorBody> {
        if let Some(account) = self.account.read().await.as_ref()
            && account.is_unlocked()
        {
            account.lock().await;
            self.approvals.locked();
            tracing::info!("locked");
        }
        Ok(())
    }

    /// Where to listen for change notifications (`push_sync`).
    pub async fn push_endpoint(&self) -> Result<nibwarden_core::PushEndpoint, ErrorBody> {
        let account = self.current().await?;
        match account.push_endpoint().await {
            Err(nibwarden_core::Error::SessionEnded) => Err(self.end_session(account).await),
            other => other.map_err(to_ipc),
        }
    }

    /// The server no longer accepts `account`'s login: logs out, as Bitwarden's clients
    /// do, and remembers whose login it was so the windows can ask to log in again.
    async fn end_session(&self, account: Arc<Account>) -> ErrorBody {
        let current = self
            .account
            .read()
            .await
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(a, &account));
        // Not a login made since.
        if current {
            tracing::warn!("the server ended the login; logging out");
            *self.ended.lock().expect("poisoned") = Some(account.email().to_owned());
            drop(account);
            if let Err(e) = self.logout().await {
                return e;
            }
        }
        to_ipc(nibwarden_core::Error::SessionEnded)
    }

    /// Checks the master password without changing the lock state (reprompt items).
    pub async fn verify_password(&self, password: &str) -> Result<bool, ErrorBody> {
        self.current()
            .await?
            .verify_password(password)
            .map_err(to_ipc)
    }

    pub async fn sync(&self, force: bool) -> Result<bool, ErrorBody> {
        let account = self.current().await?;
        let ran = match account.sync(force).await {
            Err(nibwarden_core::Error::SessionEnded) => {
                return Err(self.end_session(account).await);
            }
            other => other.map_err(to_ipc)?,
        };
        *self.last_sync.lock().expect("poisoned") = Some(SystemTime::now());
        Ok(ran)
    }

    /// Not activity for the idle timer: open windows list the items every few seconds to
    /// follow changes, which would keep the vault unlocked for as long as one is open.
    /// Opening an item, reading or copying a field is.
    pub async fn list_items(&self, trash: bool) -> Result<Vec<ItemSummary>, ErrorBody> {
        self.current()
            .await?
            .list_items(trash)
            .await
            .map_err(to_ipc)
    }

    pub async fn read_field(&self, query: ItemQuery<'_>, field: &str) -> Result<Secret, ErrorBody> {
        let value = self
            .current()
            .await?
            .read_field(query, field)
            .await
            .map_err(to_ipc)?;
        self.touch();
        Ok(value)
    }

    /// An item by query, or an item in the trash by id.
    pub async fn item_details(&self, query: ItemQuery<'_>) -> Result<ItemDetails, ErrorBody> {
        let account = self.current().await?;
        let details = match account.item_details(query).await {
            Err(nibwarden_core::Error::NotFound(_)) if query.folder.is_none() => {
                account.trashed_item_details(query.item).await
            }
            other => other,
        }
        .map_err(to_ipc)?;
        self.touch();
        Ok(details)
    }

    pub async fn totp(&self, query: ItemQuery<'_>) -> Result<(Totp, ItemRef), ErrorBody> {
        let totp = self.current().await?.totp(query).await.map_err(to_ipc)?;
        self.touch();
        Ok(totp)
    }

    /// The account for a request that uses the vault (editing it, for one); counts as
    /// vault use.
    pub async fn in_use(&self) -> Result<Arc<Account>, ErrorBody> {
        let account = self.current().await?;
        self.touch();
        Ok(account)
    }

    /// Public SSH keys. Listing keys is not vault use: ssh asks for them on every
    /// connection attempt, which must not keep the vault unlocked forever.
    pub async fn ssh_keys(&self) -> Result<Vec<SshKeyInfo>, ErrorBody> {
        let keys = self.current().await?.ssh_keys().await.map_err(to_ipc)?;
        *self.ssh_keys_seen.lock().expect("poisoned") = Some(keys.clone());
        Ok(keys)
    }

    /// The SSH keys seen at the last unlocked listing; `None` if none since the daemon
    /// started or logged in.
    pub fn ssh_keys_seen(&self) -> Option<Vec<SshKeyInfo>> {
        self.ssh_keys_seen.lock().expect("poisoned").clone()
    }

    pub async fn lock_state(&self) -> LockState {
        match self.account.read().await.as_ref() {
            None => LockState::LoggedOut,
            Some(a) if a.is_unlocked() => LockState::Unlocked,
            Some(_) => LockState::Locked,
        }
    }

    /// A private key for the agent to sign with; counts as vault use.
    pub async fn ssh_private_key(&self, id: &str) -> Result<Zeroizing<String>, ErrorBody> {
        let key = self
            .current()
            .await?
            .ssh_private_key(id)
            .await
            .map_err(to_ipc)?;
        self.touch();
        Ok(key)
    }

    /// Idle-lock and periodic sync. Runs for the daemon's lifetime.
    pub async fn run_timers(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_sync_attempt = nibwarden_platform::clock::since_boot();
        loop {
            tick.tick().await;
            let now = nibwarden_platform::clock::since_boot();

            if let Some(timeout) = self.lock_timeout() {
                let idle = now.saturating_sub(*self.last_activity.lock().expect("poisoned"));
                let logout =
                    self.lock_settings().idle_action == nibwarden_config::IdleAction::Logout;
                if idle >= timeout
                    && let Ok(account) = self.current().await
                {
                    if logout {
                        match self.logout().await {
                            Ok(()) => tracing::info!(
                                idle_secs = idle.as_secs(),
                                "logged out after inactivity"
                            ),
                            Err(e) => tracing::warn!(error = %e, "cannot log out after inactivity"),
                        }
                    } else if account.is_unlocked() {
                        account.lock().await;
                        self.approvals.locked();
                        tracing::info!(idle_secs = idle.as_secs(), "locked after inactivity");
                    }
                }
            }

            if let Some(interval) = self.config.sync_interval
                && now.saturating_sub(last_sync_attempt) >= interval
            {
                last_sync_attempt = now;
                match self.sync(false).await {
                    Ok(_) => {}
                    Err(e) if e.code == ErrorCode::LoggedOut => {}
                    Err(e) => tracing::warn!(error = %e, "periodic sync failed"),
                }
            }
        }
    }
}
