//! Explicit relationships between English propositions.

use serde::{Deserialize, Serialize};

use super::{Error, Form, RealizeOptions, Sentence, realize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    And,
    But,
    Or,
    Because,
}

/// Combines declarative propositions using an explicitly supplied relationship.
///
/// Every proposition retains its subject, tense, and polarity. No cause,
/// contrast, shared subject, or entity reference is inferred from the wording.
/// For `because`, the first proposition is the effect and the second is the
/// cause. The cause is rendered first to keep negation attached to the effect.
/// Conditional propositions require an explicit grouping and are rejected.
pub fn combine(sentences: &[Sentence], relation: Relation, options: &RealizeOptions) -> Result<String, Error> {
    if sentences.len() < 2 || (relation == Relation::Because && sentences.len() != 2) {
        return Err(Error::InvalidField {
            field: "sentences",
            reason: "combine requires at least two propositions and because requires exactly two",
        });
    }
    if sentences.iter().any(|sentence| sentence.clause.form != Form::Statement) {
        return Err(Error::UnsupportedConstruction("combine requires declarative propositions"));
    }
    if sentences.iter().any(|sentence| sentence.condition.is_some()) {
        return Err(Error::UnsupportedConstruction(
            "combine requires unconditional propositions to preserve condition scope",
        ));
    }
    let separator = match relation {
        Relation::And => ", and ",
        Relation::But => ", but ",
        Relation::Or => ", or ",
        Relation::Because => {
            return Ok(format!(
                "Because {}, {}.",
                super::realization::realize_body(&sentences[1], options)?,
                super::realization::realize_body(&sentences[0], options)?,
            ));
        }
    };
    let mut result = realize(&sentences[0], options)?;
    result.pop();
    for sentence in &sentences[1..] {
        result.push_str(separator);
        result.push_str(&super::realization::realize_body(sentence, options)?);
    }
    result.push('.');
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::english::{Aspect, Clause, Condition, Number, Person, Polarity, Predicate, Subject, Tense, Verb};

    fn sentence(subject: &str, verb: &str) -> Sentence {
        Sentence {
            clause: Clause {
                subject: Subject {
                    text: subject.into(),
                    person: Person::Third,
                    number: Number::Singular,
                },
                predicate: Predicate {
                    verb: Verb::lookup(verb).unwrap(),
                    complement: None,
                },
                tense: Tense::Present,
                aspect: Aspect::Simple,
                modal: None,
                polarity: Polarity::Positive,
                form: Form::Statement,
            },
            condition: None,
        }
    }

    #[test]
    fn explicit_relations_preserve_propositions_and_names() {
        let first = sentence("the guest", "arrive");
        let mut second = sentence("Maya", "leave");
        second.clause.tense = Tense::Past;
        second.clause.polarity = Polarity::Negative;
        let sentences = [first, second];
        let original = sentences.clone();
        for (relation, expected) in [
            (Relation::And, "The guest arrives, and Maya did not leave."),
            (Relation::But, "The guest arrives, but Maya did not leave."),
            (Relation::Or, "The guest arrives, or Maya did not leave."),
            (Relation::Because, "Because Maya did not leave, the guest arrives."),
        ] {
            assert_eq!(combine(&sentences, relation, &RealizeOptions::default()).unwrap(), expected);
        }
        assert_eq!(sentences, original);
        let ids = [sentence("api_v2", "work"), sentence("The Hague", "wait")];
        assert_eq!(
            combine(&ids, Relation::And, &RealizeOptions::default()).unwrap(),
            "api_v2 works, and The Hague waits."
        );
        let mut effect = sentence("the guest", "leave");
        effect.clause.polarity = Polarity::Negative;
        effect.clause.tense = Tense::Past;
        let mut cause = sentence("Maya", "arrive");
        cause.clause.tense = Tense::Past;
        assert_eq!(
            combine(&[effect, cause], Relation::Because, &RealizeOptions::default()).unwrap(),
            "Because Maya arrived, the guest did not leave."
        );
    }

    #[test]
    fn invalid_combination_does_not_silently_change_questions_or_cause() {
        let mut clause = sentence("the guest", "arrive");
        assert!(combine(&[], Relation::And, &RealizeOptions::default()).is_err());
        assert!(combine(&[clause.clone()], Relation::And, &RealizeOptions::default()).is_err());
        assert!(
            combine(
                &[clause.clone(), clause.clone(), clause.clone()],
                Relation::Because,
                &RealizeOptions::default()
            )
            .is_err()
        );
        let mut conditional = clause.clone();
        conditional.condition = Some(Condition {
            subject: clause.clause.subject.clone(),
            predicate: clause.clause.predicate.clone(),
            polarity: Polarity::Positive,
            tense: Tense::Present,
            aspect: Aspect::Simple,
        });
        assert!(combine(&[conditional, clause.clone()], Relation::And, &RealizeOptions::default()).is_err());
        clause.clause.form = Form::Question;
        assert!(combine(&[clause.clone(), clause], Relation::And, &RealizeOptions::default()).is_err());
    }
}
