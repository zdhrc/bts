//! Checks that a provider is consistent with the contract.
//!
//! Every provider's tests call [`check`]. It needs no credentials and runs nothing.

use super::Provider;
use bts_lang::semantics::Source;
use bts_lang::{Construct, spec::SPEC};

/// Returns every way a provider departs from the contract, or nothing when it conforms.
///
/// - Its schema id is its bundle id, and both the id and the name are present.
/// - Its definition sources have distinct names and compile with no user source.
/// - Every kind its schema lists is a language primitive, a construct root or a kind
///   it defines, and belongs to the construct it is listed under.
/// - Its guidance mentions every kind it defines, as a whole word.
/// - Every function its schema declares is named once, is not a built-in function,
///   accepts some type for each parameter and has exactly one implementation.
/// - Every implementation belongs to a declared function, runs somewhere, and each
///   helper's text defines the function the helper names.
pub fn check<P: Provider>(provider: &P) -> Vec<String> {
    let schema = provider.schema();
    let mut problems = Vec::new();
    if schema.id != P::ID {
        problems.push(format!("schema id `{}` is not the bundle id `{}`", schema.id, P::ID));
    }
    if schema.id.is_empty() || schema.name.is_empty() {
        problems.push("the schema needs an id and a name".to_owned());
    }
    for (index, definition) in schema.definitions.iter().enumerate() {
        if (schema.definitions[..index].iter()).any(|earlier| earlier.name == definition.name) {
            problems.push(format!("definition source `{}` is named twice", definition.name));
        }
    }

    let empty = Source {
        name: "<conformance>",
        text: "",
    };
    let model = match bts_lang::semantics::compile_module_with_provider(empty, &schema, true) {
        Ok(model) => model,
        Err(diags) => {
            for diag in diags {
                let located = schema.definition(diag.r#where.source);
                problems.push(match located {
                    Some(source) => diag.render(source.name, source.text),
                    None => diag.what,
                });
            }
            return problems;
        }
    };

    let listed = [
        (Construct::Node, schema.nodes),
        (Construct::Edge, schema.edges),
        (Construct::Fork, schema.forks),
        (Construct::Junction, schema.junctions),
    ];
    for (construct, kinds) in listed {
        for kind in kinds {
            let defined = (model.kind_names.get(*kind)).map(|id| model.kinds[id.0].construct);
            let primitive = (SPEC.primitives.iter())
                .find(|primitive| primitive.name == *kind)
                .map(|primitive| primitive.construct);
            let found = match (defined, primitive) {
                (Some(defined), _) => Some(defined.name()),
                (None, Some(primitive)) => Some(primitive),
                (None, None) if *kind == construct.name() => Some(construct.name()),
                (None, None) => None,
            };
            match found {
                None => problems.push(format!(
                    "{} kind `{kind}` is neither a primitive nor defined by the provider",
                    construct.name()
                )),
                Some(found) if found != construct.name() => {
                    problems.push(format!(
                        "`{kind}` is listed under {} kinds and belongs to {found} kinds",
                        construct.name()
                    ));
                }
                Some(_) => {}
            }
        }
    }

    let functions = provider.functions();
    for (index, function) in schema.functions.iter().enumerate() {
        let name = function.doc.name;
        if schema.functions[..index].iter().any(|earlier| earlier.doc.name == name) {
            problems.push(format!("function `{name}` is declared twice"));
        }
        if SPEC.function(name).is_some() {
            problems.push(format!("function `{name}` is a built-in function"));
        }
        for param in function.params {
            if param.accepts.is_empty() {
                problems.push(format!("function `{name}` accepts no type for `{}`", param.name));
            }
        }
        match functions.iter().filter(|known| known.name == name).count() {
            1 => {}
            0 => problems.push(format!("function `{name}` has no implementation")),
            _ => problems.push(format!("function `{name}` has more than one implementation")),
        }
    }
    for function in functions {
        let name = function.name;
        if !schema.functions.iter().any(|declared| declared.doc.name == name) {
            problems.push(format!("the implementation `{name}` belongs to no declared function"));
        }
        if function.data.is_none() && function.python.is_none() && function.typescript.is_none() {
            problems.push(format!("function `{name}` runs nowhere"));
        }
        let helpers = [("def", &function.python), ("function", &function.typescript)];
        for (keyword, helper) in helpers {
            if let Some(helper) = helper
                && !helper.text.contains(&format!("\n{keyword} {}(", helper.name))
            {
                problems.push(format!("the helper text of `{name}` does not define `{}`", helper.name));
            }
        }
    }

    let guidance = provider.guidance().sections().concat().to_lowercase();
    let words: Vec<&str> = guidance
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .collect();
    for kind in &model.kinds {
        let name = kind.name.to_lowercase();
        let plural = format!("{name}s");
        if !words.iter().any(|word| *word == name || *word == plural) {
            problems.push(format!("the guidance does not mention the kind `{}`", kind.name));
        }
    }
    problems
}
