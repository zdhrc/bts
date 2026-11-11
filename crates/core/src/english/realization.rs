use super::model::{
    Aspect, Clause, Condition, ConditionPosition, Error, Form, Modal, NegationStyle, Number, Person, Polarity, Predicate,
    RealizeOptions, Sentence, Subject, Tense,
};

/// Renders explicit English grammatical features without making random choices.
///
/// The first auxiliary carries tense, agreement, negation, and question
/// inversion. Lexical verbs use do support when a simple clause needs it.
/// Subjects and complements are authored fragments, not parsed prose. Their
/// agreement and lexical verb forms are supplied by the caller.
pub fn realize(sentence: &Sentence, options: &RealizeOptions) -> Result<String, Error> {
    let mut text = sentence_start(&realize_body(sentence, options)?);
    text.push(match sentence.clause.form {
        Form::Statement => '.',
        Form::Question => '?',
    });
    Ok(text)
}

pub(crate) fn realize_body(sentence: &Sentence, options: &RealizeOptions) -> Result<String, Error> {
    let main = render_clause(ClauseView::from(&sentence.clause), options.negation)?;
    Ok(match &sentence.condition {
        Some(condition) => {
            let condition = render_clause(ClauseView::from(condition), options.negation)?;
            match options.condition_position {
                ConditionPosition::Before => format!("if {condition}, {main}"),
                ConditionPosition::After => format!("{main} if {condition}"),
            }
        }
        None => main,
    })
}

struct ClauseView<'a> {
    subject: &'a Subject,
    predicate: &'a Predicate,
    modal: Option<Modal>,
    tense: Tense,
    aspect: Aspect,
    polarity: Polarity,
    form: Form,
}

impl<'a> From<&'a Clause> for ClauseView<'a> {
    fn from(clause: &'a Clause) -> Self {
        Self {
            subject: &clause.subject,
            predicate: &clause.predicate,
            modal: clause.modal,
            tense: clause.tense,
            aspect: clause.aspect,
            polarity: clause.polarity,
            form: clause.form,
        }
    }
}

impl<'a> From<&'a Condition> for ClauseView<'a> {
    fn from(condition: &'a Condition) -> Self {
        Self {
            subject: &condition.subject,
            predicate: &condition.predicate,
            modal: None,
            tense: condition.tense,
            aspect: condition.aspect,
            polarity: condition.polarity,
            form: Form::Statement,
        }
    }
}

struct VerbPhrase {
    first: String,
    rest: String,
    modal: Option<Modal>,
}

fn render_clause(clause: ClauseView<'_>, style: NegationStyle) -> Result<String, Error> {
    validate_subject(clause.subject)?;
    validate_predicate(clause.predicate)?;
    if clause.modal.is_some() && clause.tense != Tense::Present {
        return Err(Error::UnsupportedConstruction(
            "modal clauses use present tense and express temporal relationships through the modal and aspect",
        ));
    }
    let verb = verb_phrase(&clause);
    let subject = clause.subject.text.as_str();
    let first = verb.first.as_str();
    let start = match (clause.form, clause.polarity, style) {
        (Form::Statement, Polarity::Positive, _) => format!("{subject} {first}"),
        (Form::Question, Polarity::Positive, _) => format!("{first} {subject}"),
        (Form::Statement, Polarity::Negative, NegationStyle::Expanded) if first == "can" => {
            format!("{subject} cannot")
        }
        (Form::Statement, Polarity::Negative, NegationStyle::Expanded) => format!("{subject} {first} not"),
        (Form::Question, Polarity::Negative, NegationStyle::Expanded) => format!("{first} {subject} not"),
        (form, Polarity::Negative, NegationStyle::Contracted) => {
            let contracted = match verb.modal {
                Some(modal) => modal.contraction().ok_or(Error::UnsupportedContraction(modal))?,
                None => super::negation::contract(first)
                    .ok_or(Error::UnsupportedConstruction("no supported contraction for am not"))?,
            };
            match form {
                Form::Statement => format!("{subject} {contracted}"),
                Form::Question => format!("{contracted} {subject}"),
            }
        }
    };
    let mut parts = vec![start.as_str()];
    if !verb.rest.is_empty() {
        parts.push(&verb.rest);
    }
    if let Some(complement) = &clause.predicate.complement {
        parts.push(complement);
    }
    Ok(parts.join(" "))
}

