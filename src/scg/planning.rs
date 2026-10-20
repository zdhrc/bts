//! Select generation targets and resolve their names and dependencies.

use super::assembly::Error;
use crate::dsl::{Accessor, Model, Scorer, ScorerKind, ScorerLang, Trace};
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

pub(super) struct Agent<'m> {
    pub name: &'m str,
    pub file: String,
    pub trace: &'m Trace,
}

pub(super) fn agents(model: &Model, lang: ScorerLang) -> Result<Vec<Agent<'_>>, String> {
    if model.agents.is_empty() {
        return Err("the shape declares no agent blocks".into());
    }
    let mut names = HashSet::new();
    model
        .agents
        .iter()
        .map(|agent| {
            let stem = agent_stem(&agent.name)?;
            let extension = if lang == ScorerLang::Python { "py" } else { "mts" };
            let file = format!("src/agents/{stem}.{extension}");
            if !names.insert(file.clone()) {
                return Err(format!("multiple agents generate {file}"));
            }
            let Accessor::Block { node, .. } = model.refs[agent.entrypoint.0 as usize].accessor else {
                unreachable!("validated agent entrypoint")
            };
            let trace = model.traces.iter().find(|trace| trace.node == node).expect("validated trace");
            Ok(Agent {
                name: &agent.name,
                file,
                trace,
            })
        })
        .collect()
}

pub(super) fn agent_stem(name: &str) -> Result<String, String> {
    let stem = module_stem(name)?;
    Ok(if stem.ends_with("_agent") {
        stem
    } else {
        format!("{stem}_agent")
    })
}
pub(super) fn module_stem(name: &str) -> Result<String, String> {
    let slug = crate::scg::slugify(name).ok_or_else(|| format!("{name:?} needs an alphanumeric name"))?;
    let stem = slug.replace('-', "_");
    Ok(if stem.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{stem}")
    } else {
        stem
    })
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

pub(super) struct Evaluation<'m> {
    pub experiment: &'m crate::dsl::Experiment,
    pub dataset: &'m crate::dsl::Dataset,
    pub agent: String,
    pub module: String,
    pub scorers: Vec<&'m Scorer>,
}

pub(super) fn evals(model: &Model) -> Result<Vec<Evaluation<'_>>, String> {
    let mut plans = Vec::new();
    let mut names = HashSet::new();
    for experiment in &model.experiments {
        if experiment.baseline.is_some() {
            return Err("generated eval baselines are not supported yet".into());
        }
        let task = &model.refs[experiment.task.0 as usize].accessor;
        let matches = model
            .agents
            .iter()
            .filter(|agent| {
                matches!((task, &model.refs[agent.entrypoint.0 as usize].accessor),
                (Accessor::Block {node: a, ..}, Accessor::Block {node: b, ..}) if a == b)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "experiment {:?} must target exactly one agent's trace entrypoint",
                experiment.name
            ));
        }
        let stem = module_stem(&matches[0].name)?;
        let stem = if stem.ends_with("_agent") {
            stem
        } else {
            format!("{stem}_agent")
        };
        let eval_stem = module_stem(&experiment.name)?;
        if !names.insert(eval_stem.clone()) {
            return Err(format!("multiple experiments generate {eval_stem}"));
        }
        let dataset = model
            .datasets
            .iter()
            .find(|d| d.name == experiment.dataset)
            .expect("validated dataset");
        let scorers = experiment
            .scorers
            .iter()
            .map(|name| {
                let scorer = model.scorers.iter().find(|s| &s.name == name).expect("validated scorer");
                if !matches!(scorer.kind, ScorerKind::Code { .. }) {
                    return Err(format!("generated evals support code scorers only; {name:?} is a judge"));
                }
                Ok(scorer)
            })
            .collect::<Result<Vec<_>, String>>()?;
        plans.push(Evaluation {
            experiment,
            dataset,
            agent: stem,
            module: eval_stem,
            scorers,
        });
    }
    Ok(plans)
}

pub(super) struct Operation {
    pub key: super::lowering::code::Dependency,
    pub reads: HashSet<super::lowering::code::Dependency>,
    pub code: Vec<super::lowering::code::Statement>,
}
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

pub(in crate::scg) fn mentions(source: &str, symbol: &str) -> bool {
    source.match_indices(symbol).any(|(index, _)| {
        let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
        (index == 0 || !source[..index].chars().next_back().is_some_and(identifier))
            && (!source[index + symbol.len()..].chars().next().is_some_and(identifier))
    })
}
