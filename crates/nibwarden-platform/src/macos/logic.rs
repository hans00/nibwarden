//! The macOS port's pure logic: parsing and formatting that needs no macOS API. It is
//! compiled on every platform so its tests run everywhere; only macOS uses it.

use std::path::Path;

/// Names of programs that mark the edge of the user's own programs in the process tree:
/// the service manager and the remote login services. A request from below them belongs
/// to the application just under the boundary.
pub(super) fn is_boundary_program(name: &str) -> bool {
    matches!(name, "launchd" | "sshd" | "sshd-session")
}

/// Programs that run a remote login (`sshd` before OpenSSH 9.8, `sshd-session` since).
pub(super) fn is_remote_login_program(name: &str) -> bool {
    matches!(name, "sshd" | "sshd-session")
}

/// Process start time as one number: microseconds since the epoch.
pub(super) fn start_micros(sec: u64, usec: u64) -> u64 {
    sec.saturating_mul(1_000_000).saturating_add(usec)
}

/// A controlling terminal's device number as an opaque id; `0` and `NODEV` (-1) mean none.
pub(super) fn terminal_id(tdev: u32) -> Option<u64> {
    (tdev != 0 && tdev != u32::MAX).then_some(u64::from(tdev))
}

/// The value of environment variable `name` in a `KERN_PROCARGS2` buffer: an `int` argc,
/// the executable path, NUL padding, argc argument strings, then the environment, all
/// NUL-terminated.
pub(super) fn procargs2_env(buf: &[u8], name: &str) -> Option<String> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let mut rest = buf.get(4..)?;
    // The executable path, then the padding after it.
    rest = &rest[rest.iter().position(|&b| b == 0)?..];
    rest = &rest[rest.iter().position(|&b| b != 0).unwrap_or(rest.len())..];
    let mut strings = rest.split(|&b| b == 0);
    for _ in 0..argc.max(0) {
        strings.next()?;
    }
    let prefix = format!("{name}=");
    strings
        .take_while(|s| !s.is_empty())
        .find_map(|s| s.strip_prefix(prefix.as_bytes()))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// `zh-Hant-TW`, `en-US`, `pt-BR`, `fr` (BCP 47, what `CFLocaleCopyPreferredLanguages`
/// returns) → `zh_TW`, `en_US`, `pt_BR`, `fr`. Without a region, a script stands in for
/// one where the translations are told apart by region (`zh-Hant` → `zh_TW`).
pub(super) fn bcp47_to_posix(tag: &str) -> Option<String> {
    let mut parts = tag.trim().split(['-', '_']);
    let language = parts.next().filter(|l| (2..=3).contains(&l.len()))?;
    if !language.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let language = language.to_ascii_lowercase();
    let (mut script, mut region) = (None, None);
    for part in parts {
        let alpha = part.bytes().all(|b| b.is_ascii_alphabetic());
        if part.len() == 4 && alpha && script.is_none() && region.is_none() {
            script = Some(part.to_ascii_lowercase());
        } else if (part.len() == 2 && alpha)
            || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
        {
            region = Some(part.to_ascii_uppercase());
            break;
        }
    }
    let region = region.or_else(|| match (language.as_str(), script.as_deref()) {
        ("zh", Some("hant")) => Some("TW".to_owned()),
        ("zh", Some("hans")) => Some("CN".to_owned()),
        _ => None,
    });
    Some(match region {
        Some(region) => format!("{language}_{region}"),
        None => language,
    })
}

/// What a finished `evaluatePolicy` means: `Ok(true)` if the user authenticated,
/// `Ok(false)` if they failed, cancelled or the system cancelled the dialog, an error if
/// the system cannot ask at all. `code` is the `LAError` code (LAError.h) when there was
/// an error.
pub(super) fn authentication_outcome(success: bool, code: Option<isize>) -> std::io::Result<bool> {
    if success {
        return Ok(true);
    }
    match code {
        // AuthenticationFailed, UserCancel, UserFallback, SystemCancel, AppCancel.
        Some(-1 | -2 | -3 | -4 | -9) => Ok(false),
        Some(code) => Err(std::io::Error::other(format!(
            "the system cannot authenticate you (LAError {code})"
        ))),
        None => Ok(false),
    }
}

/// Text for a property list `<string>`.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // Not allowed in XML 1.0 at all.
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// A LaunchAgent property list (launchd.plist(5)) that runs `arguments` as `label`.
pub(super) fn launch_agent_plist(
    label: &str,
    arguments: &[String],
    run_at_load: bool,
    log: Option<&Path>,
) -> String {
    let mut plist = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    plist.push_str(&format!(
        "\t<key>Label</key>\n\t<string>{}</string>\n",
        xml_escape(label)
    ));
    plist.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for argument in arguments {
        plist.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(argument)));
    }
    plist.push_str("\t</array>\n");
    plist.push_str(&format!("\t<key>RunAtLoad</key>\n\t<{run_at_load}/>\n"));
    if let Some(log) = log {
        let log = xml_escape(&log.to_string_lossy());
        plist.push_str(&format!(
            "\t<key>StandardErrorPath</key>\n\t<string>{log}</string>\n\
             \t<key>StandardOutPath</key>\n\t<string>{log}</string>\n"
        ));
    }
    plist.push_str("</dict>\n</plist>\n");
    plist
}

