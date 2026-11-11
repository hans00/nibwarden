//! What nibwarden writes into other programs' settings, from the CLI and the settings
//! window alike: ssh's agent (`~/.ssh/config`) and git's commit signing (the global or a
//! repository's git config).

use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Command,
};

fn home() -> io::Result<PathBuf> {
    // Windows' ssh and git look in the profile directory; a HOME that a shell (Git Bash)
    // sets may point elsewhere.
    if cfg!(windows) {
        return std::env::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

/// The directory of Windows' own OpenSSH (`ssh.exe`, `ssh-keygen.exe`), the programs that
/// speak to an agent through a named pipe. Git for Windows ships ssh programs built for
/// MSYS, which do not.
#[cfg(windows)]
pub fn windows_openssh(program: &str) -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir"))?;
    let path = PathBuf::from(root)
        .join("System32")
        .join("OpenSSH")
        .join(format!("{program}.exe"));
    path.is_file().then_some(path)
}

/// The OpenSSH program `program` to run: Windows' own where there is one.
pub fn openssh(program: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    if let Some(path) = windows_openssh(program) {
        return path.into_os_string();
    }
    program.into()
}

/// A setting not made because another one would take precedence over it; the error
/// inside the `io::Error` that [`ssh::set_agent`] and [`git::set_signing`] return then.
#[derive(Debug, Clone)]
pub struct Overridden {
    /// The setting, e.g. `IdentityAgent` or `gpg.format`.
    pub setting: String,
    /// The value that is used instead.
    pub value: String,
    /// The file it comes from, when known.
    pub file: Option<String>,
}

impl std::fmt::Display for Overridden {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} would still be used", self.setting, self.value)?;
        if let Some(file) = &self.file {
            write!(f, " (from {file})")?;
        }
        write!(f, "; nothing was changed")
    }
}

impl std::error::Error for Overridden {}

impl Overridden {
    /// The `Overridden` inside `error`, if that is what it is.
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }
}

/// ssh's agent: a `Host *` block in `~/.ssh/config` naming nibwarden's agent socket, as
/// 1Password does. ssh takes the first value it finds, so the block goes before the first
/// agent the user set for every host (keeping the agents of hosts set up above it), or at
/// the end when there is none. ssh itself (`ssh -G`) then confirms the block takes effect;
/// if it does not, the file is left as it was.
pub mod ssh {
    use super::*;

    const BEGIN: &str =
        "# Added by nibwarden: use its SSH agent (nibwarden settings, SSH and Git).";
    const END: &str = "# End of nibwarden's settings.";
    /// A host no one sets up specially, to ask ssh which agent applies by default.
    const ANY_HOST: &str = "nibwarden-check.invalid";

