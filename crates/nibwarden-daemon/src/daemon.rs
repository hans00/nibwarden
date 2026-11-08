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
            ssh_keys_seen: Mutex::new(None),
            prompts: PromptBroker::new(config.gui.clone()),
            config,
            lock_settings: Mutex::new(settings.lock),
            approvals: Approvals::new(settings.approval),
            browser: Default::default(),
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
            return StatusResult {
                state: LockState::LoggedOut,
                email: None,
                server: None,
                last_sync: None,
                lock_timeout,
                ssh_agent_socket,
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
        Ok(())
    }

    pub async fn lock(&self) -> Result<(), ErrorBody> {
        if let Some(account) = self.account.read().await.as_ref()
            && account.is_unlocked()
        {
            account.lock().await;
            self.approvals.clear();
            tracing::info!("locked");
        }
        Ok(())
    }

    /// Where to listen for change notifications (`push_sync`).
    pub async fn push_endpoint(&self) -> Result<nibwarden_core::PushEndpoint, ErrorBody> {
        self.current().await?.push_endpoint().await.map_err(to_ipc)
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
        let ran = account.sync(force).await.map_err(to_ipc)?;
        *self.last_sync.lock().expect("poisoned") = Some(SystemTime::now());
        Ok(ran)
    }

    /// Using the unlocked vault counts as activity for the idle timer; a failed attempt
    /// (e.g. locked) does not.
    pub async fn list_items(&self, trash: bool) -> Result<Vec<ItemSummary>, ErrorBody> {
        let items = self
            .current()
            .await?
            .list_items(trash)
            .await
            .map_err(to_ipc)?;
        self.touch();
        Ok(items)
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
                if idle >= timeout
                    && let Ok(account) = self.current().await
                    && account.is_unlocked()
                {
                    account.lock().await;
                    self.approvals.clear();
                    tracing::info!(idle_secs = idle.as_secs(), "locked after inactivity");
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
