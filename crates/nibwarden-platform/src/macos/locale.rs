use core_foundation::{array::CFArray, base::TCFType as _, string::CFString};
use core_foundation_sys::locale::CFLocaleCopyPreferredLanguages;

use crate::macos_logic::bcp47_to_posix;

/// `CFLocaleCopyPreferredLanguages`: the languages from System Settings, most preferred
/// first, as BCP 47 (`zh-Hant-TW`). Without any, the environment's locale (`LC_ALL`,
/// `LC_MESSAGES`, `LANG`) is used, which a program started from a script may have set.
pub fn preferred_languages() -> Vec<String> {
    // SAFETY: no arguments; the result is NULL or an owned CFArray of CFStrings.
    let raw = unsafe { CFLocaleCopyPreferredLanguages() };
    let mut tags: Vec<String> = if raw.is_null() {
        Vec::new()
    } else {
        // SAFETY: Copy function: we own the array, and its elements are CFStrings.
        let array = unsafe { CFArray::<CFString>::wrap_under_create_rule(raw) };
        array
            .iter()
            .filter_map(|tag| bcp47_to_posix(&tag.to_string()))
            .collect()
    };
    if tags.is_empty() {
        tags.extend(
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|v| std::env::var(v).ok())
                .find_map(|v| super::normalize(&v)),
        );
    }
    let mut seen = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    tags
}
