//! Secret references: `nw://[<folder>/]<item>/<field>`.
//!
//! - `<folder>` is a folder name or id; `*` (or leaving it out) means any folder.
//! - `<item>` is an item name or id.
//! - `<field>` is a built-in field (`password`, `username`, `totp`, `notes`, `uri`, ...) or
//!   a custom field name.
//!
//! A `/` or `%` inside a segment is written `%2F` / `%25`; any other `%XX` UTF-8 escape is
//! accepted too.

use std::{fmt, str::FromStr};

pub const SCHEME: &str = "nw://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub folder: Option<String>,
    pub item: String,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid reference {reference:?}: {reason}")]
pub struct ReferenceError {
    reference: String,
    reason: &'static str,
}

impl Reference {
    pub fn is_reference(s: &str) -> bool {
        s.starts_with(SCHEME)
    }
}

impl FromStr for Reference {
    type Err = ReferenceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let error = |reason| ReferenceError {
            reference: s.to_owned(),
            reason,
        };
        let rest = s
            .strip_prefix(SCHEME)
            .ok_or_else(|| error("must start with nw://"))?;
        let segments = rest
            .split('/')
            .map(|seg| percent_decode(seg).ok_or_else(|| error("bad %-escape")))
            .collect::<Result<Vec<_>, _>>()?;
        if segments.iter().any(String::is_empty) {
            return Err(error("empty segment"));
        }
        let (folder, item, field) = match <[String; 2]>::try_from(segments) {
            Ok([item, field]) => (None, item, field),
            Err(segments) => match <[String; 3]>::try_from(segments) {
                Ok([folder, item, field]) => ((folder != "*").then_some(folder), item, field),
                Err(_) => return Err(error("expected nw://[folder/]item/field")),
            },
        };
        Ok(Self {
            folder,
            item,
            field,
        })
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let enc = |s: &str| s.replace('%', "%25").replace('/', "%2F");
        write!(
            f,
            "{SCHEME}{}/{}/{}",
            self.folder.as_deref().map_or_else(|| "*".to_owned(), enc),
            enc(&self.item),
            enc(&self.field)
        )
    }
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Reference {
        s.parse().unwrap()
    }

    #[test]
    fn two_and_three_segments() {
        assert_eq!(
            parse("nw://GitHub/password"),
            Reference {
                folder: None,
                item: "GitHub".into(),
                field: "password".into()
            }
        );
        assert_eq!(
            parse("nw://Work/GitHub/totp").folder.as_deref(),
            Some("Work")
        );
        assert_eq!(parse("nw://*/GitHub/totp").folder, None);
    }

    #[test]
    fn escapes_and_unicode() {
        let r = parse("nw://工作/a%2Fb/API%20key");
        assert_eq!(r.folder.as_deref(), Some("工作"));
        assert_eq!(r.item, "a/b");
        assert_eq!(r.field, "API key");
        assert_eq!(parse(&r.to_string()), r);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "op://a/b",
            "nw://only",
            "nw://a/b/c/d",
            "nw://a//c",
            "nw://a/b%2",
            "nw://a/b%zz",
            "nw://a/%ff",
        ] {
            assert!(bad.parse::<Reference>().is_err(), "{bad}");
        }
    }
}
