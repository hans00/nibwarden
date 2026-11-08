//! Ranking for the quick access search (phase2.md §7).

use nibwarden_ipc::ItemInfo;

/// Indices into `items` that match `query`, best first. Every word of the query must
/// appear somewhere (name, username, folder, URIs, case-insensitively). Matches in the
/// name rank higher, a name that starts with the query highest; favourites win ties, and
/// the incoming order (the daemon sorts by name) breaks the rest.
pub fn rank(items: &[ItemInfo], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<(u8, bool, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let name = item.name.to_lowercase();
            let rest = [
                item.subtitle.to_lowercase(),
                item.folder.clone().unwrap_or_default().to_lowercase(),
                item.uris.join(" ").to_lowercase(),
            ];
            let found = |word: &&str| name.contains(word) || rest.iter().any(|f| f.contains(word));
            if !words.iter().all(found) {
                return None;
            }
            let score = if words.is_empty() || name.starts_with(&query) {
                0
            } else if name.split_whitespace().any(|w| w.starts_with(&query)) {
                1
            } else if name.contains(&query) {
                2
            } else {
                3
            };
            Some((score, !item.favorite, index))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, _, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, user: &str, favorite: bool) -> ItemInfo {
        ItemInfo {
            id: name.into(),
            name: name.into(),
            kind: "login".into(),
            subtitle: user.into(),
            folder: None,
            uris: vec![format!("https://{}.example", name.to_lowercase())],
            has_totp: false,
            favorite,
            trashed: false,
        }
    }

    #[test]
    fn ranks_name_matches_first() {
        let items = [
            item("Amazon", "git@example.test", false),
            item("GitHub", "alice", false),
            item("My Git server", "bob", false),
            item("Legit", "carol", false),
            item("Bank", "dave", true),
        ];
        let names = |q: &str| -> Vec<&str> {
            rank(&items, q)
                .into_iter()
                .map(|i| items[i].name.as_str())
                .collect()
        };
        assert_eq!(names("git"), ["GitHub", "My Git server", "Legit", "Amazon"]);
        assert_eq!(names("GIT ali"), ["GitHub"]);
        assert_eq!(names("nothing"), Vec::<&str>::new());
        // No query: favourites first, then the given order.
        assert_eq!(
            names(""),
            ["Bank", "Amazon", "GitHub", "My Git server", "Legit"]
        );
    }
}
