//! What nibwarden writes into other programs' settings, from the CLI and the settings
//! window alike: ssh's agent (`~/.ssh/config`) and git's commit signing (the global or a
//! repository's git config).

use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Command,
};

fn home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

/// ssh's agent: a `Host *` block in `~/.ssh/config` naming nibwarden's agent socket, as
/// 1Password does. It goes at the end, so hosts the user set up with another agent keep it.
pub mod ssh {
    use super::*;

    const BEGIN: &str =
        "# Added by nibwarden: use its SSH agent (nibwarden settings, SSH and Git).";
    const END: &str = "# End of nibwarden's settings.";

    pub fn config_path() -> io::Result<PathBuf> {
        Ok(home()?.join(".ssh/config"))
    }

    fn read(path: &Path) -> io::Result<String> {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            other => other,
        }
    }

    /// The config without nibwarden's block, and whether it had one.
    fn without_block(text: &str) -> (String, bool) {
        let mut kept = Vec::new();
        let mut inside = false;
        let mut found = false;
        for line in text.lines() {
            if line.trim() == BEGIN {
                inside = true;
                found = true;
            } else if inside && line.trim() == END {
                inside = false;
            } else if !inside {
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
        (out, found)
    }

    /// Whether ssh is set up to use nibwarden's agent.
    pub fn uses_nibwarden() -> bool {
        config_path()
            .and_then(|p| read(&p))
            .is_ok_and(|text| without_block(&text).1)
    }

    /// An `IdentityAgent` the user set themselves, which takes precedence where it applies.
    pub fn other_agent() -> Option<String> {
        let text = read(&config_path().ok()?).ok()?;
        without_block(&text).0.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            let key = words.next()?;
            key.eq_ignore_ascii_case("IdentityAgent")
                .then(|| words.collect::<Vec<_>>().join(" "))
        })
    }

    /// Points ssh at the agent `socket`, or with `None` removes nibwarden's block.
    pub fn set_agent(socket: Option<&Path>) -> io::Result<()> {
        let path = config_path()?;
        let (mut text, found) = without_block(&read(&path)?);
        if socket.is_none() && !found {
            return Ok(());
        }
        if let Some(socket) = socket {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "{BEGIN}\nHost *\n    IdentityAgent \"{}\"\n{END}\n",
                socket.display()
            ));
        }
        let dir = path.parent().expect("~/.ssh/config has a parent");
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            set_mode(dir, 0o700)?;
        }
        let new = !path.exists();
        std::fs::write(&path, text)?;
        // ssh refuses a config that others can write.
        if new {
            set_mode(&path, 0o600)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    #[cfg(not(unix))]
    fn set_mode(_: &Path, _: u32) -> io::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn adds_and_removes_its_block() {
            let user = "Host work\n    IdentityAgent ~/.other.sock\n";
            let (text, found) = without_block(user);
            assert_eq!((text.as_str(), found), (user, false));
            let with =
                format!("{user}\n{BEGIN}\nHost *\n    IdentityAgent \"/run/a.sock\"\n{END}\n");
            let (text, found) = without_block(&with);
            assert_eq!((text.as_str(), found), (user, true));
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

    /// The name of the vault key the global git config signs commits with, when it is
    /// nibwarden's setup and signing is on.
    pub fn signing_key() -> Option<String> {
        let program = get(Scope::Global, "gpg.ssh.program")?;
        let key = get(Scope::Global, "user.signingkey")?;
        let on = get(Scope::Global, "commit.gpgsign").as_deref() == Some("true");
        let ours = Path::new(&program).file_name() == Some("nibwarden-ssh-sign".as_ref());
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
        for (key, value) in &settings {
            git(&["config", scope.flag(), key, value])?;
        }
        Ok(Signing { trusted, settings })
    }

    /// Stops the global git config signing commits, if it does so through nibwarden.
    /// The allowed signers file stays, so earlier commits still verify.
    pub fn unset_signing() -> io::Result<()> {
        if signing_key().is_none() {
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
