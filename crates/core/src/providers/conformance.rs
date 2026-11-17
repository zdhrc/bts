//! Checks that a provider is consistent with the contract.
//!
//! Every provider's tests call [`check`], and so do the tests of each pairing a host
//! selects, since a function name has one definition among the providers in use. It
//! needs no credentials and runs nothing.

use super::Contributor;
use crate::mocking::ScorerLang;
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
/// - Every implementation belongs to a declared function or is named as internal, and
///   not both. An internal one runs in data generation and in both agent languages.
///   An internal name has an implementation. A provider with no model walks nothing.
/// - Every implementation runs somewhere, and each
///   helper's text or runtime defines the function the helper names. A helper that
///   scorers call has a runtime, and the runtime defines that name on its own.
pub fn check<P: Contributor>(provider: &P) -> Vec<String> {
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
        let declared = schema.functions.iter().any(|declared| declared.doc.name == name);
        let internal = provider.internal().contains(&name);
        if !declared && !internal {
            problems.push(format!("the implementation `{name}` belongs to no declared function"));
        }
        if declared && internal {
            problems.push(format!("function `{name}` is declared and is internal"));
        }
        // nothing reports an internal function a consumer cannot run, so it runs everywhere agents do
        if internal && !(function.data.is_some() && function.python.is_some() && function.typescript.is_some()) {
            problems.push(format!(
                "the internal function `{name}` does not run in data and in both agent languages"
            ));
        }
        if function.data.is_none() && function.python.is_none() && function.typescript.is_none() {
            problems.push(format!("function `{name}` runs nowhere"));
        }
        let helpers = [
            ("def", ScorerLang::Python, &function.python),
            ("function", ScorerLang::Typescript, &function.typescript),
        ];
        for (keyword, lang, helper) in helpers {
            let Some(helper) = helper else { continue };
            let defines = |text: &str, called: &str| text.contains(&format!("\n{keyword} {called}("));
            let defined = match helper.runtime {
                Some(runtime) => defines(&(runtime.source)(lang, true), helper.name),
                None => defines(helper.text, helper.name),
            };
            if !defined {
                problems.push(format!("the helper text of `{name}` does not define `{}`", helper.name));
            }
            if let Some(scorer) = helper.scorer {
                let standalone = helper.runtime.map(|runtime| (runtime.source)(lang, false));
                if !standalone.is_some_and(|text| defines(&format!("\n{text}"), scorer)) {
                    problems.push(format!("no runtime of `{name}` defines `{scorer}` for scorers"));
                }
            }
        }
    }

    for name in provider.internal() {
        if !functions.iter().any(|function| function.name == *name) {
            problems.push(format!("the internal function `{name}` has no implementation"));
        }
    }
    if !provider.elements(&model).is_empty() {
        problems.push("the provider walks elements of a model that has none".to_owned());
    }
    if let Err(diag) = provider.supply(&model) {
        problems.push(format!("supplying an empty model fails: {}", diag.what));
    }

    // each contributor speaks for its own kinds
    let guidance: String = (provider.parts().iter())
        .map(|part| part.guidance.sections().concat().to_lowercase())
        .collect();
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
