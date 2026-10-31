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
        files.push(
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
