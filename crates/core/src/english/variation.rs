//! Meaning-preserving wording choices with explicit or caller-seeded selection.

use std::ops::Range;

use rand::Rng;
use unicode_segmentation::UnicodeSegmentation;

use super::{Analysis, ConditionPosition, Error, Form, NegationStyle, Polarity, RealizeOptions, Sentence, analyze, realize};

/// Enables independent transformations. Disabled controls leave wording alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VariationOptions {
    pub negation: bool,
    pub condition_order: bool,
}

/// A wording change, never a change to modality, polarity, or question form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variation {
    Negation(NegationStyle),
    ConditionPosition(ConditionPosition),
}

/// Lists supported changes in stable order, excluding the current wording.
///
/// Negative questions are excluded from contraction variation because their
/// expanded and contracted forms can imply different speaker expectations.
pub fn applicable_variations(
    sentence: &Sentence,
    rendering: &RealizeOptions,
    options: &VariationOptions,
) -> Result<Vec<Variation>, Error> {
    realize(sentence, rendering)?;
    let mut variations = Vec::with_capacity(2);
    let negative = sentence.clause.polarity == Polarity::Negative
        || sentence
            .condition
            .as_ref()
            .is_some_and(|condition| condition.polarity == Polarity::Negative);
    let negative_question = sentence.clause.form == Form::Question && sentence.clause.polarity == Polarity::Negative;
    if options.negation && negative && !negative_question {
        let negation = match rendering.negation {
            NegationStyle::Expanded => NegationStyle::Contracted,
            NegationStyle::Contracted => NegationStyle::Expanded,
        };
        let candidate = RealizeOptions { negation, ..*rendering };
        match realize(sentence, &candidate) {
            Ok(_) => variations.push(Variation::Negation(negation)),
            Err(Error::UnsupportedContraction(_) | Error::UnsupportedConstruction(_)) => {}
            Err(error) => return Err(error),
        }
    }
    if options.condition_order && sentence.condition.is_some() {
        variations.push(Variation::ConditionPosition(match rendering.condition_position {
            ConditionPosition::Before => ConditionPosition::After,
            ConditionPosition::After => ConditionPosition::Before,
        }));
    }
    Ok(variations)
}

/// Applies an explicit supported selection, returning new rendering options.
///
/// The semantic structure is borrowed and cannot be changed by a variation.
pub fn apply_variation(sentence: &Sentence, rendering: &RealizeOptions, selection: Variation) -> Result<RealizeOptions, Error> {
    let options = VariationOptions {
        negation: true,
        condition_order: true,
    };
    if !applicable_variations(sentence, rendering, &options)?.contains(&selection) {
        return Err(Error::InvalidVariation("selection is not applicable to this sentence"));
    }
    let mut changed = *rendering;
    match selection {
        Variation::Negation(style) => changed.negation = style,
        Variation::ConditionPosition(position) => changed.condition_position = position,
    }
    Ok(changed)
}

/// Chooses uniformly between the original wording and each applicable change.
///
/// At most one change is applied. No randomness is consumed when none applies.
pub fn vary_sentence<R: Rng + ?Sized>(
    sentence: &Sentence,
    rendering: &RealizeOptions,
    options: &VariationOptions,
    rng: &mut R,
) -> Result<String, Error> {
    let variations = applicable_variations(sentence, rendering, options)?;
    let selected = if variations.is_empty() {
        0
    } else {
        rng.random_range(0..=variations.len() as u64) as usize
    };
    let mut changed = *rendering;
    if selected > 0 {
        match variations[selected - 1] {
            Variation::Negation(style) => changed.negation = style,
            Variation::ConditionPosition(position) => changed.condition_position = position,
        }
    }
    realize(sentence, &changed)
}

/// Varies supported prose without reformatting any unchanged source bytes.
///
/// Supports consistent negation register across recognized sentences and ordering
/// of recognized conditions. Unsupported prose and disabled controls preserve
/// the original. The original and each alternative have equal probability.
pub fn vary<R: Rng + ?Sized>(text: &str, options: &VariationOptions, rng: &mut R) -> Result<String, Error> {
    let alternatives = text_alternatives(text, options);
    if alternatives.is_empty() {
        return Ok(text.to_owned());
    }
    let index = rng.random_range(0..=alternatives.len() as u64) as usize;
    Ok(if index == 0 {
        text.to_owned()
    } else {
        alternatives[index - 1].clone()
    })
}

