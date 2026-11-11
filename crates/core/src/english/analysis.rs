use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::inflection::known_verbs;

/// The result of recognizing a sentence in the supported English grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Analysis<'a> {
    Supported(AnalyzedText<'a>),
    Unsupported,
}

/// A recognized sentence with its original spelling and spacing intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedText<'a> {
    pub(crate) source: &'a str,
    pub(crate) contraction: Option<ContractionEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContractionEdit {
    pub(crate) range: Range<usize>,
    pub(crate) replacement: &'static str,
    pub(crate) contracted: bool,
}

impl<'a> AnalyzedText<'a> {
    pub fn source(&self) -> &'a str {
        self.source
    }

    /// Returns a replacement range, replacement text, and whether it contracts negation.
    pub fn contraction(&self) -> Option<(Range<usize>, &'static str, bool)> {
        self.contraction
            .as_ref()
            .map(|edit| (edit.range.clone(), edit.replacement, edit.contracted))
    }
}

/// Recognizes individual English clauses using packaged verb forms.
///
/// The supported predicates include simple, progressive, perfect, and perfect
/// progressive constructions using the packaged verb lexicon. Subjects may be
/// pronouns, short determiner phrases, or names. Analysis exposes only an
/// unambiguous negation edit and preserves every byte outside that edit.
///
/// Conditions, coordinated clauses, quotations, negative questions, subject
/// contractions, and contrastive negation remain unsupported. Unsupported prose
/// is not an error and does not imply that the input is ungrammatical English.
pub fn analyze(text: &str) -> Analysis<'_> {
    analyze_inner(text).map_or(Analysis::Unsupported, Analysis::Supported)
}

fn analyze_inner(text: &str) -> Option<AnalyzedText<'_>> {
    let trimmed = text.trim();
    let offset = text.len() - text.trim_start().len();
    if trimmed.unicode_sentences().count() != 1 {
        return None;
    }
    let (body, question) = if let Some(body) = trimmed.strip_suffix('.') {
        (body, false)
    } else {
        (trimmed.strip_suffix('?')?, true)
    };
    if body
        .chars()
        .any(|c| (c.is_control() && c != '\t') || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    let tokens = tokens(body, offset);
    if question {
        let primary = tokens.first()?.text.to_ascii_lowercase();
        if !is_auxiliary(&primary) {
            return None;
        }
        let matches = (2..tokens.len().min(7))
            .filter(|&end| subject(&tokens[1..end]) && agrees(&primary, &tokens[1..end]) && predicate(&primary, &tokens[end..]))
            .count();
        return (matches == 1).then_some(AnalyzedText {
            source: text,
            contraction: None,
        });
    }
    // A bounded subject search avoids guessing clause boundaries in long prose.
    let mut found = None;
    for index in 1..tokens.len().min(6) {
        if !subject(&tokens[..index]) {
            continue;
        }
        let token = &tokens[index];
        let (primary, predicate_start, contraction) = if let Some((primary, replacement)) = super::negation::expand(token.text)
        {
            (
                primary,
                index + 1,
                Some(ContractionEdit {
                    range: token.range.clone(),
                    replacement,
                    contracted: false,
                }),
            )
        } else if token.text == "cannot" {
            (
                "can",
                index + 1,
                Some(ContractionEdit {
                    range: token.range.clone(),
                    replacement: "can't",
                    contracted: true,
                }),
            )
        } else if is_auxiliary(token.text) {
            if tokens.get(index + 1).is_some_and(|token| token.text == "not") {
                // "can not" can negate the predicate rather than the ability.
                if token.text == "can" {
                    continue;
                }
                let contraction = super::negation::contract(token.text).map(|replacement| ContractionEdit {
                    range: token.range.start..tokens[index + 1].range.end,
                    replacement,
                    contracted: true,
                });
                (token.text, index + 2, contraction)
            } else {
                (token.text, index + 1, None)
            }
        } else {
            continue;
        };
        if agrees(primary, &tokens[..index]) && predicate(primary, &tokens[predicate_start..]) {
            if found.is_some() {
                return None;
            }
            found = Some(AnalyzedText {
                source: text,
                contraction,
            });
        }
    }
    found.or_else(|| {
        lexical_statement(&tokens).then_some(AnalyzedText {
            source: text,
            contraction: None,
        })
    })
}

