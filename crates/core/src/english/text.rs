//! Unicode boundaries and explicit English noun inflection.

use unicode_segmentation::UnicodeSegmentation;

use super::{Error, Number};

/// Returns Unicode words, excluding punctuation and whitespace.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    text.unicode_words()
}

/// Returns sentence segments containing letters or numbers, with their spacing.
///
/// Unicode boundaries do not imply that a segment has a supported grammar.
pub fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.unicode_sentences()
}

/// Returns extended grapheme clusters without splitting combining characters.
pub fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// Inflects a single English noun using the packaged pluralizer rules.
///
/// This is an explicit lexical operation, not a way to infer subject agreement
/// or modify identifiers. Unknown nouns use the library's regular rules.
pub fn inflect_noun(noun: &str, number: Number) -> Result<String, Error> {
    if noun.is_empty() || !noun.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(Error::InvalidField {
            field: "noun",
            reason: "expected one English word containing only ASCII letters",
        });
    }
    let count = match number {
        Number::Singular => 1,
        Number::Plural => 2,
    };
    Ok(pluralizer::pluralize(noun, count, false))
}

// Explicit terms, quotations and code-like tokens are opaque to style edits.
pub(crate) fn protected_ranges(text: &str, terms: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    for term in terms.iter().filter(|term| !term.is_empty()) {
        ranges.extend(text.match_indices(term).map(|(start, _)| start..start + term.len()));
    }
    let mut quoted = None;
    for (index, c) in text.char_indices() {
        if let Some((start, close)) = quoted {
            if c == close {
                ranges.push(start..index + c.len_utf8());
                quoted = None;
            }
        } else if matches!(c, '"' | '`' | '“' | '‘')
            || c == '\'' && text[..index].chars().next_back().is_none_or(|p| !p.is_alphanumeric())
        {
            quoted = Some((
                index,
                match c {
                    '“' => '”',
                    '‘' => '’',
                    _ => c,
                },
            ));
        }
    }
    if let Some((start, _)) = quoted {
        ranges.push(start..text.len());
    }
    let mut offset = 0;
    for token in text.split_whitespace() {
        let start = offset + text[offset..].find(token).unwrap();
        let core = token.trim_end_matches(['.', ',', '!', '?', ';', ')', ']']);
        if core
            .chars()
            .any(|c| c.is_numeric() || matches!(c, '@' | '/' | '\\' | '_' | '-' | ':' | '.' | '=' | '{' | '}'))
        {
            ranges.push(start..start + token.len());
        }
        offset = start + token.len();
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_boundaries_preserve_graphemes_and_source() {
        assert_eq!(graphemes("e\u{301}👩‍💻").collect::<Vec<_>>(), ["e\u{301}", "👩‍💻"]);
        assert_eq!(words("The café, ZX-42.").collect::<Vec<_>>(), ["The", "café", "ZX", "42"]);
        let source = "The guest arrives. Can they check in?";
        assert_eq!(sentences(source).collect::<String>(), source);
        assert_eq!(sentences(source).count(), 2);
    }

    #[test]
    fn noun_inflection_is_explicit_and_handles_irregulars() {
        for (noun, number, expected) in [
            ("guest", Number::Plural, "guests"),
            ("guests", Number::Singular, "guest"),
            ("child", Number::Plural, "children"),
            ("children", Number::Singular, "child"),
            ("fish", Number::Plural, "fish"),
        ] {
            assert_eq!(inflect_noun(noun, number).unwrap(), expected);
        }
        for noun in ["", "guest id", "ZX-42", "guest_1"] {
            assert!(inflect_noun(noun, Number::Plural).is_err());
        }
    }
}