fn verb_phrase(clause: &ClauseView<'_>) -> VerbPhrase {
    let verb = &clause.predicate.verb;
    let modal = clause.modal.or((clause.tense == Tense::Future).then_some(Modal::Will));
    if let Some(modal) = modal {
        let rest = match clause.aspect {
            Aspect::Simple => verb.base.clone(),
            Aspect::Progressive => format!("be {}", verb.present_participle),
            Aspect::Perfect => format!("have {}", verb.past_participle),
            Aspect::PerfectProgressive => format!("have been {}", verb.present_participle),
        };
        return VerbPhrase {
            first: modal.as_str().into(),
            rest,
            modal: Some(modal),
        };
    }
    let (first, rest) = match clause.aspect {
        Aspect::Simple if verb.base == "be" => (finite_be(clause.subject, clause.tense).into(), String::new()),
        Aspect::Simple if clause.polarity == Polarity::Negative || clause.form == Form::Question => {
            (finite_auxiliary("do", clause.subject, clause.tense).into(), verb.base.clone())
        }
        Aspect::Simple => {
            let form = if clause.tense == Tense::Past {
                &verb.past
            } else if third_singular(clause.subject) {
                &verb.third_person_singular
            } else {
                &verb.base
            };
            (form.clone(), String::new())
        }
        Aspect::Progressive => (
            finite_be(clause.subject, clause.tense).into(),
            verb.present_participle.clone(),
        ),
        Aspect::Perfect => (
            finite_auxiliary("have", clause.subject, clause.tense).into(),
            verb.past_participle.clone(),
        ),
        Aspect::PerfectProgressive => (
            finite_auxiliary("have", clause.subject, clause.tense).into(),
            format!("been {}", verb.present_participle),
        ),
    };
    VerbPhrase {
        first,
        rest,
        modal: None,
    }
}

fn third_singular(subject: &Subject) -> bool {
    subject.person == Person::Third && subject.number == Number::Singular
}

fn finite_be(subject: &Subject, tense: Tense) -> &'static str {
    match (tense, subject.person, subject.number) {
        (Tense::Past, Person::First | Person::Third, Number::Singular) => "was",
        (Tense::Past, _, _) => "were",
        (_, Person::First, Number::Singular) => "am",
        (_, Person::Third, Number::Singular) => "is",
        _ => "are",
    }
}

fn finite_auxiliary(base: &str, subject: &Subject, tense: Tense) -> &'static str {
    match (base, tense, third_singular(subject)) {
        ("have", Tense::Past, _) => "had",
        ("have", _, true) => "has",
        ("have", _, false) => "have",
        (_, Tense::Past, _) => "did",
        (_, _, true) => "does",
        (_, _, false) => "do",
    }
}

fn validate_subject(subject: &Subject) -> Result<(), Error> {
    validate_fragment(&subject.text, "subject")?;
    let expected = match subject.text.as_str() {
        "I" => Some((Person::First, Number::Singular)),
        "we" | "We" => Some((Person::First, Number::Plural)),
        "he" | "He" | "she" | "She" | "it" | "It" => Some((Person::Third, Number::Singular)),
        "they" | "They" => Some((Person::Third, Number::Plural)),
        "you" | "You" if subject.person != Person::Second => {
            return Err(Error::InvalidField {
                field: "subject",
                reason: "you requires second person agreement",
            });
        }
        _ => None,
    };
    if expected.is_some_and(|expected| expected != (subject.person, subject.number)) {
        return Err(Error::InvalidField {
            field: "subject",
            reason: "pronoun and agreement features disagree",
        });
    }
    Ok(())
}