/// Whether a property list has `RunAtLoad` set to true (the first such key).
pub(super) fn plist_run_at_load(plist: &str) -> bool {
    plist
        .split_once("<key>RunAtLoad</key>")
        .is_some_and(|(_, rest)| rest.trim_start().starts_with("<true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_environment_in_procargs2() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend(b"/usr/bin/ssh\0\0\0\0");
        buf.extend(b"ssh\0host\0");
        buf.extend(b"HOME=/Users/me\0SSH_CONNECTION=10.0.0.2 5000 10.0.0.1 22\0\0\0");
        assert_eq!(
            procargs2_env(&buf, "SSH_CONNECTION").as_deref(),
            Some("10.0.0.2 5000 10.0.0.1 22")
        );
        assert_eq!(procargs2_env(&buf, "SSH_TTY"), None);
        // A variable named like an argument is not found among the arguments.
        let mut buf = 1i32.to_ne_bytes().to_vec();
        buf.extend(b"/bin/x\0\0SSH_CONNECTION=arg\0A=b\0");
        assert_eq!(procargs2_env(&buf, "SSH_CONNECTION"), None);
        assert_eq!(procargs2_env(&[1, 0], "A"), None);
        assert_eq!(procargs2_env(&[], "A"), None);
    }

    #[test]
    fn terminals_and_times() {
        assert_eq!(terminal_id(0), None);
        assert_eq!(terminal_id(u32::MAX), None);
        assert_eq!(terminal_id(0x1000_0004), Some(0x1000_0004));
        assert_eq!(start_micros(2, 5), 2_000_005);
        assert_eq!(start_micros(u64::MAX, 5), u64::MAX);
    }

    #[test]
    fn boundaries() {
        assert!(is_boundary_program("launchd"));
        assert!(is_boundary_program("sshd-session"));
        assert!(!is_boundary_program("Terminal"));
        assert!(is_remote_login_program("sshd"));
        assert!(!is_remote_login_program("launchd"));
    }

    #[test]
    fn converts_language_tags() {
        let posix = |t| bcp47_to_posix(t);
        assert_eq!(posix("en-US").as_deref(), Some("en_US"));
        assert_eq!(posix("fr").as_deref(), Some("fr"));
        assert_eq!(posix("pt-BR").as_deref(), Some("pt_BR"));
        assert_eq!(posix("zh-Hant-TW").as_deref(), Some("zh_TW"));
        assert_eq!(posix("zh-Hans-CN").as_deref(), Some("zh_CN"));
        assert_eq!(posix("zh-Hant").as_deref(), Some("zh_TW"));
        assert_eq!(posix("zh-Hant-HK").as_deref(), Some("zh_HK"));
        assert_eq!(posix("sr-Latn").as_deref(), Some("sr"));
        assert_eq!(posix("es-419").as_deref(), Some("es_419"));
        assert_eq!(posix("en_GB").as_deref(), Some("en_GB"));
        assert_eq!(posix(""), None);
        assert_eq!(posix("x"), None);
    }

    #[test]
    fn reads_authentication_results() {
        assert!(authentication_outcome(true, None).unwrap());
        assert!(!authentication_outcome(false, Some(-2)).unwrap());
        assert!(!authentication_outcome(false, Some(-1)).unwrap());
        // PasscodeNotSet, NotInteractive: nothing to ask with.
        assert!(authentication_outcome(false, Some(-5)).is_err());
        assert!(authentication_outcome(false, Some(-1004)).is_err());
    }

    #[test]
    fn writes_a_launch_agent() {
        let args = ["/Apps/nib & co/nibwardend".to_owned(), "--x".to_owned()];
        let plist = launch_agent_plist("me.example.d", &args, true, Some(Path::new("/l/d.log")));
        assert!(plist.contains("<key>Label</key>\n\t<string>me.example.d</string>"));
        assert!(plist.contains("<string>/Apps/nib &amp; co/nibwardend</string>"));
        assert!(plist.contains("<string>--x</string>"));
        assert!(plist.contains("<key>StandardErrorPath</key>\n\t<string>/l/d.log</string>"));
        assert!(plist_run_at_load(&plist));
        let off = launch_agent_plist("me.example.d", &args, false, None);
        assert!(!plist_run_at_load(&off));
        assert!(!off.contains("StandardErrorPath"));
        assert!(!plist_run_at_load("<dict></dict>"));
    }
}
