//! Select generation targets and resolve their names and dependencies.

use super::assembly::Error;
use crate::dsl::{Scorer, ScorerKind, ScorerLang};
use std::collections::{HashMap, HashSet};

// braintrust function slugs: lowercase alphanumeric runs joined by hyphens
pub(crate) fn slugify(name: &str) -> Option<String> {
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

pub(crate) fn resolve_slugs(scorers: &[Scorer]) -> Result<Vec<String>, Error> {
    let mut slugs = Vec::with_capacity(scorers.len());
    let mut first_by_slug: HashMap<String, String> = HashMap::new();
    for scorer in scorers {
        let slug = slugify(&scorer.name).ok_or_else(|| Error::UnsluggableName {
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

pub(super) fn source_name(stem: &str, lang: ScorerLang) -> String {
    match lang {
        ScorerLang::Python => {
            let mut normalized = String::new();
            for ch in stem.chars() {
                if ch.is_ascii_alphanumeric() {
                    normalized.push(ch.to_ascii_lowercase());
                } else if !normalized.ends_with('_') && !normalized.is_empty() {
                    normalized.push('_');
                }
            }
            let normalized = normalized.trim_end_matches('_');
            let prefix = if normalized.starts_with(|ch: char| ch.is_ascii_digit()) {
                "_"
            } else {
                ""
            };
            format!("{prefix}{normalized}_scorer.py")
        }
        ScorerLang::Typescript => format!("{stem}.scorer.ts"),
    }
}

pub(crate) fn callable_name(stem: &str, lang: ScorerLang) -> String {
    let registration = source_name(stem, lang);
    match lang {
        ScorerLang::Python => registration.replace("_scorer.py", ".py"),
        ScorerLang::Typescript => registration.replace(".scorer.ts", ".ts"),
    }
}

pub(super) struct ScorerGroup {
    pub stem: String,
    pub members: Vec<usize>,
    pub lang: ScorerLang,
}

pub(super) fn scorer_groups(scorers: &[Scorer], slugs: &[String], lang: Option<ScorerLang>) -> Result<Vec<ScorerGroup>, Error> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, (scorer, slug)) in scorers.iter().zip(slugs).enumerate() {
        if matches!(scorer.kind, ScorerKind::Judge { .. }) {
            continue;
        }
        let stem = scorer.file.clone().unwrap_or_else(|| slug.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == stem) {
            Some((_, members)) => members.push(index),
            None => groups.push((stem, vec![index])),
        }
    }

    groups
        .into_iter()
        .map(|(stem, members)| {
            let file_lang = resolve_lang(&scorers[members[0]], lang);
            if let Some(&conflict) = members
                .iter()
                .find(|&&member| resolve_lang(&scorers[member], lang) != file_lang)
            {
                return Err(Error::PackedLangConflict {
                    file: stem,
                    first: scorers[members[0]].name.clone(),
                    second: scorers[conflict].name.clone(),
                });
            }
            Ok(ScorerGroup {
                stem,
                members,
                lang: file_lang,
            })
        })
        .collect()
}

#[allow(dead_code)]
pub(super) struct Operation {
    pub key: super::lowering::code::Dependency,
    pub reads: HashSet<super::lowering::code::Dependency>,
    pub code: Vec<super::lowering::code::Statement>,
}
#[allow(dead_code)]
pub(super) fn order(mut operations: Vec<Operation>, frame: &str) -> Result<Vec<super::lowering::code::Statement>, String> {
    let mut ordered = Vec::new();
    while !operations.is_empty() {
        let next = operations
            .iter()
            .position(|op| !operations.iter().any(|other| op.reads.contains(&other.key)));
        let Some(index) = next else {
            return Err(format!(
                "scope {frame} requires deferred field dependencies: {:?}",
                operations.iter().map(|op| (&op.key, &op.reads)).collect::<Vec<_>>()
            ));
        };
        ordered.extend(operations.remove(index).code);
    }
    Ok(ordered)
}

#[allow(dead_code)]
pub(in crate::scg) fn mentions(source: &str, symbol: &str) -> bool {
    source.match_indices(symbol).any(|(index, _)| {
        let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
        (index == 0 || !source[..index].chars().next_back().is_some_and(identifier))
            && (!source[index + symbol.len()..].chars().next().is_some_and(identifier))
    })
}