fn lexical_statement(tokens: &[Token<'_>]) -> bool {
    let mut matches = 0;
    for index in 1..tokens.len().min(6) {
        if !subject(&tokens[..index]) || !tokens[index..].iter().all(|token| safe_word(token.text)) {
            continue;
        }
        let word = tokens[index].text;
        let pronoun = (index == 1).then(|| tokens[0].text.to_ascii_lowercase());
        let valid = known_verbs().iter().any(|verb| {
            if verb.base == "be" {
                return false;
            }
            word == verb.past
                || match pronoun.as_deref() {
                    Some("he" | "she" | "it") => word == verb.third_person_singular,
                    Some("i" | "you" | "we" | "they") => word == verb.base,
                    _ => word == verb.base || word == verb.third_person_singular,
                }
        });
        matches += usize::from(valid);
    }
    matches == 1
}

struct Token<'a> {
    text: &'a str,
    range: Range<usize>,
}

fn tokens(text: &str, offset: usize) -> Vec<Token<'_>> {
    let mut cursor = 0;
    text.split_whitespace()
        .map(|word| {
            let start = cursor + text[cursor..].find(word).expect("split word is in source");
            cursor = start + word.len();
            Token {
                text: word,
                range: offset + start..offset + cursor,
            }
        })
        .collect()
}