    /// Whether ssh uses nibwarden's agent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Agent {
        /// No nibwarden block.
        Off,
        On,
        /// nibwarden's block is there, but ssh uses this agent instead (the file was
        /// changed since).
        Overridden(String),
    }

    pub fn config_path() -> io::Result<PathBuf> {
        Ok(home()?.join(".ssh/config"))
    }

    fn read(path: &Path) -> io::Result<String> {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            other => other,
        }
    }

    /// How the socket is written in the file. ssh reads backslashes in a quoted value as
    /// escapes in some versions (OpenSSH 8.7 and later, whose Windows port follows), so a
    /// named pipe (`\\.\pipe\name`) may have to be written with its backslashes doubled;
    /// [`set_agent`] asks ssh which spelling it understands.
    fn spelled(socket: &Path, escaped: bool) -> String {
        let text = socket.display().to_string();
        if escaped {
            text.replace('\\', "\\\\")
        } else {
            text
        }
    }

    fn block(socket: &Path, escaped: bool) -> String {
        format!(
            "{BEGIN}\nHost *\n    IdentityAgent \"{}\"\n{END}\n",
            spelled(socket, escaped)
        )
    }

    /// The socket as ssh reads `written` back: with doubled backslashes undone.
    fn unescaped(written: &str) -> String {
        written.replace("\\\\", "\\")
    }

    /// The config without nibwarden's block, and the socket the block named, if any.
    fn without_block(text: &str) -> (String, Option<String>) {
        let mut kept = Vec::new();
        let mut inside = false;
        let mut socket = None;
        for line in text.lines() {
            if line.trim() == BEGIN {
                inside = true;
                socket = Some(String::new());
            } else if inside && line.trim() == END {
                inside = false;
            } else if inside {
                if let Some(value) = directive(line, "IdentityAgent") {
                    socket = Some(value.trim_matches('"').to_owned());
                }
            } else {
                kept.push(line);
            }
        }
        while kept.last().is_some_and(|l| l.trim().is_empty()) {
            kept.pop();
        }
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        (out, socket)
    }

    /// The value of `keyword` on `line` (`Keyword value` or `Keyword=value`).
    fn directive<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
        let line = line.trim_start();
        let (key, value) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
        key.eq_ignore_ascii_case(keyword).then(|| {
            value
                .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                .trim()
        })
    }

    /// Whether `line` starts a block that applies to every host (`Host *`, `Match all`).
    fn global_header(line: &str) -> Option<bool> {
        if let Some(patterns) = directive(line, "Host") {
            return Some(patterns.split_whitespace().any(|p| p == "*"));
        }
        directive(line, "Match").map(|criteria| criteria.eq_ignore_ascii_case("all"))
    }

    /// Where nibwarden's block goes in `text` (without it): the line of the first block for
    /// every host that sets an agent, or `None` for the end.
    fn insert_at(text: &str) -> Option<usize> {
        let mut header: Option<(usize, bool)> = None;
        for (i, line) in text.lines().enumerate() {
            if let Some(global) = global_header(line) {
                header = Some((i, global));
            } else if directive(line, "IdentityAgent").is_some() {
                match header {
                    // Before any Host line, a directive applies to every host.
                    None => return Some(0),
                    Some((at, true)) => return Some(at),
                    Some((_, false)) => {}
                }
            }
        }
        None
    }

    /// `text` with nibwarden's block at line `at` (`None`: at the end).
    fn with_block(text: &str, socket: &Path, at: Option<usize>, escaped: bool) -> String {
        let block = block(socket, escaped);
        match at {
            Some(at) => {
                let mut lines: Vec<&str> = text.lines().collect();
                lines.insert(at, block.trim_end());
                lines.join("\n") + "\n"
            }
            None if text.is_empty() => block,
            None => format!("{text}\n{block}"),
        }
    }

    /// The agent ssh uses for `host` with the config at `path`, as ssh reports it; `None`
    /// if ssh cannot be asked.
    fn effective(path: &Path, host: &str) -> Option<String> {
        let out = Command::new(openssh("ssh"))
            .arg("-G")
            .arg("-F")
            .arg(path)
            .arg(host)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("identityagent "))
                .unwrap_or("none")
                .to_owned(),
        )
    }

    /// Whether ssh uses nibwarden's agent, by asking ssh.
    pub fn agent() -> Agent {
        let Ok(path) = config_path() else {
            return Agent::Off;
        };
        let Some(socket) = read(&path).ok().and_then(|text| without_block(&text).1) else {
            return Agent::Off;
        };
        match effective(&path, ANY_HOST) {
            Some(used) if used != socket && used != unescaped(&socket) => Agent::Overridden(used),
            _ => Agent::On,
        }
    }

    /// An `IdentityAgent` the user set for particular hosts, which they keep.
    pub fn other_agent() -> Option<String> {
        let text = read(&config_path().ok()?).ok()?;
        let mut global = true;
        without_block(&text).0.lines().find_map(|line| {
            if let Some(g) = global_header(line) {
                global = g;
                return None;
            }
            directive(line, "IdentityAgent")
                .filter(|_| !global)
                .map(str::to_owned)
        })
    }

    /// Points ssh at the agent `socket`, or with `None` removes nibwarden's block. A block
    /// that ssh would not use is not left behind: the file is restored and the reason
    /// returned.
    pub fn set_agent(socket: Option<&Path>) -> io::Result<()> {
        let path = config_path()?;
        let existed = path.exists();
        let original = read(&path)?;
        let (text, had) = without_block(&original);
        let Some(socket) = socket else {
            restore_git_ssh_command();
            return if had.is_some() {
                write(&path, &text)
            } else {
                Ok(())
            };
        };
        let wanted = socket.display().to_string();
        // Before the user's agent for every host; failing that (e.g. one set in an
        // Included file), first of all.
        // Only a Windows pipe name has backslashes to be read wrongly.
        let spellings: &[bool] = if wanted.contains('\\') {
            &[false, true]
        } else {
            &[false]
        };
        for at in [insert_at(&text), Some(0)] {
            for &escaped in spellings {
                write(&path, &with_block(&text, socket, at, escaped))?;
                match effective(&path, ANY_HOST) {
                    Some(used) if used != wanted => {}
                    _ => {
                        use_windows_openssh_in_git();
                        return Ok(());
                    }
                }
            }
        }
        let used = effective(&path, ANY_HOST).unwrap_or_default();
        if !existed {
            std::fs::remove_file(&path)?;
        } else {
            write(&path, &original)?;
        }
        Err(io::Error::other(Overridden {
            setting: "IdentityAgent".into(),
            value: used,
            file: None,
        }))
    }

    fn write(path: &Path, text: &str) -> io::Result<()> {
        let dir = path.parent().expect("~/.ssh/config has a parent");
        if !dir.exists() {
            nibwarden_platform::fs::create_private_dir(dir)?;
        }
        let new = !path.exists();
        std::fs::write(path, text)?;
        // ssh refuses a config that others can write (Windows' ssh: one whose access
        // control list names anyone but the owner, SYSTEM and the administrators).
        if new {
            nibwarden_platform::fs::restrict_file(path)?;
        }
        Ok(())
    }

    /// `core.sshCommand` of the global git config, set by [`use_windows_openssh_in_git`].
    #[cfg(windows)]
    const GIT_SSH_COMMAND: &str = "core.sshCommand";

    /// Git for Windows runs its own MSYS `ssh` for `git push` and `git pull` over SSH,
    /// which cannot reach an agent through a named pipe, so the `IdentityAgent` set above
    /// would be of no use to it. Points git at Windows' OpenSSH instead, as 1Password's
    /// setup guide does, unless the user set a command of their own. Best effort: git may
    /// not be installed.
    fn use_windows_openssh_in_git() {
        #[cfg(windows)]
        if let Some(ssh) = windows_openssh("ssh")
            && super::git::get_global(GIT_SSH_COMMAND).is_none()
        {
            // Forward slashes: git reads backslashes in config values as escapes.
            let command = ssh.to_string_lossy().replace('\\', "/");
            let _ = super::git::set_global(GIT_SSH_COMMAND, &command);
        }
    }

    /// Takes back [`use_windows_openssh_in_git`], if the command is the one it set.
    fn restore_git_ssh_command() {
        #[cfg(windows)]
        if let Some(ssh) = windows_openssh("ssh")
            && super::git::get_global(GIT_SSH_COMMAND).as_deref()
                == Some(ssh.to_string_lossy().replace('\\', "/").as_str())
        {
            let _ = super::git::unset_global(GIT_SSH_COMMAND);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn named_pipes_are_written_with_or_without_doubled_backslashes() {
            let pipe = Path::new(r"\\.\pipe\nibwarden-S-1-5-21-1-ssh-agent");
            assert_eq!(
                spelled(pipe, false),
                r"\\.\pipe\nibwarden-S-1-5-21-1-ssh-agent"
            );
            let escaped = spelled(pipe, true);
            assert_eq!(escaped, r"\\\\.\\pipe\\nibwarden-S-1-5-21-1-ssh-agent");
            assert_eq!(unescaped(&escaped), pipe.to_str().unwrap());
            // A Unix socket has no backslashes to change.
            let socket = Path::new("/run/user/1000/nibwarden/ssh-agent.sock");
            assert_eq!(spelled(socket, true), socket.to_str().unwrap());
            assert!(block(pipe, true).contains(&format!("IdentityAgent \"{escaped}\"")));
        }

        #[test]
        fn adds_and_removes_its_block() {
            let user = "Host work\n    IdentityAgent ~/.other.sock\n";
            let (text, found) = without_block(user);
            assert_eq!((text.as_str(), found), (user, None));
            let with = with_block(user, Path::new("/run/a.sock"), insert_at(user), false);
            assert!(with.ends_with(&block(Path::new("/run/a.sock"), false)));
            let (text, found) = without_block(&with);
            assert_eq!(
                (text.as_str(), found.as_deref()),
                (user, Some("/run/a.sock"))
            );
        }

        #[test]
        fn goes_before_the_users_agent_for_every_host() {
            let user = "Host work\n  IdentityAgent /w.sock\nHost *\n  IdentityAgent /all.sock\n";
            assert_eq!(insert_at(user), Some(2));
            let with = with_block(user, Path::new("/n.sock"), insert_at(user), false);
            assert!(with.starts_with(&format!("Host work\n  IdentityAgent /w.sock\n{BEGIN}")));
            assert_eq!(without_block(&with).0, user);
            // Before any Host line, and Match all, count as every host.
            assert_eq!(insert_at("IdentityAgent /a.sock\n"), Some(0));
            assert_eq!(
                insert_at("Host a\nMatch all\n  IdentityAgent=/a\n"),
                Some(1)
            );
            assert_eq!(insert_at("Host a b\n  IdentityAgent /a\n"), None);
        }
    }
}

