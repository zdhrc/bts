use super::contracts::{Assembly, CodeGroup, Emitter, Item, Judge, Target};
use crate::dsl::{Scorer, ScorerKind, ScorerLang};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub fn resolve_slugs(scorers: &[Scorer], target: &(impl Target + ?Sized)) -> Result<Vec<String>, Error> {
    let mut slugs = Vec::with_capacity(scorers.len());
    let mut first_by_slug: HashMap<String, String> = HashMap::new();
    for scorer in scorers {
        let slug = target.slug(&scorer.name).ok_or_else(|| Error::UnsluggableName {
            name: scorer.name.clone(),
        })?;
        if let Some(first) = first_by_slug.insert(slug.clone(), scorer.name.clone()) {
            return Err(Error::DuplicateSlug {
                slug,
                first,
                second: scorer.name.clone(),
            });
        }
        slugs.push(slug);
    }
    Ok(slugs)
}

fn resolve_lang(scorer: &Scorer, lang: Option<ScorerLang>) -> ScorerLang {
    lang.or(scorer.lang).unwrap_or(ScorerLang::Python)
}

/// Assembles scorer source using the supplied emitter and integration target.
///
/// Checks export counts, blank names, and duplicate names before passing each
/// module to the target. The emitter is responsible for valid source and for
/// returning function identifiers in the same order as the input items.
pub fn assemble<E: Emitter + ?Sized, T: Target + ?Sized>(
    scorers: &[Scorer],
    lang: Option<ScorerLang>,
    emitter: &E,
    target: &T,
) -> Result<Assembly, AssemblyError<E::Error, T::Error>> {
    let slugs = resolve_slugs(scorers, target).map_err(AssemblyError::Plan)?;
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, (scorer, slug)) in scorers.iter().zip(&slugs).enumerate() {
        if matches!(scorer.kind, ScorerKind::Judge { .. }) {
            continue;
        }
        let stem = scorer.file.clone().unwrap_or_else(|| slug.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == stem) {
            Some((_, members)) => members.push(index),
            None => groups.push((stem, vec![index])),
        }
    }

    let mut files = Vec::new();
    for (stem, members) in groups {
        let file_lang = resolve_lang(&scorers[members[0]], lang);
        if let Some(&conflict) = members
            .iter()
            .find(|&&member| resolve_lang(&scorers[member], lang) != file_lang)
        {
            return Err(AssemblyError::Plan(Error::PackedLangConflict {
                file: stem,
                first: scorers[members[0]].name.clone(),
                second: scorers[conflict].name.clone(),
            }));
        }
        let items: Vec<Item> = members
            .iter()
            .map(|&member| {
                let ScorerKind::Code { score, steps } = &scorers[member].kind else {
                    unreachable!("judges were filtered out")
                };
                Item {
                    name: &scorers[member].name,
                    slug: &slugs[member],
                    score,
                    steps,
                }
            })
            .collect();
        let module = emitter.emit(file_lang, &items).map_err(AssemblyError::Emit)?;
        if module.functions.len() != items.len() {
            return Err(AssemblyError::Plan(Error::ExportCount {
                expected: items.len(),
                actual: module.functions.len(),
            }));
        }
        let mut exports = HashSet::with_capacity(module.functions.len());
        for (index, function) in module.functions.iter().enumerate() {
            if function.trim().is_empty() {
                return Err(AssemblyError::Plan(Error::EmptyExport { index }));
            }
            if !exports.insert(function) {
                return Err(AssemblyError::Plan(Error::DuplicateExport {
                    function: function.clone(),
                }));
            }
        }
        files.extend(
            target
                .code(
                    CodeGroup {
                        lang: file_lang,
                        stem: &stem,
                        items: &items,
                    },
                    module,
                )
                .map_err(AssemblyError::Target)?,
        );
    }

    for (scorer, slug) in scorers.iter().zip(&slugs) {
        if let ScorerKind::Judge { model, prompt, options } = &scorer.kind {
            files.push(
                target
                    .judge(Judge {
                        lang: lang.unwrap_or(ScorerLang::Python),
                        name: &scorer.name,
                        slug,
                        model,
                        prompt,
                        options,
                    })
                    .map_err(AssemblyError::Target)?,
            );
        }
    }
    let mut seen = HashSet::new();
    for file in &files {
        if !seen.insert(&file.name) {
            return Err(AssemblyError::Plan(Error::DuplicateFile { file: file.name.clone() }));
        }
    }
    Ok(Assembly { files })
}