struct Edit {
    range: Range<usize>,
    replacement: &'static str,
    contracts: bool,
}

/// Lists conservative prose alternatives without selecting one.
///
/// Negation changes use a consistent register across all supported clauses.
/// Conditional ordering preserves the authored references. Any unsupported
/// sentence leaves the whole input untouched, represented by an empty list.
pub fn text_alternatives(text: &str, options: &VariationOptions) -> Vec<String> {
    if !options.negation && !options.condition_order {
        return Vec::new();
    }
    let mut edits = Vec::new();
    let mut reordered = None;
    if let Some(conditional) = Conditional::parse(text) {
        if !clause(conditional.condition, '.', conditional.condition_offset, &mut edits)
            || !clause(conditional.main, conditional.punctuation, conditional.main_offset, &mut edits)
        {
            return Vec::new();
        }
        if options.condition_order {
            reordered = Some(conditional.reorder());
        }
    } else {
        let mut offset = 0;
        let mut count = 0;
        for segment in text.split_sentence_bounds() {
            if !segment.trim().is_empty() {
                let Analysis::Supported(analysis) = analyze(segment) else {
                    return Vec::new();
                };
                if let Some((range, replacement, contracts)) = analysis.contraction() {
                    edits.push(Edit {
                        range: offset + range.start..offset + range.end,
                        replacement,
                        contracts,
                    });
                }
                count += 1;
            }
            offset += segment.len();
        }
        if count == 0 {
            return Vec::new();
        }
    }
    edits.sort_by_key(|edit| edit.range.start);
    let mut alternatives = Vec::with_capacity(3);
    if options.negation {
        for contracts in [false, true] {
            let mut changed = text.to_owned();
            for edit in edits.iter().rev().filter(|edit| edit.contracts == contracts) {
                changed.replace_range(edit.range.clone(), edit.replacement);
            }
            if changed != text && !alternatives.contains(&changed) {
                alternatives.push(changed);
            }
        }
    }
    if let Some(reordered) = reordered.filter(|changed| changed != text && !alternatives.contains(changed)) {
        alternatives.push(reordered);
    }
    alternatives
}

fn clause(body: &str, punctuation: char, offset: usize, edits: &mut Vec<Edit>) -> bool {
    let input = format!("{body}{punctuation}");
    let Analysis::Supported(analysis) = analyze(&input) else {
        return false;
    };
    if let Some((range, replacement, contracts)) = analysis.contraction() {
        edits.push(Edit {
            range: offset + range.start..offset + range.end,
            replacement,
            contracts,
        });
    }
    true
}

struct Conditional<'a> {
    source: &'a str,
    condition: &'a str,
    main: &'a str,
    condition_offset: usize,
    main_offset: usize,
    leading: usize,
    trailing: usize,
    punctuation: char,
    before: bool,
}

impl<'a> Conditional<'a> {
    fn parse(source: &'a str) -> Option<Self> {
        let trimmed = source.trim();
        let leading = source.len() - source.trim_start().len();
        let trailing = source.trim_end().len();
        let punctuation = trimmed.chars().last()?;
        if !matches!(punctuation, '.' | '?') || trimmed.unicode_sentences().count() != 1 {
            return None;
        }
        let body = &trimmed[..trimmed.len() - 1];
        if body.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
            return None;
        }
        let (condition, main, condition_offset, main_offset, before) =
            if let Some(rest) = body.strip_prefix("If ").or_else(|| body.strip_prefix("if ")) {
                let (condition, main) = rest.split_once(", ")?;
                if condition.is_empty() || main.is_empty() || main.contains(',') {
                    return None;
                }
                (condition, main, leading + 3, leading + 3 + condition.len() + 2, true)
            } else {
                let (main, condition) = body.split_once(" if ")?;
                if main.is_empty() || condition.is_empty() || main.contains(',') || condition.contains(',') {
                    return None;
                }
                (condition, main, leading + main.len() + 4, leading, false)
            };
        Some(Self {
            source,
            condition,
            main,
            condition_offset,
            main_offset,
            leading,
            trailing,
            punctuation,
            before,
        })
    }

    fn reorder(&self) -> String {
        let main = initial_case(self.main, self.punctuation == '?', self.before);
        let changed = if self.before {
            format!("{main} if {}{}", self.condition, self.punctuation)
        } else {
            format!("If {}, {main}{}", self.condition, self.punctuation)
        };
        format!("{}{changed}{}", &self.source[..self.leading], &self.source[self.trailing..])
    }
}

