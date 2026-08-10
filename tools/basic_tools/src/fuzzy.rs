//! Fuzzy text normalization used when an exact `old_text` match fails.
//!
//! Mirrors pi's `normalizeForFuzzyMatch`: NFKC, per-line trailing whitespace
//! stripping, and ASCII folding of smart quotes, Unicode dashes, and spaces.

use unicode_normalization::UnicodeNormalization;

/// Normalize text so cosmetic Unicode differences stop blocking a match.
pub fn normalize(text: &str) -> String {
    let folded: String = text.nfkc().map(fold).collect();
    folded
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn fold(character: char) -> char {
    match character {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        other => other,
    }
}

/// Locate `needle` in `haystack`, preferring an exact match.
///
/// Returns the match offset, its length, and the content the replacement must be
/// applied to. A fuzzy hit reports offsets in normalized space.
pub fn find(haystack: &str, needle: &str) -> Option<(usize, usize, Option<String>)> {
    if let Some(index) = haystack.find(needle) {
        return Some((index, needle.len(), None));
    }
    let normalized = normalize(haystack);
    let target = normalize(needle);
    let index = normalized.find(&target)?;
    Some((index, target.len(), Some(normalized)))
}

/// Count how many times `needle` appears once cosmetic differences are folded.
pub fn count(haystack: &str, needle: &str) -> usize {
    let target = normalize(needle);
    if target.is_empty() {
        return 0;
    }
    normalize(haystack).matches(&target).count()
}

#[cfg(test)]
mod tests {
    use super::{count, find, normalize};

    #[test]
    fn folds_cosmetic_unicode_differences() {
        assert_eq!(
            normalize("let x = \u{201C}hi\u{201D};  "),
            "let x = \"hi\";"
        );
        assert_eq!(normalize("a\u{2014}b"), "a-b");
        assert_eq!(normalize("a\u{00A0}b"), "a b");
    }

    #[test]
    fn prefers_exact_matches_then_falls_back() {
        let (index, length, base) = find("let a = 1;", "a = 1").unwrap();
        assert_eq!((index, length), (4, 5));
        assert!(base.is_none());

        let (index, length, base) = find("let s = \u{2018}x\u{2019};", "'x'").unwrap();
        assert_eq!((index, length), (8, 3));
        assert_eq!(base.unwrap(), "let s = 'x';");

        assert!(find("abc", "zzz").is_none());
        assert_eq!(count("x\u{2019}y x\u{2019}y", "x'y"), 2);
    }
}
