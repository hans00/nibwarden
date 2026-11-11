//! The user's language preferences, for choosing a translation.

/// Preferred languages, most preferred first, as POSIX-style tags (`zh_TW`, `en`, `pt_BR`).
/// Empty if unknown.
pub fn preferred_languages() -> Vec<String> {
    imp::preferred_languages()
}

/// `zh_TW.UTF-8@foo` or `zh-TW` → `zh_TW`; `C` and `POSIX` (no preference) → `None`.
pub fn normalize(tag: &str) -> Option<String> {
    let tag = tag.split(['.', '@']).next()?.trim().replace('-', "_");
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        return None;
    }
    Some(match tag.split_once('_') {
        Some((lang, region)) => format!("{}_{}", lang.to_lowercase(), region.to_uppercase()),
        None => tag.to_lowercase(),
    })
}

#[cfg_attr(target_os = "linux", path = "linux/locale.rs")]
#[cfg_attr(target_os = "macos", path = "macos/locale.rs")]
#[cfg_attr(windows, path = "windows/locale.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    path = "other/locale.rs"
)]
mod imp;

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes_tags() {
        assert_eq!(normalize("zh_TW.UTF-8").as_deref(), Some("zh_TW"));
        assert_eq!(normalize("zh-tw").as_deref(), Some("zh_TW"));
        assert_eq!(normalize("en").as_deref(), Some("en"));
        assert_eq!(normalize("de_DE@euro").as_deref(), Some("de_DE"));
        assert_eq!(normalize("C.UTF-8"), None);
        assert_eq!(normalize("POSIX"), None);
    }
}