#[derive(Debug)]
pub enum AssemblyError<E, T> {
    Plan(Error),
    Emit(E),
    Target(T),
}

impl<E: fmt::Display, T: fmt::Display> fmt::Display for AssemblyError<E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(source) => source.fmt(formatter),
            Self::Emit(source) => source.fmt(formatter),
            Self::Target(source) => source.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static, T: std::error::Error + 'static> std::error::Error for AssemblyError<E, T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(source) => Some(source),
            Self::Emit(source) => Some(source),
            Self::Target(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    UnsluggableName { name: String },
    DuplicateSlug { slug: String, first: String, second: String },
    PackedLangConflict { file: String, first: String, second: String },
    DuplicateFile { file: String },
    ExportCount { expected: usize, actual: usize },
    EmptyExport { index: usize },
    DuplicateExport { function: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsluggableName { name } => {
                write!(formatter, "scorer name \"{name}\" has no alphanumeric characters to slug")
            }
            Self::DuplicateSlug { slug, first, second } => {
                write!(formatter, "scorers \"{first}\" and \"{second}\" both use slug `{slug}`")
            }
            Self::PackedLangConflict { file, first, second } => write!(
                formatter,
                "scorers \"{first}\" and \"{second}\" pack into file `{file}` but resolve to different languages"
            ),
            Self::DuplicateFile { file } => write!(formatter, "multiple scorers would build into `{file}`"),
            Self::ExportCount { expected, actual } => {
                write!(formatter, "emitter exported {actual} functions for {expected} scorers")
            }
            Self::EmptyExport { index } => write!(formatter, "emitter returned a blank function identifier at index {index}"),
            Self::DuplicateExport { function } => {
                write!(formatter, "emitter returned duplicate function identifier `{function}`")
            }
        }
    }
}

impl std::error::Error for Error {}

use super::{lowering, planning};

pub fn assemble_agents(
    model: &crate::dsl::Model,
    lang: ScorerLang,
    target: &(impl super::AgentTarget + ?Sized),
) -> Result<Assembly, String> {
    let mut sources = Vec::new();
    let mut types = Vec::new();
    for agent in planning::agents(model, lang)? {
        let function = lowering::agent(model, agent.trace, lang).map_err(|e| format!("agent {:?}: {e}", agent.name))?;
        let path = std::path::Path::new(&agent.file);
        let module = format!("{}_types", path.file_stem().unwrap().to_string_lossy());
        let (contents, definitions) = super::languages::rendering::agent(&function, lang, &module, target.tracing(lang));
        if let Some(contents) = definitions {
            let name = path.with_file_name(format!("{module}.{}", path.extension().unwrap().to_string_lossy()));
            types.push(super::SourceFile {
                slugs: Vec::new(),
                lang,
                name: name.to_string_lossy().into_owned(),
                contents,
            });
        }
        sources.push(super::SourceFile {
            slugs: Vec::new(),
            lang,
            name: agent.file,
            contents,
        });
    }
    separate_runtime(&mut sources, lang);
    sources.extend(types);
    Ok(Assembly { files: sources })
}

fn separate_runtime(sources: &mut Vec<super::SourceFile>, lang: ScorerLang) {
    let marker = match lang {
        ScorerLang::Python => "_MISSING = object()",
        ScorerLang::Typescript => "const _MISSING = Symbol",
    };
    let mut runtimes = Vec::with_capacity(sources.len());
    for source in sources.iter_mut() {
        let index = source.contents.find(marker).expect("agent runtime boundary");
        let helpers = source.contents[..index].to_owned();
        let body = source.contents[index..].to_owned();
        let path = std::path::Path::new(&source.name);
        let module = format!("{}_runtime", path.file_stem().unwrap().to_string_lossy());
        let runtime_path = path.with_file_name(format!("{module}.{}", path.extension().unwrap().to_string_lossy()));
        let mut symbols = helpers
            .lines()
            .filter_map(|line| {
                ["def ", "function ", "class ", "const "]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                    .map(|tail| {
                        tail.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                            .next()
                            .unwrap()
                            .to_owned()
                    })
            })
            .filter(|name| planning::mentions(&body, name))
            .collect::<Vec<_>>();
        if lang == ScorerLang::Python {
            symbols.push("_random".into());
        }
        symbols.sort();
        symbols.dedup();
        let names = symbols.join(", ");
        let imports = match lang {
            ScorerLang::Python => format!(
                "# Generated by bts; edits will be overwritten.\nif __package__:\n    from .{module} import {names}\nelse:\n    from {module} import {names}\n\n"
            ),
            ScorerLang::Typescript => {
                format!("// Generated by bts; edits will be overwritten.\nimport {{{names}}} from \"./{module}.mts\";\n\n")
            }
        };
        source.contents = format!("{imports}{body}");
        let mut runtime = needed_helpers(&helpers, &source.contents);
        if lang == ScorerLang::Typescript {
            runtime = runtime
                .lines()
                .map(|line| {
                    if ["function ", "class ", "const "]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
                    {
                        format!("export {line}\n")
                    } else {
                        format!("{line}\n")
                    }
                })
                .collect();
        }
        runtimes.push(super::SourceFile {
            slugs: Vec::new(),
            lang,
            name: runtime_path.to_string_lossy().into_owned(),
            contents: runtime,
        });
    }
    sources.extend(runtimes);
}