/// git's commit signing with an SSH key from the vault, through `nibwarden-ssh-sign`.
pub mod git {
    use super::*;

    /// Which git config to write.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        Global,
        /// The repository of the current directory.
        Local,
    }

    impl Scope {
        fn flag(self) -> &'static str {
            match self {
                Self::Global => "--global",
                Self::Local => "--local",
            }
        }
    }

    /// What [`set_signing`] did.
    pub struct Signing {
        /// Where the key was trusted for the user's address, and whether it was new there;
        /// `None` when git has no user.email to trust it for.
        pub trusted: Option<Trusted>,
        /// The git settings written.
        pub settings: Vec<(&'static str, String)>,
    }

    pub struct Trusted {
        pub email: String,
        /// The allowed signers file.
        pub signers: PathBuf,
        pub added: bool,
    }

    /// Prefix of `user.signingkey` that marks nibwarden's setup; the key's name follows.
    const MARK: &str = " nibwarden:";

    fn git(args: &[&str]) -> io::Result<String> {
        let out = Command::new("git")
            .args(args)
            .output()
            .map_err(|e| io::Error::new(e.kind(), format!("cannot run git: {e}")))?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    fn get(scope: Scope, name: &str) -> Option<String> {
        git(&["config", scope.flag(), name])
            .ok()
            .filter(|v| !v.is_empty())
    }

    #[cfg(windows)]
    pub(super) fn get_global(name: &str) -> Option<String> {
        get(Scope::Global, name)
    }

    #[cfg(windows)]
    pub(super) fn set_global(name: &str, value: &str) -> io::Result<()> {
        git(&["config", "--global", name, value]).map(drop)
    }

    #[cfg(windows)]
    pub(super) fn unset_global(name: &str) -> io::Result<()> {
        git(&["config", "--global", "--unset", name]).map(drop)
    }

    /// The value git uses outside any repository, every config file and include counted.
    fn effective(name: &str) -> Option<String> {
        outside_repository(&["config", "--get", name])
    }

    /// Where [`effective`]'s value comes from: `file:<path>\t<value>`.
    fn effective_origin(name: &str) -> Option<String> {
        outside_repository(&["config", "--show-origin", "--get", name])
    }

    fn outside_repository(args: &[&str]) -> Option<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(std::env::temp_dir())
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// The name of the vault key the global git config signs commits with, when it is
    /// nibwarden's setup and signing is on.
    pub fn signing_key() -> Option<String> {
        let name = ours()?;
        // In effect, not overridden by another file git reads.
        [
            "gpg.format",
            "gpg.ssh.program",
            "user.signingkey",
            "commit.gpgsign",
        ]
        .iter()
        .all(|key| effective(key) == get(Scope::Global, key))
        .then_some(name)
    }

    /// The vault key name, when the global git config holds nibwarden's signing setup.
    fn ours() -> Option<String> {
        let program = get(Scope::Global, "gpg.ssh.program")?;
        let key = get(Scope::Global, "user.signingkey")?;
        let on = get(Scope::Global, "commit.gpgsign").as_deref() == Some("true");
        // `nibwarden-ssh-sign`, or `nibwarden-ssh-sign.exe`.
        let ours = Path::new(&program).file_stem() == Some("nibwarden-ssh-sign".as_ref());
        (on && ours && get(Scope::Global, "gpg.format").as_deref() == Some("ssh"))
            .then(|| key.split_once(MARK).map(|(_, name)| name.to_owned()))
            .flatten()
    }

    /// Makes git sign commits with the vault key `name` (`public_key` in authorized_keys
    /// format) through `helper`, as 1Password's setup does. When git knows the user's
    /// address, the key is also trusted for it in the allowed signers file, so
    /// `git log --show-signature` verifies the commits locally; signing does not need that.
    pub fn set_signing(
        scope: Scope,
        name: &str,
        public_key: &str,
        helper: &Path,
    ) -> io::Result<Signing> {
        if scope == Scope::Local {
            git(&["rev-parse", "--git-dir"])
                .map_err(|_| io::Error::other("not in a git repository"))?;
        }
        // "type base64" without the comment, as allowed_signers and key:: expect.
        let public = public_key
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        let mut settings = vec![
            ("gpg.format", "ssh".to_owned()),
            ("user.signingkey", format!("key::{public}{MARK}{name}")),
            ("gpg.ssh.program", helper.to_string_lossy().into_owned()),
            ("commit.gpgsign", "true".to_owned()),
        ];
        let email = git(&["config", "user.email"])
            .ok()
            .filter(|e| !e.is_empty());
        let trusted = match email {
            Some(email) => {
                let configured = git(&["config", "--path", "gpg.ssh.allowedSignersFile"])
                    .ok()
                    .filter(|p| !p.is_empty());
                let signers = match &configured {
                    Some(path) => PathBuf::from(path),
                    None => default_allowed_signers()?,
                };
                let added = add_allowed_signer(&signers, &email, &public).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("cannot update {}: {e}", signers.display()),
                    )
                })?;
                if configured.is_none() {
                    settings.push((
                        "gpg.ssh.allowedSignersFile",
                        signers.to_string_lossy().into_owned(),
                    ));
                }
                Some(Trusted {
                    email,
                    signers,
                    added,
                })
            }
            None => None,
        };
        let before: Vec<Option<String>> = settings.iter().map(|(key, _)| get(scope, key)).collect();
        for (key, value) in &settings {
            git(&["config", scope.flag(), key, value])?;
        }
        // Another file git reads (an [include]d one, or the repository's) may set these too;
        // a setup git would not use is not left behind.
        if scope == Scope::Global
            && let Some((key, _)) = settings
                .iter()
                .find(|(key, value)| effective(key).as_deref() != Some(value.as_str()))
        {
            // "file:<path>\t<value>"
            let origin = effective_origin(key).unwrap_or_default();
            let (file, value) = origin.split_once('\t').unwrap_or(("", &origin));
            let file = file.strip_prefix("file:").unwrap_or(file).to_owned();
            let value = value.to_owned();
            for ((key, _), old) in settings.iter().zip(before) {
                match old {
                    Some(old) => git(&["config", "--global", key, &old]).map(drop)?,
                    None => git(&["config", "--global", "--unset", key]).map(drop)?,
                };
            }
            return Err(io::Error::other(Overridden {
                setting: (*key).to_owned(),
                value,
                file: (!file.is_empty()).then_some(file),
            }));
        }
        Ok(Signing { trusted, settings })
    }

    /// Stops the global git config signing commits, if it does so through nibwarden.
    /// The allowed signers file stays, so earlier commits still verify.
    pub fn unset_signing() -> io::Result<()> {
        if ours().is_none() {
            return Ok(());
        }
        for key in [
            "commit.gpgsign",
            "gpg.ssh.program",
            "user.signingkey",
            "gpg.format",
        ] {
            git(&["config", "--global", "--unset", key])?;
        }
        Ok(())
    }

    fn default_allowed_signers() -> io::Result<PathBuf> {
        let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => home()?.join(".config"),
        };
        Ok(config.join("git/allowed_signers"))
    }

    /// Adds `email`'s `public` key for git signatures; `false` if it was there already.
    pub(crate) fn add_allowed_signer(path: &Path, email: &str, public: &str) -> io::Result<bool> {
        let existing = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let present = existing
            .lines()
            .any(|line| line.split_whitespace().next() == Some(email) && line.contains(public));
        if present {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        writeln!(file, "{separator}{email} namespaces=\"git\" {public}")?;
        Ok(true)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn allowed_signers_append_once() {
            let dir = std::env::temp_dir().join(format!("nw-signers-{}", std::process::id()));
            let path = dir.join("git/allowed_signers");
            let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDummy";
            assert!(add_allowed_signer(&path, "a@example.test", key).unwrap());
            assert!(!add_allowed_signer(&path, "a@example.test", key).unwrap());
            // Same key for another address, and a file without a trailing newline.
            std::fs::write(&path, std::fs::read_to_string(&path).unwrap().trim_end()).unwrap();
            assert!(add_allowed_signer(&path, "b@example.test", key).unwrap());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!(
                    "a@example.test namespaces=\"git\" {key}\nb@example.test namespaces=\"git\" {key}\n"
                )
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