fn subject(tokens: &[Token<'_>]) -> bool {
    let Some(first) = tokens.first() else { return false };
    let first_lower = first.text.to_ascii_lowercase();
    if matches!(first_lower.as_str(), "i" | "you" | "he" | "she" | "it" | "we" | "they") {
        return tokens.len() == 1;
    }
    if determiner(&first_lower) {
        return tokens.len() >= 2
            && tokens[1..].iter().all(|token| {
                let lower = token.text.to_ascii_lowercase();
                safe_word(token.text)
                    && !determiner(&lower)
                    && !is_auxiliary(&lower)
                    && !matches!(lower.as_str(), "i" | "you" | "he" | "she" | "it" | "we" | "they" | "to")
            });
    }
    if tokens.len() == 1 && safe_word(first.text) && identifier(first.text) {
        return true;
    }
    tokens.len() <= 3
        && tokens.iter().all(|token| {
            safe_word(token.text)
                && token.text.chars().next().is_some_and(char::is_uppercase)
                && !is_auxiliary(&token.text.to_ascii_lowercase())
        })
}

fn identifier(text: &str) -> bool {
    text.chars().any(char::is_alphabetic) && (text.contains('_') || text.bytes().any(|byte| byte.is_ascii_digit()))
}

fn determiner(text: &str) -> bool {
    matches!(
        text,
        "the" | "a" | "an" | "this" | "that" | "these" | "those" | "my" | "your" | "our" | "their" | "his" | "her" | "its"
    )
}

fn agrees(primary: &str, subject: &[Token<'_>]) -> bool {
    if subject.len() != 1 {
        return true;
    }
    let allowed: &[&str] = match subject[0].text.to_ascii_lowercase().as_str() {
        "i" => &["am", "was", "do", "have"],
        "you" | "we" | "they" => &["are", "were", "do", "have"],
        "he" | "she" | "it" => &["is", "was", "does", "has"],
        _ => return true,
    };
    !matches!(primary, "am" | "is" | "are" | "was" | "were" | "do" | "does" | "have" | "has") || allowed.contains(&primary)
}

fn is_auxiliary(text: &str) -> bool {
    matches!(
        text,
        "can"
            | "could"
            | "may"
            | "might"
            | "must"
            | "should"
            | "will"
            | "would"
            | "do"
            | "does"
            | "did"
            | "am"
            | "is"
            | "are"
            | "was"
            | "were"
            | "have"
            | "has"
            | "had"
    )
}

fn predicate(primary: &str, tokens: &[Token<'_>]) -> bool {
    if tokens.is_empty() || !tokens.iter().all(|token| safe_word(token.text)) {
        return false;
    }
    match primary {
        "do" | "does" | "did" => base_predicate(tokens),
        "am" | "is" | "are" | "was" | "were" => be_complement(tokens),
        "have" | "has" | "had" => perfect_predicate(tokens),
        _ => match tokens[0].text {
            "be" => be_complement(&tokens[1..]),
            "have"
                if tokens
                    .get(1)
                    .is_some_and(|token| token.text == "been" || is_participle(token.text)) =>
            {
                perfect_predicate(&tokens[1..])
            }
            _ => base_predicate(tokens),
        },
    }
}

fn base_predicate(tokens: &[Token<'_>]) -> bool {
    let Some(first) = tokens.first() else { return false };
    if first.text == "have" && tokens.get(1).is_some_and(|token| token.text == "to") {
        return tokens
            .get(2)
            .is_some_and(|token| known_verbs().iter().any(|verb| verb.base == token.text));
    }
    known_verbs().iter().any(|verb| verb.base == first.text)
}

fn perfect_predicate(tokens: &[Token<'_>]) -> bool {
    match tokens.first().map(|token| token.text) {
        Some("been") => be_complement(&tokens[1..]),
        Some(word) => is_participle(word),
        None => false,
    }
}

fn is_participle(word: &str) -> bool {
    known_verbs().iter().any(|verb| verb.past_participle == word)
}

fn be_complement(tokens: &[Token<'_>]) -> bool {
    let Some(first) = tokens.first() else { return false };
    if known_verbs()
        .iter()
        .any(|verb| verb.present_participle == first.text || verb.past_participle == first.text)
    {
        return true;
    }
    if determiner(first.text) {
        return tokens.len() > 1;
    }
    if matches!(
        first.text,
        "in" | "on" | "at" | "near" | "under" | "above" | "outside" | "inside" | "with" | "without"
    ) {
        return tokens.len() > 1;
    }
    matches!(
        first.text,
        "ready"
            | "late"
            | "early"
            | "available"
            | "unavailable"
            | "hungry"
            | "thirsty"
            | "open"
            | "closed"
            | "complete"
            | "correct"
            | "incorrect"
            | "certain"
            | "safe"
            | "sure"
            | "possible"
            | "impossible"
            | "necessary"
            | "happy"
            | "sad"
            | "tired"
            | "well"
            | "ill"
            | "here"
            | "there"
            | "alone"
            | "able"
            | "unable"
            | "busy"
            | "free"
            | "valid"
            | "invalid"
            | "empty"
            | "full"
            | "active"
            | "inactive"
            | "online"
            | "offline"
            | "clear"
            | "wrong"
            | "right"
    )
}

fn safe_word(word: &str) -> bool {
    word.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        && word.unicode_words().count() > 0
        && !matches!(
            word.to_ascii_lowercase().as_str(),
            "if" | "when"
                | "unless"
                | "because"
                | "although"
                | "while"
                | "that"
                | "whether"
                | "who"
                | "whom"
                | "whose"
                | "which"
                | "where"
                | "as"
                | "or"
                | "and"
                | "but"
                | "not"
                | "no"
                | "never"
                | "neither"
                | "nor"
                | "only"
                | "just"
                | "merely"
                | "except"
                | "rather"
                | "instead"
                | "so"
        )
}

// Style rules ask for the same bounded predicate evidence as prose analysis.
pub(crate) fn style_predicate(auxiliary: &str, body: &str) -> bool {
    predicate(auxiliary, &tokens(body, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(source: &str) -> String {
        let Analysis::Supported(analysis) = analyze(source) else {
            panic!("unsupported: {source}")
        };
        let mut output = source.to_owned();
        if let Some((range, replacement, _)) = analysis.contraction() {
            output.replace_range(range, replacement);
        }
        output
    }

    #[test]
    fn recognizes_modal_negation_without_changing_other_bytes() {
        for (source, expected) in [
            ("The guest cannot check in.", "The guest can't check in."),
            ("The guest can't check in.", "The guest cannot check in."),
            ("I will not cancel booking ZX-42.", "I won't cancel booking ZX-42."),
            ("They couldn’t update résumé_ID-42.", "They could not update résumé_ID-42."),
            (
                "  Our agents must not\tprocess request ABC_123.\n",
                "  Our agents mustn't\tprocess request ABC_123.\n",
            ),
            ("You wouldn't leave.", "You would not leave."),
            ("She should not pay.", "She shouldn't pay."),
            ("They cannot update cannot_ID.", "They can't update cannot_ID."),
            ("They cannot cancel booking cannot.", "They can't cancel booking cannot."),
        ] {
            assert_eq!(rewrite(source), expected);
        }
    }

    #[test]
    fn recognizes_supported_sentences_without_inventing_variations() {
        for source in [
            "The guest can check in.",
            "Can the guest check in?",
            "Will they cancel booking ZX-42?",
            "The guest may not check in.",
            "They might not arrive.",
        ] {
            let Analysis::Supported(analysis) = analyze(source) else {
                panic!("unsupported: {source}")
            };
            assert_eq!(analysis.source(), source);
            assert!(analysis.contraction().is_none());
        }
    }

    #[test]
    fn rejects_ambiguity_and_unsupported_grammar() {
        for source in [
            "",
            "The guest can not check in.",
            "The guest cannot only check in.",
            "The guest cannot check in but can leave.",
            "The guest will not cancel if they arrive.",
            "The guest cannot check in. They can leave.",
            "Can't the guest check in?",
            "Can the guest not check in?",
            "I have to not cancel.",
            "The guest can't say 'cancel'.",
            "The guest can't update won't_ID.",
            "The guest cannot check in",
            "The guest cannot check in!",
            "I wouldn't say that they can cancel.",
            "The guest's agent cannot check in.",
            "The guest cannot check in, sadly.",
            "If the guest arrives late, can they check in?",
            "I will\nnot cancel.",
            "I will\u{1c}not cancel.",
            "I will not\u{2028}cancel.",
            "I will not cancel booking \"cannot\".",
            "The guest cannot check in; they can leave.",
            "The guest will not cancel unless they arrive.",
            "The guest will not cancel, or leave.",
        ] {
            assert_eq!(analyze(source), Analysis::Unsupported, "{source}");
        }
    }

    #[test]
    fn ordinary_auxiliaries_preserve_tense_and_aspect() {
        for (source, expected) in [
            ("The editor does not write letters.", "The editor doesn't write letters."),
            ("Maya did not leave.", "Maya didn't leave."),
            ("They are not running.", "They aren't running."),
            ("The editor has not written letters.", "The editor hasn't written letters."),
            ("They had not been waiting.", "They hadn't been waiting."),
            ("The children were not swimming.", "The children weren't swimming."),
            ("The teacher was not ready.", "The teacher wasn't ready."),
            ("Our cat is not sleeping.", "Our cat isn't sleeping."),
            ("The musicians do not play music.", "The musicians don't play music."),
            ("The parcel has not been delivered.", "The parcel hasn't been delivered."),
            ("She will not have written the letter.", "She won't have written the letter."),
            ("They could not have been waiting.", "They couldn't have been waiting."),
        ] {
            assert_eq!(rewrite(source), expected, "{source}");
            assert_eq!(rewrite(expected), source, "{expected}");
        }
    }

    #[test]
    fn obligation_scope_and_authored_subjects_are_preserved() {
        for (source, expected) in [
            ("I do not have to cancel.", "I don't have to cancel."),
            ("I will not have to cancel.", "I won't have to cancel."),
            ("Guest_42 cannot check in.", "Guest_42 can't check in."),
            ("api_v2 cannot update résumé_ID-42.", "api_v2 can't update résumé_ID-42."),
            ("batch-42 did not finish.", "batch-42 didn't finish."),
            (
                "José García did not send résumé_ID-42.",
                "José García didn't send résumé_ID-42.",
            ),
            ("That old tree is not growing.", "That old tree isn't growing."),
        ] {
            assert_eq!(rewrite(source), expected, "{source}");
            assert_eq!(rewrite(expected), source, "{expected}");
        }
        assert_eq!(analyze("I have to not cancel."), Analysis::Unsupported);
    }

    #[test]
    fn positive_questions_and_uncontractible_negation_are_recognized() {
        for source in [
            "Does Maya write letters?",
            "Did the editor leave?",
            "Are they running?",
            "Has the editor written letters?",
            "Had they been waiting?",
            "I am not ready.",
        ] {
            let Analysis::Supported(analysis) = analyze(source) else {
                panic!("unsupported: {source}")
            };
            assert!(analysis.contraction().is_none());
        }
    }

    #[test]
    fn uncertain_auxiliary_constructions_are_not_transformed() {
        for source in [
            "He'd written a letter.",
            "She's running.",
            "She isn't only running.",
            "They do not running.",
            "She has not wrote letters.",
            "They had not been wait.",
            "The child is not thinking when asleep.",
            "Doesn't Maya write?",
            "Did Maya not leave?",
            "They have not a reservation.",
            "He do not write letters.",
            "They has not written letters.",
            "She are not running.",
            "I do not have to not cancel.",
            "The child does not sing and dance.",
            "Maya did not say \"leave\".",
            "Ella no escribió la carta.",
            "Sie hat den Brief nicht geschrieben.",
        ] {
            assert_eq!(analyze(source), Analysis::Unsupported, "{source}");
        }
    }
}