fn needed_helpers(runtime: &str, source: &str) -> String {
    let mut preamble = String::new();
    let mut definitions: Vec<(String, String)> = Vec::new();
    for line in runtime.lines() {
        if line.starts_with("import ") || line.starts_with("from ") || line.starts_with("const ") {
            preamble.push_str(line);
            preamble.push('\n');
            continue;
        }
        let name = ["def ", "function ", "class "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(|tail| tail.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap());
        if let Some(name) = name {
            definitions.push((name.into(), String::new()));
        }
        if let Some((_, body)) = definitions.last_mut() {
            body.push_str(line);
            body.push('\n');
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    let mut used = HashSet::new();
    let mut references = source.to_owned();
    loop {
        let next = definitions
            .iter()
            .filter(|(name, _)| !used.contains(name) && planning::mentions(&references, name))
            .cloned()
            .collect::<Vec<_>>();
        if next.is_empty() {
            break;
        }
        for (name, body) in next {
            used.insert(name);
            references.push_str(&body);
        }
    }
    for (name, body) in definitions {
        if used.contains(&name) {
            preamble.push_str(&body);
        }
    }
    preamble
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::providers::braintrust::mocking::code::CodeTarget;

    #[test]
    fn selected_agents_keep_independent_runtimes() {
        let model = compile(
            r#"
trace "count" { output = tokens("hello") }
trace "plain" { output = "hello" }
agent "count" { entrypoint = trace.count }
agent "plain" { entrypoint = trace.plain }
"#,
        )
        .unwrap();
        let target = CodeTarget::new("");
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let mut combined = std::collections::BTreeMap::new();
            for name in ["count", "plain"] {
                let selected = crate::selection::agents(&model, &[format!("agent.{name}").parse().unwrap()]).unwrap();
                let built = assemble_agents(&selected, lang, &target).unwrap();
                let extension = if lang == ScorerLang::Python { "py" } else { "mts" };
                let runtime_name = format!("src/agents/{name}_agent_runtime.{extension}");
                let runtime = built.files.iter().find(|file| file.name == runtime_name).unwrap();
                assert_eq!(runtime.contents.contains("_tokens("), name == "count");
                let source = built
                    .files
                    .iter()
                    .find(|file| file.name == format!("src/agents/{name}_agent.{extension}"))
                    .unwrap();
                assert!(source.contents.contains(&format!("{name}_agent_runtime")));
                for file in built.files {
                    assert!(combined.insert(file.name, file.contents).is_none());
                }
            }
            let together = assemble_agents(&model, lang, &target).unwrap();
            assert_eq!(
                combined,
                together.files.into_iter().map(|file| (file.name, file.contents)).collect()
            );
        }
    }

    #[test]
    fn validates_agent_names_and_language_integer_limits() {
        let target = CodeTarget::new("");
        for source in [
            r#"agent "a-b" { entrypoint = trace.t } agent "a_b" { entrypoint = trace.t } trace "t" { output = "ok" }"#,
            r#"trace "t" { output = "ok" }"#,
        ] {
            for lang in [ScorerLang::Python, ScorerLang::Typescript] {
                assert!(assemble_agents(&compile(source).unwrap(), lang, &target).is_err());
            }
        }
        let model = compile(r#"agent "a" { entrypoint = trace.t } trace "t" { output = 9007199254740993 }"#).unwrap();
        assert!(assemble_agents(&model, ScorerLang::Python, &target).is_ok());
        assert!(
            assemble_agents(&model, ScorerLang::Typescript, &target)
                .err()
                .unwrap()
                .contains("safe integer")
        );
    }
}
