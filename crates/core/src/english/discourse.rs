//! Explicit conversation context for references and previously established facts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Error, Number, Person};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pronouns {
    pub subject: String,
    pub object: String,
    pub possessive_determiner: String,
    pub possessive: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub id: String,
    pub label: String,
    #[serde(default = "third_person")]
    pub person: Person,
    #[serde(default = "singular")]
    pub number: Number,
    #[serde(default)]
    pub pronouns: Option<Pronouns>,
}

fn third_person() -> Person {
    Person::Third
}
fn singular() -> Number {
    Number::Singular
}

/// A caller-owned conversation snapshot. An empty value starts a fresh scope.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscourseContext {
    pub entities: Vec<Entity>,
    pub mentioned: Vec<String>,
    pub facts: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceForm {
    #[default]
    Subject,
    Object,
    PossessiveDeterminer,
    Possessive,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceStyle {
    #[default]
    Auto,
    Name,
    Pronoun,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReferenceOptions {
    pub form: ReferenceForm,
    pub style: ReferenceStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detail {
    pub id: String,
    pub text: String,
}

/// Chooses an authored name or an unambiguous, explicitly supplied pronoun.
///
/// This function does not mark an entity mentioned. Carry a context returned by
/// [`remember`] to the next turn when the entity has actually been introduced.
pub fn refer(entity: &Entity, context: &DiscourseContext, options: &ReferenceOptions) -> Result<String, Error> {
    validate_context(context)?;
    validate_entity(entity)?;
    if context.entities.iter().any(|other| other.id == entity.id && other != entity) {
        return invalid("entity", "entity id conflicts with the context definition");
    }
    let candidate = entity.pronouns.as_ref().map(|forms| pronoun(forms, options.form));
    let introduced = context.mentioned.contains(&entity.id);
    let unambiguous = candidate.is_some_and(|candidate| {
        !context.entities.iter().any(|other| {
            other.id != entity.id
                && context.mentioned.contains(&other.id)
                && other
                    .pronouns
                    .as_ref()
                    .is_none_or(|forms| pronoun(forms, options.form).eq_ignore_ascii_case(candidate))
        })
    });
    if options.style != ReferenceStyle::Name && introduced && unambiguous {
        return Ok(candidate.expect("unambiguous candidate").into());
    }
    if options.style == ReferenceStyle::Pronoun {
        return invalid(
            "reference",
            "pronoun requires an introduced entity with an unambiguous supplied form",
        );
    }
    Ok(match options.form {
        ReferenceForm::Subject | ReferenceForm::Object => entity.label.clone(),
        ReferenceForm::PossessiveDeterminer | ReferenceForm::Possessive => {
            if entity.label.ends_with(['s', 'S']) {
                format!("{}'", entity.label)
            } else {
                format!("{}'s", entity.label)
            }
        }
    })
}

/// Returns an updated context without changing the supplied snapshot.
pub fn remember(context: &DiscourseContext, entities: &[Entity], facts: &[String]) -> Result<DiscourseContext, Error> {
    validate_context(context)?;
    let mut next = context.clone();
    for entity in entities {
        validate_entity(entity)?;
        match next.entities.iter().find(|other| other.id == entity.id) {
            Some(other) if other != entity => return invalid("entity", "entity id conflicts with the context definition"),
            Some(_) => {}
            None => next.entities.push(entity.clone()),
        }
        if !next.mentioned.contains(&entity.id) {
            next.mentioned.push(entity.id.clone());
        }
    }
    for fact in facts {
        validate_text(fact, "fact id")?;
        if !next.facts.contains(fact) {
            next.facts.push(fact.clone());
        }
    }
    Ok(next)
}

/// Selects details whose stable fact identifiers are not established yet.
///
/// Identical repeated details are deduplicated in source order. Conflicting
/// text for one identifier is an error, including for already established facts.
pub fn unmentioned(details: &[Detail], context: &DiscourseContext) -> Result<Vec<Detail>, Error> {
    validate_context(context)?;
    let mut seen = BTreeMap::new();
    let mut selected = Vec::new();
    for detail in details {
        validate_text(&detail.id, "detail id")?;
        validate_text(&detail.text, "detail text")?;
        match seen.insert(&detail.id, &detail.text) {
            Some(text) if text != &detail.text => return invalid("details", "a detail id has conflicting text"),
            Some(_) => continue,
            None => {}
        }
        if !context.facts.contains(&detail.id) {
            selected.push(detail.clone());
        }
    }
    Ok(selected)
}

fn pronoun(forms: &Pronouns, form: ReferenceForm) -> &str {
    match form {
        ReferenceForm::Subject => &forms.subject,
        ReferenceForm::Object => &forms.object,
        ReferenceForm::PossessiveDeterminer => &forms.possessive_determiner,
        ReferenceForm::Possessive => &forms.possessive,
    }
}

fn validate_entity(entity: &Entity) -> Result<(), Error> {
    validate_text(&entity.id, "entity id")?;
    validate_text(&entity.label, "entity label")?;
    if let Some(forms) = &entity.pronouns {
        for form in [&forms.subject, &forms.object, &forms.possessive_determiner, &forms.possessive] {
            if form.is_empty() || !form.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return invalid("pronouns", "pronoun forms must be single English words");
            }
        }
    }
    Ok(())
}

fn validate_context(context: &DiscourseContext) -> Result<(), Error> {
    let mut entities = BTreeSet::new();
    for entity in &context.entities {
        validate_entity(entity)?;
        if !entities.insert(&entity.id) {
            return invalid("context", "entity ids must be unique");
        }
    }
    let mut mentioned = BTreeSet::new();
    for id in &context.mentioned {
        if !entities.contains(id) {
            return invalid("context", "mentioned entity has no definition");
        }
        if !mentioned.insert(id) {
            return invalid("context", "mentioned entity ids must be unique");
        }
    }
    let mut facts = BTreeSet::new();
    for fact in &context.facts {
        validate_text(fact, "fact id")?;
        if !facts.insert(fact) {
            return invalid("context", "fact ids must be unique");
        }
    }
    Ok(())
}

fn validate_text(text: &str, field: &'static str) -> Result<(), Error> {
    if text.is_empty()
        || text.trim() != text
        || text
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return invalid(
            field,
            "expected nonempty text without surrounding whitespace or control characters",
        );
    }
    Ok(())
}

fn invalid<T>(field: &'static str, reason: &'static str) -> Result<T, Error> {
    Err(Error::InvalidField { field, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maya() -> Entity {
        Entity {
            id: "customer-42".into(),
            label: "Maya".into(),
            person: Person::Third,
            number: Number::Singular,
            pronouns: Some(Pronouns {
                subject: "she".into(),
                object: "her".into(),
                possessive_determiner: "her".into(),
                possessive: "hers".into(),
            }),
        }
    }

    #[test]
    fn references_follow_explicit_context_and_reset() {
        let entity = maya();
        let empty = DiscourseContext::default();
        assert_eq!(refer(&entity, &empty, &ReferenceOptions::default()).unwrap(), "Maya");
        let context = remember(&empty, std::slice::from_ref(&entity), &["arrival-time".into()]).unwrap();
        assert_eq!(refer(&entity, &context, &ReferenceOptions::default()).unwrap(), "she");
        assert_eq!(
            refer(
                &entity,
                &context,
                &ReferenceOptions {
                    form: ReferenceForm::Possessive,
                    style: ReferenceStyle::Auto
                }
            )
            .unwrap(),
            "hers"
        );
        assert_eq!(refer(&entity, &empty, &ReferenceOptions::default()).unwrap(), "Maya");
        assert_eq!(empty, DiscourseContext::default());
    }

    #[test]
    fn ambiguous_pronouns_fall_back_to_names_without_inferred_gender() {
        let entity = maya();
        let other = Entity {
            id: "agent-7".into(),
            label: "Amina".into(),
            ..entity.clone()
        };
        let context = remember(&DiscourseContext::default(), &[entity.clone(), other], &[]).unwrap();
        assert_eq!(refer(&entity, &context, &ReferenceOptions::default()).unwrap(), "Maya");
        assert!(
            refer(
                &entity,
                &context,
                &ReferenceOptions {
                    style: ReferenceStyle::Pronoun,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let unnamed = Entity {
            pronouns: None,
            ..entity.clone()
        };
        let context = remember(&DiscourseContext::default(), std::slice::from_ref(&unnamed), &[]).unwrap();
        assert_eq!(refer(&unnamed, &context, &ReferenceOptions::default()).unwrap(), "Maya");
        let unknown = Entity {
            id: "unknown".into(),
            label: "the other guest".into(),
            pronouns: None,
            ..entity.clone()
        };
        let context = remember(&DiscourseContext::default(), &[entity.clone(), unknown], &[]).unwrap();
        assert_eq!(refer(&entity, &context, &ReferenceOptions::default()).unwrap(), "Maya");
    }

    #[test]
    fn context_conflicts_and_unknown_references_are_errors() {
        let entity = maya();
        let context = remember(&DiscourseContext::default(), std::slice::from_ref(&entity), &[]).unwrap();
        let changed = Entity {
            label: "different identity".into(),
            ..entity.clone()
        };
        assert!(remember(&context, std::slice::from_ref(&changed), &[]).is_err());
        assert!(refer(&changed, &context, &ReferenceOptions::default()).is_err());
        assert_eq!(context.entities.as_slice(), std::slice::from_ref(&entity));
        let invalid = DiscourseContext {
            mentioned: vec!["missing".into()],
            ..Default::default()
        };
        assert!(refer(&entity, &invalid, &ReferenceOptions::default()).is_err());
        assert_eq!(remember(&context, &[entity.clone(), entity], &[]).unwrap(), context);
    }

    #[test]
    fn detail_selection_preserves_order_and_detects_conflicts() {
        let details = vec![
            Detail {
                id: "arrival-time".into(),
                text: "after midnight".into(),
            },
            Detail {
                id: "booking".into(),
                text: "ZX-42".into(),
            },
        ];
        let context = remember(&DiscourseContext::default(), &[], &["arrival-time".into()]).unwrap();
        assert_eq!(unmentioned(&details, &context).unwrap(), [details[1].clone()]);
        assert_eq!(unmentioned(&details, &DiscourseContext::default()).unwrap(), details);
        assert_eq!(
            unmentioned(&[details[1].clone(), details[1].clone()], &context).unwrap(),
            [details[1].clone()]
        );
        assert!(
            unmentioned(
                &[
                    details[0].clone(),
                    Detail {
                        id: "arrival-time".into(),
                        text: "before noon".into()
                    }
                ],
                &context
            )
            .is_err()
        );
    }

    #[test]
    fn names_and_explicit_pronoun_forms_keep_authored_spelling() {
        let entity = Entity {
            label: "Chris".into(),
            ..maya()
        };
        let options = ReferenceOptions {
            form: ReferenceForm::PossessiveDeterminer,
            style: ReferenceStyle::Name,
        };
        assert_eq!(refer(&entity, &DiscourseContext::default(), &options).unwrap(), "Chris'");
        let entity = Entity {
            label: "José".into(),
            ..entity
        };
        assert_eq!(refer(&entity, &DiscourseContext::default(), &options).unwrap(), "José's");
        assert!(serde_json::from_str::<DiscourseContext>(r#"{"hidden_state":true}"#).is_err());
    }
}
