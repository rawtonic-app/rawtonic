//! Hierarchical keywords (G36/G37). A keyword is stored as its path from the
//! top, its levels joined by `|` (`Animals|Dogs`): the form of XMP's
//! `lr:hierarchicalSubject`, which Lightroom, Capture One and Bridge write
//! and read. A keyword without `|` is a top-level keyword. The Library's
//! entry forms (`Animals > Dogs`, `Dogs < Animals`) are parsed by the
//! frontend into this form (`src/utils/keywordTree.ts`).

/// The separator of a keyword's levels.
pub const SEPARATOR: char = '|';

/// The levels of `keyword`, each trimmed; empty levels (`A||B`, a trailing
/// `|`) are dropped.
pub fn levels(keyword: &str) -> impl Iterator<Item = &str> {
    keyword
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|level| !level.is_empty())
}

/// `keyword` in its stored form: trimmed levels joined by `|`
/// (`" Animals | Dogs "` → `Animals|Dogs`); empty when it has no level.
pub fn normalize(keyword: &str) -> String {
    let mut out = String::with_capacity(keyword.len());
    for level in levels(keyword) {
        if !out.is_empty() {
            out.push(SEPARATOR);
        }
        out.push_str(level);
    }
    out
}

/// Whether `keyword` is `path` or a keyword under it, level by level and
/// ignoring case as the library's search does (`Animals|Dogs` is within
/// `animals`, `Animalsx` is not).
pub fn is_within(keyword: &str, path: &str) -> bool {
    let mut keyword = levels(keyword);
    let mut any = false;
    for level in levels(path) {
        any = true;
        match keyword.next() {
            Some(own) if own == level || own.to_lowercase() == level.to_lowercase() => {}
            _ => return false,
        }
    }
    any
}

/// `keyword` and the paths of its parents, the top first
/// (`A|B|C` → `A`, `A|B`, `A|B|C`); in stored form.
pub fn with_ancestors(keyword: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for level in levels(keyword) {
        let path = match out.last() {
            Some(parent) => format!("{parent}{SEPARATOR}{level}"),
            None => level.to_string(),
        };
        out.push(path);
    }
    out
}

/// `keyword` with `from` (its prefix) replaced by `to`: a keyword under
/// `from` keeps its levels below it (`Animals|Dogs|Pugs` renamed
/// `Animals|Dogs` → `Pets` is `Pets|Pugs`). None when it is not within
/// `from`.
pub fn renamed(keyword: &str, from: &str, to: &str) -> Option<String> {
    if !is_within(keyword, from) {
        return None;
    }
    let rest = levels(keyword).skip(levels(from).count());
    Some(normalize(
        &std::iter::once(to)
            .chain(rest)
            .collect::<Vec<_>>()
            .join("|"),
    ))
}

/// What `keyword` becomes when the keyword `path` is deleted: none for
/// `path` itself, a keyword below it moves up one level (`Animals|Dogs|Pugs`
/// without `Animals|Dogs` is `Animals|Pugs`, as Lightroom keeps a deleted
/// keyword's children), any other keyword stays.
pub fn without(keyword: &str, path: &str) -> Option<String> {
    if !is_within(keyword, path) {
        return Some(keyword.to_string());
    }
    let own = levels(path).count();
    if levels(keyword).count() == own {
        return None;
    }
    let kept: Vec<&str> = levels(keyword)
        .enumerate()
        .filter(|(at, _)| *at != own - 1)
        .map(|(_, level)| level)
        .collect();
    Some(kept.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_compared_level_by_level() {
        assert!(is_within("Animals|Dogs", "animals"));
        assert!(is_within("Animals | Dogs", "ANIMALS|dogs"));
        assert!(is_within("Animals", "Animals"));
        assert!(!is_within("Animalsx|Dogs", "Animals"));
        assert!(!is_within("Animals", "Animals|Dogs"));
        assert!(!is_within("Animals", ""));
        assert!(is_within("Ärger|Ölung", "ärger"));
    }

    #[test]
    fn renaming_and_deleting_keep_the_levels_below() {
        assert_eq!(
            renamed("Animals|Dogs|Pugs", "animals|dogs", "Pets").as_deref(),
            Some("Pets|Pugs")
        );
        assert_eq!(renamed("Cats", "Dogs", "Pets"), None);
        assert_eq!(without("Animals|Dogs", "Animals|Dogs"), None);
        assert_eq!(
            without("Animals|Dogs|Pugs", "animals|dogs").as_deref(),
            Some("Animals|Pugs")
        );
        assert_eq!(without("Animals|Dogs", "Animals").as_deref(), Some("Dogs"));
        assert_eq!(without("Cats", "Dogs").as_deref(), Some("Cats"));
    }

    #[test]
    fn normal_form_and_ancestors() {
        assert_eq!(normalize(" Animals | Dogs |"), "Animals|Dogs");
        assert_eq!(normalize("||"), "");
        assert_eq!(with_ancestors("A|B|C"), ["A", "A|B", "A|B|C"]);
        assert_eq!(with_ancestors("flat"), ["flat"]);
    }
}
