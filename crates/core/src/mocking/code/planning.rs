//! Select generation targets and resolve their names and dependencies.

use crate::dsl::{Accessor, Model, Scorer, ScorerLang, Trace};
use std::collections::HashSet;

// Normalize names consistently before constructing language identifiers.
pub(crate) fn identifier_stem(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    let mut gap = false;
    for part in name.chars() {
        if part.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('_');
            }
            gap = false;
            slug.push(part.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

pub(crate) fn source_name(stem: &str, lang: ScorerLang) -> String {
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
        ScorerLang::Python => format!("{}_impl.py", registration.strip_suffix("_scorer.py").unwrap()),
        ScorerLang::Typescript => format!("{}.impl.ts", registration.strip_suffix(".scorer.ts").unwrap()),
    }
}

pub(crate) struct Agent<'m> {
    pub name: &'m str,
    pub file: String,
    pub trace: &'m Trace,
}

pub(crate) fn agents(model: &Model, lang: ScorerLang) -> Result<Vec<Agent<'_>>, String> {
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

pub(crate) fn agent_stem(name: &str) -> Result<String, String> {
    let stem = module_stem(name)?;
    Ok(if stem.ends_with("_agent") {
        stem
    } else {
        format!("{stem}_agent")
    })
}
pub(crate) fn module_stem(name: &str) -> Result<String, String> {
    let stem = identifier_stem(name).ok_or_else(|| format!("{name:?} needs an alphanumeric name"))?;
    Ok(if stem.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{stem}")
    } else {
        stem
    })
}

pub(crate) struct Evaluation<'m> {
    pub experiment: &'m crate::dsl::Experiment,
    pub dataset: &'m crate::dsl::Dataset,
    pub agent: String,
    pub module: String,
    pub scorers: Vec<&'m Scorer>,
}

pub(crate) fn evals(model: &Model) -> Result<Vec<Evaluation<'_>>, String> {
    let mut plans = Vec::new();
    let mut names = HashSet::new();
    for experiment in &model.experiments {
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
            .map(|name| model.scorers.iter().find(|s| &s.name == name).expect("validated scorer"))
            .collect();
        plans.push(Evaluation {
            experiment,
            dataset,
            agent: stem,
            module: eval_stem,
            scorers,
        });
    }
    let mut ordered = Vec::with_capacity(plans.len());
    let mut completed = HashSet::new();
    while !plans.is_empty() {
        let next = plans.iter().position(|plan| {
            plan.experiment
                .baseline
                .as_ref()
                .is_none_or(|baseline| completed.contains(baseline.as_str()))
        });
        let Some(index) = next else {
            return Err("generated eval baselines must be present and have no dependency cycles".into());
        };
        let plan = plans.remove(index);
        completed.insert(plan.experiment.name.as_str());
        ordered.push(plan);
    }
    Ok(ordered)
}

pub(crate) struct Operation {
    pub key: super::lowering::code::Dependency,
    pub reads: HashSet<super::lowering::code::Dependency>,
    pub code: Vec<super::lowering::code::Statement>,
}
pub(crate) fn order(mut operations: Vec<Operation>, frame: &str) -> Result<Vec<super::lowering::code::Statement>, String> {
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

pub(in crate::mocking::code) fn mentions(source: &str, symbol: &str) -> bool {
    source.match_indices(symbol).any(|(index, _)| {
        let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
        (index == 0 || !source[..index].chars().next_back().is_some_and(identifier))
            && (!source[index + symbol.len()..].chars().next().is_some_and(identifier))
    })
}
