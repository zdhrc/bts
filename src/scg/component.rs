use super::emit;
use crate::dsl::{Part, Value};

// braintrust function slugs: lowercase alphanumeric runs joined by hyphens
pub(super) fn slugify(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    let mut gap = false;
    for part in name.chars() {
        if part.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(part.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

// a judge prompt renders to braintrust's template syntax: argument-path holes
// become {{path}} slots; the modeler validated every hole is such a path
pub(super) fn mustache(prompt: &Value) -> String {
    match prompt {
        Value::Str(text) => text.clone(),
        Value::Template(template) => template
            .parts
            .iter()
            .map(|part| match part {
                Part::Lit(text) => text.clone(),
                Part::Dynamic(value) => format!("{{{{{}}}}}", slot_path(value)),
                Part::Ref(_) | Part::VarRef(_) => unreachable!("the modeler validated judge prompt holes"),
            })
            .collect(),
        _ => unreachable!("the modeler validated the judge prompt shape"),
    }
}

fn slot_path(value: &Value) -> String {
    match value {
        Value::ArgRef(arg) => emit::arg_name(*arg).to_owned(),
        Value::Index { target, index, .. } => match &**index {
            Value::Str(key) => format!("{}.{key}", slot_path(target)),
            _ => unreachable!("the modeler validated judge prompt holes"),
        },
        _ => unreachable!("the modeler validated judge prompt holes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("response-quality").as_deref(), Some("response-quality"));
        assert_eq!(slugify("Answer Quality!").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("answer_quality").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("  spaced   out  ").as_deref(), Some("spaced-out"));
        assert_eq!(slugify("v2 scorer").as_deref(), Some("v2-scorer"));
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify(""), None);
    }
}
