use windows::{
    Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
    core::PWSTR,
};

/// The user's display languages from Settings > Time & language, most preferred first
/// (`GetUserPreferredUILanguages`: BCP 47 names such as `zh-Hant-TW`, `en-US`).
///
/// A script subtag Windows adds (`zh-Hant-TW`, `sr-Latn-RS`) is dropped: translations here
/// are named by language and region (`zh_TW`).
pub fn preferred_languages() -> Vec<String> {
    let mut count = 0u32;
    let mut len = 0u32;
    // SAFETY: the first call reports the buffer size needed, the second fills the buffer.
    let list = unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err()
            || len == 0
        {
            return Vec::new();
        }
        let mut buffer = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
        .is_err()
        {
            return Vec::new();
        }
        buffer.truncate(len as usize);
        buffer
    };
    let mut tags: Vec<String> = Vec::new();
    // A multi-string: names separated by NULs, ended by an empty one.
    for name in list.split(|&unit| unit == 0).filter(|n| !n.is_empty()) {
        let name = String::from_utf16_lossy(name);
        if let Some(tag) = super::normalize(&without_script(&name))
            && !tags.contains(&tag)
        {
            tags.push(tag);
        }
    }
    tags
}

/// `zh-Hant-TW` -> `zh-TW`: drops a four-letter script subtag.
fn without_script(tag: &str) -> String {
    tag.split('-')
        .enumerate()
        .filter(|(i, part)| !(*i > 0 && part.len() == 4 && part.chars().all(char::is_alphabetic)))
        .map(|(_, part)| part)
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_scripts() {
        assert_eq!(without_script("zh-Hant-TW"), "zh-TW");
        assert_eq!(without_script("sr-Latn-RS"), "sr-RS");
        assert_eq!(without_script("en-US"), "en-US");
        assert_eq!(without_script("zh-Hans"), "zh");
        assert_eq!(without_script("de"), "de");
    }

    #[test]
    fn answers() {
        // CI runners have at least en-US; a bare Wine may have nothing.
        for tag in preferred_languages() {
            assert!(!tag.is_empty() && !tag.contains('-'), "{tag}");
        }
    }
}
