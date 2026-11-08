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

#[cfg(target_os = "linux")]
mod imp {
    /// gettext's order: `LANGUAGE` (a colon-separated list) unless the locale is C, then
    /// `LC_ALL`, `LC_MESSAGES`, `LANG`.
    pub fn preferred_languages() -> Vec<String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|v| var(v));
        let mut tags: Vec<String> = Vec::new();
        if locale.as_deref().and_then(super::normalize).is_some()
            && let Some(list) = var("LANGUAGE")
        {
            tags.extend(list.split(':').filter_map(super::normalize));
        }
        tags.extend(locale.as_deref().and_then(super::normalize));
        let mut seen = std::collections::HashSet::new();
        tags.retain(|t| seen.insert(t.clone()));
        tags
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn preferred_languages() -> Vec<String> {
        Vec::new()
    }
}

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