fn validate_predicate(predicate: &Predicate) -> Result<(), Error> {
    for (field, form) in [
        ("verb base", &predicate.verb.base),
        ("third person singular verb", &predicate.verb.third_person_singular),
        ("past verb", &predicate.verb.past),
        ("past participle", &predicate.verb.past_participle),
        ("present participle", &predicate.verb.present_participle),
    ] {
        if form.is_empty() || !form.bytes().all(|byte| byte.is_ascii_lowercase()) {
            return Err(Error::InvalidField {
                field,
                reason: "expected a lowercase single word",
            });
        }
    }
    if predicate.verb.base == "be"
        && (predicate.verb.third_person_singular != "is"
            || predicate.verb.past != "was"
            || predicate.verb.past_participle != "been"
            || predicate.verb.present_participle != "being")
    {
        return Err(Error::InvalidField {
            field: "verb",
            reason: "be requires the forms is, was, been, and being",
        });
    }
    if let Some(complement) = &predicate.complement {
        validate_fragment(complement, "complement")?;
    }
    Ok(())
}

fn validate_fragment(fragment: &str, field: &'static str) -> Result<(), Error> {
    if fragment.is_empty() || fragment.trim() != fragment {
        return Err(Error::InvalidField {
            field,
            reason: "expected a nonempty fragment without surrounding whitespace",
        });
    }
    if fragment
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '?' | '!' | ';' | ',' | '\u{2028}' | '\u{2029}'))
        || fragment.ends_with('.')
        || fragment
            .char_indices()
            .any(|(index, ch)| ch == '.' && fragment[index + 1..].chars().next().is_some_and(char::is_whitespace))
    {
        return Err(Error::InvalidField {
            field,
            reason: "expected a fragment without clause or sentence punctuation",
        });
    }
    Ok(())
}

