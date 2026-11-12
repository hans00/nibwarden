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