fn initial_case(text: &str, question: bool, upper: bool) -> String {
    let end = text.find(' ').unwrap_or(text.len());
    let first = &text[..end];
    let lower = first.to_ascii_lowercase();
    let pronoun = matches!(lower.as_str(), "you" | "we" | "they" | "he" | "she" | "it");
    let determiner = matches!(
        lower.as_str(),
        "the" | "a" | "an" | "this" | "that" | "these" | "those" | "my" | "your" | "our" | "their" | "his" | "her" | "its"
    ) && text
        .get(end + 1..)
        .and_then(|tail| tail.chars().next())
        .is_some_and(char::is_lowercase);
    let auxiliary = question
        && matches!(
            lower.as_str(),
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
        );
    if !(pronoun || determiner || auxiliary) || !first.is_ascii() || first.is_empty() {
        return text.to_owned();
    }
    let mut changed = text.to_owned();
    if upper {
        changed[..1].make_ascii_uppercase();
    } else {
        changed[..1].make_ascii_lowercase();
    }
    changed
}

#[cfg(test)]
mod tests {
    use rand::{RngCore, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    use super::*;
    use crate::english::{Aspect, Clause, Condition, Modal, Number, Person, Predicate, Subject, Tense, Verb};

    fn sentence() -> Sentence {
        let subject = Subject {
            text: "the guest".into(),
            person: Person::Third,
            number: Number::Singular,
        };
        Sentence {
            clause: Clause {
                subject: subject.clone(),
                modal: Some(Modal::Can),
                tense: Tense::Present,
                aspect: Aspect::Simple,
                polarity: Polarity::Negative,
                predicate: Predicate {
                    verb: Verb::lookup("check").unwrap(),
                    complement: Some("in with booking ZX-42".into()),
                },
                form: Form::Statement,
            },
            condition: Some(Condition {
                subject,
                predicate: Predicate {
                    verb: Verb::lookup("arrive").unwrap(),
                    complement: Some("late".into()),
                },
                polarity: Polarity::Positive,
                tense: Tense::Present,
                aspect: Aspect::Simple,
            }),
        }
    }

    #[test]
    fn selections_are_explicit_and_do_not_change_semantic_features() {
        let sentence = sentence();
        let original = sentence.clone();
        let rendering = RealizeOptions::default();
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        assert_eq!(
            applicable_variations(&sentence, &rendering, &options).unwrap(),
            [
                Variation::Negation(NegationStyle::Contracted),
                Variation::ConditionPosition(ConditionPosition::After),
            ]
        );
        let changed = apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Contracted)).unwrap();
        assert_eq!(
            realize(&sentence, &changed).unwrap(),
            "If the guest arrives late, the guest can't check in with booking ZX-42."
        );
        assert_eq!(sentence, original);
        assert!(apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Expanded)).is_err());
    }

    #[test]
    fn seeded_choices_are_repeatable_and_only_produce_supported_alternatives() {
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        let sequence = |seed| {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
            (0..64)
                .map(|_| vary_sentence(&sentence(), &RealizeOptions::default(), &options, &mut rng).unwrap())
                .collect::<Vec<_>>()
        };
        let outputs = sequence(42);
        assert_eq!(outputs, sequence(42));
        assert_ne!(outputs, sequence(43));
        let expected = [
            "If the guest arrives late, the guest cannot check in with booking ZX-42.",
            "If the guest arrives late, the guest can't check in with booking ZX-42.",
            "The guest cannot check in with booking ZX-42 if the guest arrives late.",
        ];
        assert!(outputs.iter().all(|output| expected.contains(&output.as_str())));
        assert!(
            expected
                .iter()
                .all(|expected| outputs.iter().any(|output| output == expected))
        );
    }

    #[test]
    fn unsupported_text_and_disabled_variations_do_not_consume_randomness() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut untouched = rng.clone();
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for source in [
            "Don't have to cancel.",
            "They can not cancel.",
            "The guest can check in.",
            "",
            "🙂",
        ] {
            assert_eq!(vary(source, &options, &mut rng).unwrap(), source);
        }
        assert_eq!(
            vary("The guest cannot check in.", &VariationOptions::default(), &mut rng).unwrap(),
            "The guest cannot check in."
        );
        assert_eq!(rng.next_u64(), untouched.next_u64());
    }

    #[test]
    fn text_variation_preserves_identifiers_and_is_reproducible() {
        let source = "  They cannot update résumé_ID-42.\n";
        let options = VariationOptions {
            negation: true,
            condition_order: false,
        };
        let mut first = Xoshiro256PlusPlus::seed_from_u64(19);
        let mut second = first.clone();
        let outputs = (0..32)
            .map(|_| {
                let output = vary(source, &options, &mut first).unwrap();
                assert_eq!(output, vary(source, &options, &mut second).unwrap());
                assert!([source, "  They can't update résumé_ID-42.\n"].contains(&output.as_str()));
                output
            })
            .collect::<Vec<_>>();
        assert!(outputs.iter().any(|output| output == source));
        assert!(outputs.iter().any(|output| output != source));
    }

    #[test]
    fn unsupported_styles_and_negative_questions_are_not_random_alternatives() {
        let mut sentence = sentence();
        sentence.condition = None;
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for modal in [Modal::May, Modal::Might] {
            sentence.clause.modal = Some(modal);
            assert!(
                applicable_variations(&sentence, &RealizeOptions::default(), &options)
                    .unwrap()
                    .is_empty()
            );
        }
        sentence.clause.modal = Some(Modal::Can);
        sentence.clause.form = Form::Question;
        assert!(
            applicable_variations(&sentence, &RealizeOptions::default(), &options)
                .unwrap()
                .is_empty()
        );
        sentence.clause.subject.text.clear();
        assert!(applicable_variations(&sentence, &RealizeOptions::default(), &options).is_err());
    }

    #[test]
    fn conditional_reordering_keeps_references_and_identifiers() {
        let options = VariationOptions {
            negation: false,
            condition_order: true,
        };
        for (before, after) in [
            (
                "If the guest arrives late, can they check in?",
                "Can they check in if the guest arrives late?",
            ),
            (
                "  If Maya has finished, api_v2 will update résumé_ID-42.\n",
                "  api_v2 will update résumé_ID-42 if Maya has finished.\n",
            ),
        ] {
            assert_eq!(text_alternatives(before, &options), [after]);
            assert_eq!(text_alternatives(after, &options), [before]);
        }
    }

    #[test]
    fn contractions_use_consistent_register_and_preserve_other_bytes() {
        let options = VariationOptions {
            negation: true,
            condition_order: false,
        };
        assert_eq!(
            text_alternatives("I won't leave. They cannot stay.", &options),
            ["I will not leave. They cannot stay.", "I won't leave. They can't stay.",]
        );
        assert_eq!(
            text_alternatives("If they do not arrive, Maya cannot leave.", &options),
            ["If they don't arrive, Maya can't leave."]
        );
        assert_eq!(
            text_alternatives("Maya cannot leave if they do not arrive.", &options),
            ["Maya can't leave if they don't arrive."]
        );
    }

    #[test]
    fn unsupported_clauses_prevent_partial_rewrites() {
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for input in [
            "If the guest arrives, can't they check in?",
            "The guest will leave when they arrive.",
            "If they arrive if she leaves, Maya cannot stay.",
            "I cannot leave. She's already gone.",
            "If they arrive, Maya cannot leave, or stay.",
        ] {
            assert!(text_alternatives(input, &options).is_empty(), "{input}");
        }
    }
}