fn sentence_start(text: &str) -> String {
    let first_end = text.find(' ').unwrap_or(text.len());
    let first = &text[..first_end];
    if matches!(
        first,
        "if" | "the"
            | "a"
            | "an"
            | "this"
            | "that"
            | "these"
            | "those"
            | "my"
            | "your"
            | "our"
            | "their"
            | "we"
            | "you"
            | "he"
            | "she"
            | "it"
            | "they"
            | "can"
            | "could"
            | "may"
            | "might"
            | "must"
            | "should"
            | "will"
            | "would"
            | "can't"
            | "couldn't"
            | "mustn't"
            | "shouldn't"
            | "won't"
            | "wouldn't"
            | "am"
            | "is"
            | "are"
            | "was"
            | "were"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "isn't"
            | "aren't"
            | "wasn't"
            | "weren't"
            | "haven't"
            | "hasn't"
            | "hadn't"
            | "don't"
            | "doesn't"
            | "didn't"
    ) {
        let mut capitalized = text.to_owned();
        capitalized[..1].make_ascii_uppercase();
        capitalized
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::english::model::Verb;

    fn guest() -> Subject {
        Subject {
            text: "the guest".into(),
            person: Person::Third,
            number: Number::Singular,
        }
    }

    fn sentence() -> Sentence {
        Sentence {
            clause: Clause {
                subject: guest(),
                modal: Some(Modal::Can),
                tense: Tense::Present,
                aspect: Aspect::Simple,
                polarity: Polarity::Positive,
                predicate: Predicate {
                    verb: Verb::lookup("check").unwrap(),
                    complement: Some("in".into()),
                },
                form: Form::Statement,
            },
            condition: None,
        }
    }

    fn condition(base: &str, singular: &str, complement: &str) -> Condition {
        Condition {
            subject: guest(),
            predicate: Predicate {
                verb: {
                    let verb = Verb::lookup(base).unwrap();
                    assert_eq!(verb.third_person_singular, singular);
                    verb
                },
                complement: Some(complement.into()),
            },
            polarity: Polarity::Positive,
            tense: Tense::Present,
            aspect: Aspect::Simple,
        }
    }

    #[test]
    fn modal_features_control_the_sentence() {
        let mut sentence = sentence();
        let mut options = RealizeOptions::default();
        assert_eq!(realize(&sentence, &options).unwrap(), "The guest can check in.");
        sentence.clause.form = Form::Question;
        assert_eq!(realize(&sentence, &options).unwrap(), "Can the guest check in?");
        sentence.clause.polarity = Polarity::Negative;
        assert_eq!(realize(&sentence, &options).unwrap(), "Can the guest not check in?");
        options.negation = NegationStyle::Contracted;
        assert_eq!(realize(&sentence, &options).unwrap(), "Can't the guest check in?");
        sentence.clause.form = Form::Statement;
        assert_eq!(realize(&sentence, &options).unwrap(), "The guest can't check in.");
        options.negation = NegationStyle::Expanded;
        assert_eq!(realize(&sentence, &options).unwrap(), "The guest cannot check in.");
    }

    #[test]
    fn conditional_order_preserves_explicit_references() {
        let mut sentence = sentence();
        sentence.clause.subject = Subject {
            text: "they".into(),
            person: Person::Third,
            number: Number::Plural,
        };
        sentence.clause.form = Form::Question;
        sentence.condition = Some(condition("arrive", "arrives", "late"));
        let mut options = RealizeOptions::default();
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If the guest arrives late, can they check in?"
        );
        options.condition_position = ConditionPosition::After;
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "Can they check in if the guest arrives late?"
        );
    }

    #[test]
    fn conditional_agreement_uses_explicit_forms_and_do_support() {
        let mut sentence = sentence();
        sentence.condition = Some(condition("have", "has", "a reservation"));
        let options = RealizeOptions::default();
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If the guest has a reservation, the guest can check in."
        );
        sentence.condition.as_mut().unwrap().polarity = Polarity::Negative;
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If the guest does not have a reservation, the guest can check in."
        );
        sentence.condition.as_mut().unwrap().subject = Subject {
            text: "they".into(),
            person: Person::Third,
            number: Number::Plural,
        };
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If they do not have a reservation, the guest can check in."
        );
        assert_eq!(
            realize(
                &sentence,
                &RealizeOptions {
                    negation: NegationStyle::Contracted,
                    ..options
                }
            )
            .unwrap(),
            "If they don't have a reservation, the guest can check in."
        );
    }

    #[test]
    fn be_agrees_and_does_not_use_do_support() {
        let mut sentence = sentence();
        let mut options = RealizeOptions::default();
        sentence.condition = Some(condition("be", "is", "late"));
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If the guest is late, the guest can check in."
        );
        sentence.condition.as_mut().unwrap().polarity = Polarity::Negative;
        options.negation = NegationStyle::Contracted;
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If the guest isn't late, the guest can check in."
        );
        sentence.condition.as_mut().unwrap().subject = Subject {
            text: "I".into(),
            person: Person::First,
            number: Number::Singular,
        };
        assert!(matches!(realize(&sentence, &options), Err(Error::UnsupportedConstruction(_))));
        options.negation = NegationStyle::Expanded;
        assert_eq!(
            realize(&sentence, &options).unwrap(),
            "If I am not late, the guest can check in."
        );
    }

    #[test]
    fn modals_keep_their_meaning_and_unsupported_contractions_fail() {
        let mut sentence = sentence();
        sentence.clause.polarity = Polarity::Negative;
        for (modal, expanded, contracted) in [
            (Modal::Could, "could not", Some("couldn't")),
            (Modal::May, "may not", None),
            (Modal::Might, "might not", None),
            (Modal::Must, "must not", Some("mustn't")),
            (Modal::Should, "should not", Some("shouldn't")),
            (Modal::Will, "will not", Some("won't")),
            (Modal::Would, "would not", Some("wouldn't")),
        ] {
            sentence.clause.modal = Some(modal);
            assert_eq!(
                realize(&sentence, &RealizeOptions::default()).unwrap(),
                format!("The guest {expanded} check in.")
            );
            let result = realize(
                &sentence,
                &RealizeOptions {
                    negation: NegationStyle::Contracted,
                    ..Default::default()
                },
            );
            match contracted {
                Some(contracted) => assert_eq!(result.unwrap(), format!("The guest {contracted} check in.")),
                None => assert_eq!(result, Err(Error::UnsupportedContraction(modal))),
            }
        }
    }

    #[test]
    fn authored_identifiers_and_unicode_are_preserved() {
        let mut sentence = sentence();
        sentence.clause.subject.text = "api_v2".into();
        sentence.clause.predicate.complement = Some("in with José and booking AbC-42.v2".into());
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "api_v2 can check in with José and booking AbC-42.v2."
        );
    }

    #[test]
    fn sentence_initial_determiners_are_capitalized_without_changing_identifiers() {
        let mut sentence = sentence();
        for determiner in ["this", "that", "these", "those", "my", "your", "our", "their"] {
            let plural = matches!(determiner, "these" | "those");
            let noun = if plural { "guests" } else { "guest" };
            sentence.clause.subject.number = if plural { Number::Plural } else { Number::Singular };
            sentence.clause.subject.text = format!("{determiner} {noun}");
            let mut expected = format!("{determiner} {noun} can check in.");
            expected[..1].make_ascii_uppercase();
            assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), expected);
        }
        sentence.clause.subject.text = "their_API".into();
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "their_API can check in."
        );
    }

    #[test]
    fn invalid_fragments_and_agreement_are_errors() {
        for text in [
            "",
            " the guest",
            "the guest ",
            "the guest. Another guest",
            "the guest.\u{a0}Another guest",
            "the guest\n",
            "the guest\u{2028}another guest",
            "the guest\u{2029}another guest",
            "the guest?",
        ] {
            let mut sentence = sentence();
            sentence.clause.subject.text = text.into();
            assert!(matches!(
                realize(&sentence, &RealizeOptions::default()),
                Err(Error::InvalidField { .. })
            ));
        }
        let mut sentence = sentence();
        for text in ["they", "They", "We", "You"] {
            sentence.clause.subject.text = text.into();
            assert!(matches!(
                realize(&sentence, &RealizeOptions::default()),
                Err(Error::InvalidField { .. })
            ));
        }
        sentence.clause.subject = guest();
        sentence.clause.predicate.verb.base = "check in".into();
        assert!(matches!(
            realize(&sentence, &RealizeOptions::default()),
            Err(Error::InvalidField { .. })
        ));
    }

    #[test]
    fn tense_and_aspect_produce_exact_statements_and_questions() {
        let mut sentence = sentence();
        sentence.clause.modal = None;
        sentence.clause.subject.text = "the editor".into();
        sentence.clause.predicate = Predicate {
            verb: Verb::lookup("write").unwrap(),
            complement: Some("the report".into()),
        };
        for (tense, aspect, statement, question) in [
            (
                Tense::Present,
                Aspect::Simple,
                "The editor writes the report.",
                "Does the editor write the report?",
            ),
            (
                Tense::Past,
                Aspect::Simple,
                "The editor wrote the report.",
                "Did the editor write the report?",
            ),
            (
                Tense::Future,
                Aspect::Simple,
                "The editor will write the report.",
                "Will the editor write the report?",
            ),
            (
                Tense::Present,
                Aspect::Progressive,
                "The editor is writing the report.",
                "Is the editor writing the report?",
            ),
            (
                Tense::Past,
                Aspect::Progressive,
                "The editor was writing the report.",
                "Was the editor writing the report?",
            ),
            (
                Tense::Future,
                Aspect::Progressive,
                "The editor will be writing the report.",
                "Will the editor be writing the report?",
            ),
            (
                Tense::Present,
                Aspect::Perfect,
                "The editor has written the report.",
                "Has the editor written the report?",
            ),
            (
                Tense::Past,
                Aspect::Perfect,
                "The editor had written the report.",
                "Had the editor written the report?",
            ),
            (
                Tense::Future,
                Aspect::Perfect,
                "The editor will have written the report.",
                "Will the editor have written the report?",
            ),
            (
                Tense::Present,
                Aspect::PerfectProgressive,
                "The editor has been writing the report.",
                "Has the editor been writing the report?",
            ),
            (
                Tense::Past,
                Aspect::PerfectProgressive,
                "The editor had been writing the report.",
                "Had the editor been writing the report?",
            ),
            (
                Tense::Future,
                Aspect::PerfectProgressive,
                "The editor will have been writing the report.",
                "Will the editor have been writing the report?",
            ),
        ] {
            sentence.clause.tense = tense;
            sentence.clause.aspect = aspect;
            sentence.clause.form = Form::Statement;
            assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), statement);
            sentence.clause.form = Form::Question;
            assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), question);
        }
    }

    #[test]
    fn negation_attaches_to_the_first_auxiliary() {
        let mut sentence = sentence();
        sentence.clause.modal = None;
        sentence.clause.polarity = Polarity::Negative;
        sentence.clause.predicate = Predicate {
            verb: Verb::lookup("leave").unwrap(),
            complement: None,
        };
        for (tense, aspect, expanded, contracted, question, contracted_question) in [
            (
                Tense::Present,
                Aspect::Simple,
                "The guest does not leave.",
                "The guest doesn't leave.",
                "Does the guest not leave?",
                "Doesn't the guest leave?",
            ),
            (
                Tense::Past,
                Aspect::Simple,
                "The guest did not leave.",
                "The guest didn't leave.",
                "Did the guest not leave?",
                "Didn't the guest leave?",
            ),
            (
                Tense::Present,
                Aspect::Progressive,
                "The guest is not leaving.",
                "The guest isn't leaving.",
                "Is the guest not leaving?",
                "Isn't the guest leaving?",
            ),
            (
                Tense::Past,
                Aspect::Progressive,
                "The guest was not leaving.",
                "The guest wasn't leaving.",
                "Was the guest not leaving?",
                "Wasn't the guest leaving?",
            ),
            (
                Tense::Present,
                Aspect::Perfect,
                "The guest has not left.",
                "The guest hasn't left.",
                "Has the guest not left?",
                "Hasn't the guest left?",
            ),
            (
                Tense::Past,
                Aspect::Perfect,
                "The guest had not left.",
                "The guest hadn't left.",
                "Had the guest not left?",
                "Hadn't the guest left?",
            ),
            (
                Tense::Present,
                Aspect::PerfectProgressive,
                "The guest has not been leaving.",
                "The guest hasn't been leaving.",
                "Has the guest not been leaving?",
                "Hasn't the guest been leaving?",
            ),
            (
                Tense::Past,
                Aspect::PerfectProgressive,
                "The guest had not been leaving.",
                "The guest hadn't been leaving.",
                "Had the guest not been leaving?",
                "Hadn't the guest been leaving?",
            ),
            (
                Tense::Future,
                Aspect::Simple,
                "The guest will not leave.",
                "The guest won't leave.",
                "Will the guest not leave?",
                "Won't the guest leave?",
            ),
            (
                Tense::Future,
                Aspect::Progressive,
                "The guest will not be leaving.",
                "The guest won't be leaving.",
                "Will the guest not be leaving?",
                "Won't the guest be leaving?",
            ),
            (
                Tense::Future,
                Aspect::Perfect,
                "The guest will not have left.",
                "The guest won't have left.",
                "Will the guest not have left?",
                "Won't the guest have left?",
            ),
            (
                Tense::Future,
                Aspect::PerfectProgressive,
                "The guest will not have been leaving.",
                "The guest won't have been leaving.",
                "Will the guest not have been leaving?",
                "Won't the guest have been leaving?",
            ),
        ] {
            sentence.clause.tense = tense;
            sentence.clause.aspect = aspect;
            for (form, negation, expected) in [
                (Form::Statement, NegationStyle::Expanded, expanded),
                (Form::Statement, NegationStyle::Contracted, contracted),
                (Form::Question, NegationStyle::Expanded, question),
                (Form::Question, NegationStyle::Contracted, contracted_question),
            ] {
                sentence.clause.form = form;
                assert_eq!(
                    realize(
                        &sentence,
                        &RealizeOptions {
                            negation,
                            ..Default::default()
                        }
                    )
                    .unwrap(),
                    expected
                );
            }
        }
    }

    #[test]
    fn person_and_number_control_be_and_have_agreement() {
        let mut sentence = sentence();
        sentence.clause.modal = None;
        for (text, person, number, present, past, perfect) in [
            (
                "I",
                Person::First,
                Number::Singular,
                "I am ready.",
                "I was ready.",
                "I have been ready.",
            ),
            (
                "we",
                Person::First,
                Number::Plural,
                "We are ready.",
                "We were ready.",
                "We have been ready.",
            ),
            (
                "you",
                Person::Second,
                Number::Singular,
                "You are ready.",
                "You were ready.",
                "You have been ready.",
            ),
            (
                "you",
                Person::Second,
                Number::Plural,
                "You are ready.",
                "You were ready.",
                "You have been ready.",
            ),
            (
                "she",
                Person::Third,
                Number::Singular,
                "She is ready.",
                "She was ready.",
                "She has been ready.",
            ),
            (
                "they",
                Person::Third,
                Number::Plural,
                "They are ready.",
                "They were ready.",
                "They have been ready.",
            ),
        ] {
            sentence.clause.subject = Subject {
                text: text.into(),
                person,
                number,
            };
            sentence.clause.predicate = Predicate {
                verb: Verb::lookup("be").unwrap(),
                complement: Some("ready".into()),
            };
            for (tense, aspect, expected) in [
                (Tense::Present, Aspect::Simple, present),
                (Tense::Past, Aspect::Simple, past),
                (Tense::Present, Aspect::Perfect, perfect),
            ] {
                sentence.clause.tense = tense;
                sentence.clause.aspect = aspect;
                assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), expected);
            }
        }
    }

    #[test]
    fn lexical_have_uses_do_support_and_copular_be_inverts_directly() {
        let mut sentence = sentence();
        sentence.clause.modal = None;
        sentence.clause.predicate = Predicate {
            verb: Verb::lookup("have").unwrap(),
            complement: Some("a reservation".into()),
        };
        sentence.clause.form = Form::Question;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "Does the guest have a reservation?"
        );
        sentence.clause.polarity = Polarity::Negative;
        sentence.clause.form = Form::Statement;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "The guest does not have a reservation."
        );
        sentence.clause.predicate = Predicate {
            verb: Verb::lookup("be").unwrap(),
            complement: Some("ready".into()),
        };
        sentence.clause.form = Form::Question;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "Is the guest not ready?"
        );
        sentence.clause.tense = Tense::Past;
        sentence.clause.polarity = Polarity::Positive;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "Was the guest ready?"
        );
    }

    #[test]
    fn modal_aspect_and_past_conditions_preserve_temporal_relationships() {
        let mut sentence = sentence();
        sentence.clause.modal = Some(Modal::Would);
        sentence.clause.aspect = Aspect::Perfect;
        sentence.clause.subject = Subject {
            text: "they".into(),
            person: Person::Third,
            number: Number::Plural,
        };
        sentence.clause.predicate = Predicate {
            verb: Verb::lookup("wait").unwrap(),
            complement: None,
        };
        let mut condition = condition("arrive", "arrives", "late");
        condition.tense = Tense::Past;
        condition.aspect = Aspect::Perfect;
        sentence.condition = Some(condition);
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "If the guest had arrived late, they would have waited."
        );
        assert_eq!(
            realize(
                &sentence,
                &RealizeOptions {
                    condition_position: ConditionPosition::After,
                    ..Default::default()
                }
            )
            .unwrap(),
            "They would have waited if the guest had arrived late."
        );
        sentence.condition = None;
        sentence.clause.modal = Some(Modal::Should);
        sentence.clause.aspect = Aspect::Progressive;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "They should be waiting."
        );
        sentence.clause.modal = Some(Modal::Might);
        sentence.clause.aspect = Aspect::PerfectProgressive;
        assert_eq!(
            realize(&sentence, &RealizeOptions::default()).unwrap(),
            "They might have been waiting."
        );
        sentence.clause.tense = Tense::Past;
        assert!(matches!(
            realize(&sentence, &RealizeOptions::default()),
            Err(Error::UnsupportedConstruction(_))
        ));
        sentence.clause.tense = Tense::Future;
        assert!(matches!(
            realize(&sentence, &RealizeOptions::default()),
            Err(Error::UnsupportedConstruction(_))
        ));
    }
}
