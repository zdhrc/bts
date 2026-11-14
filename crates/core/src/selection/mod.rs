pub mod filter;

use crate::dsl::{self, Accessor, Automation, AutomationKind, Dataset, Model};
use std::{collections::BTreeSet, str::FromStr};

#[derive(Debug, Clone)]
pub struct ResourceSelector {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationType {
    Scorers,
    Topics,
}

impl AutomationType {
    fn matches(self, automation: &Automation) -> bool {
        matches!(
            (self, &automation.kind),
            (Self::Scorers, AutomationKind::Scorer { .. }) | (Self::Topics, AutomationKind::Topics { .. })
        )
    }
}

/// A selector that could not be parsed or does not name a supported resource.
#[derive(Debug)]
pub enum ParseError {
    Syntax(dsl::Diags),
    Expected(&'static str),
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(diags) => f.write_str(&diags.iter().map(|d| d.what.as_str()).collect::<Vec<_>>().join("\n")),
            Self::Expected(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for ParseError {}

impl FromStr for ResourceSelector {
    type Err = ParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let mut path = dsl::syntax::parse_traversal(source).map_err(ParseError::Syntax)?;
        if path.len() != 2 {
            return Err(ParseError::Expected("a named resource traversal without field access"));
        }
        let name = path.pop().expect("two traversal segments");
        let kind = path.pop().expect("two traversal segments");
        if !matches!(
            kind.as_str(),
            "agent" | "scorer" | "facet" | "automation" | "dataset" | "experiment"
        ) || name.is_empty()
        {
            return Err(ParseError::Expected(
                "a named agent, scorer, facet, automation, dataset, or experiment",
            ));
        }
        Ok(Self { kind, name })
    }
}

/// A resource selection that cannot be resolved against the model or saved state.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Error {
    Incompatible {
        kind: String,
        target: &'static str,
    },
    Unknown {
        kind: String,
        name: String,
    },
    NoAutomations {
        kind: String,
        name: String,
        include_saved: bool,
    },
    NoAgent {
        experiment: String,
    },
    AmbiguousAgents {
        experiment: String,
        agents: Vec<String>,
    },
    ArtifactCollision {
        kind: &'static str,
        first: String,
        second: String,
        stem: String,
    },
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incompatible { kind, target } => write!(formatter, "{kind} is not compatible with {target}"),
            Self::Unknown { kind, name } => write!(formatter, "unknown {kind} {name:?}"),
            Self::NoAgent { experiment } => {
                write!(
                    formatter,
                    "experiment {experiment:?} does not target an agent's trace entrypoint"
                )
            }
            Self::AmbiguousAgents { experiment, agents } => write!(
                formatter,
                "experiment {experiment:?} matches multiple agents ({}), select one matching agent explicitly",
                agents.join(", ")
            ),
            Self::ArtifactCollision {
                kind,
                first,
                second,
                stem,
            } => write!(formatter, "{kind} {first:?} and {second:?} both generate module {stem:?}"),
            Self::NoAutomations {
                kind,
                name,
                include_saved,
            } => {
                let scope = if *include_saved {
                    "local or previously synced automations"
                } else {
                    "automations"
                };
                write!(formatter, "{kind} {name:?} matches no {scope} of this type")
            }
        }
    }
}
impl std::error::Error for Error {}

/// Selects agents and their evaluation dependencies while preserving resolved trace references.
///
/// Agent selectors include evaluations of their entrypoints. Experiment selectors
/// include their agents and baseline chains, without adding unrelated evaluations.
/// An explicitly selected agent can disambiguate an entrypoint shared by several agents.
/// Empty selectors retain the full model. Traces, scorer definitions, bindings,
/// and reference indexes remain unchanged so generation can resolve source traces
/// and preserve scorers packed into the same output module.
pub fn agents(model: &Model, selectors: &[ResourceSelector]) -> Result<Model, Error> {
    if selectors.is_empty() {
        return Ok(model.clone());
    }
    let mut selected_agents = BTreeSet::new();
    let mut selected_experiments = BTreeSet::new();
    for selector in selectors {
        let selected = match selector.kind.as_str() {
            "agent" => (
                model.agents.iter().position(|agent| agent.name == selector.name),
                &mut selected_agents,
            ),
            "experiment" => (
                model
                    .experiments
                    .iter()
                    .position(|experiment| experiment.name == selector.name),
                &mut selected_experiments,
            ),
            _ => {
                return Err(Error::Incompatible {
                    kind: selector.kind.clone(),
                    target: "agent builds",
                });
            }
        };
        let index = selected.0.ok_or_else(|| Error::Unknown {
            kind: selector.kind.clone(),
            name: selector.name.clone(),
        })?;
        selected.1.insert(index);
    }
    let matches = |experiment: &dsl::Experiment, agent: &dsl::Agent| {
        matches!(
            (&model.refs[experiment.task.0 as usize].accessor, &model.refs[agent.entrypoint.0 as usize].accessor),
            (Accessor::Block { node: task, .. }, Accessor::Block { node: entrypoint, .. }) if task == entrypoint
        )
    };
    for (index, experiment) in model.experiments.iter().enumerate() {
        if selected_agents.iter().any(|&agent| matches(experiment, &model.agents[agent])) {
            selected_experiments.insert(index);
        }
    }
    let explicit_agents = selected_agents.clone();
    let mut pending = selected_experiments.iter().copied().collect::<Vec<_>>();
    while let Some(index) = pending.pop() {
        let experiment = &model.experiments[index];
        let candidates = model
            .agents
            .iter()
            .enumerate()
            .filter_map(|(index, agent)| matches(experiment, agent).then_some(index))
            .collect::<Vec<_>>();
        let explicit = candidates
            .iter()
            .copied()
            .filter(|index| explicit_agents.contains(index))
            .collect::<Vec<_>>();
        let candidates = if explicit.is_empty() { &candidates } else { &explicit };
        match candidates.as_slice() {
            [agent] => {
                selected_agents.insert(*agent);
            }
            [] => {
                return Err(Error::NoAgent {
                    experiment: experiment.name.clone(),
                });
            }
            _ => {
                return Err(Error::AmbiguousAgents {
                    experiment: experiment.name.clone(),
                    agents: candidates.iter().map(|&index| model.agents[index].name.clone()).collect(),
                });
            }
        }
        if let Some(baseline) = &experiment.baseline {
            let index = model
                .experiments
                .iter()
                .position(|experiment| &experiment.name == baseline)
                .ok_or_else(|| Error::Unknown {
                    kind: "experiment".into(),
                    name: baseline.clone(),
                })?;
            if selected_experiments.insert(index) {
                pending.push(index);
            }
        }
    }
    let agent_names = model.agents.iter().map(|agent| agent.name.as_str()).collect::<Vec<_>>();
    let experiment_names = model
        .experiments
        .iter()
        .map(|experiment| experiment.name.as_str())
        .collect::<Vec<_>>();
    for (kind, names, selected, stem) in [
        (
            "agent",
            &agent_names,
            &selected_agents,
            crate::mocking::code::planning::agent_stem as fn(&str) -> Result<String, String>,
        ),
        (
            "experiment",
            &experiment_names,
            &selected_experiments,
            crate::mocking::code::planning::module_stem,
        ),
    ] {
        for &index in selected {
            let Ok(selected_stem) = stem(names[index]) else {
                continue;
            };
            if let Some((_, name)) = names
                .iter()
                .enumerate()
                .find(|(other, name)| *other != index && stem(name).as_ref() == Ok(&selected_stem))
            {
                return Err(Error::ArtifactCollision {
                    kind,
                    first: names[index].into(),
                    second: (*name).into(),
                    stem: selected_stem,
                });
            }
        }
    }
    let mut selected = model.clone();
    selected.agents = selected_agents.into_iter().map(|index| model.agents[index].clone()).collect();
    selected.experiments = selected_experiments
        .into_iter()
        .map(|index| model.experiments[index].clone())
        .collect();
    let datasets = selected
        .experiments
        .iter()
        .map(|experiment| &experiment.dataset)
        .collect::<BTreeSet<_>>();
    selected.datasets.retain(|dataset| datasets.contains(&dataset.name));
    Ok(selected)
}

pub fn automations<'a>(
    model: &'a Model,
    kind: AutomationType,
    selectors: &[ResourceSelector],
) -> Result<Vec<&'a Automation>, Error> {
    let applicable = || model.automations.iter().filter(|automation| kind.matches(automation));
    if selectors.is_empty() {
        return Ok(applicable().collect());
    }
    let mut selected = BTreeSet::new();
    for selector in selectors {
        let exists = match selector.kind.as_str() {
            "automation" => model.automations.iter().any(|automation| automation.name == selector.name),
            "scorer" if kind == AutomationType::Scorers => model.scorers.iter().any(|scorer| scorer.name == selector.name),
            "facet" if kind == AutomationType::Topics => model.facets.iter().any(|facet| facet.name == selector.name),
            _ => {
                return Err(Error::Incompatible {
                    kind: selector.kind.clone(),
                    target: "this automation type",
                });
            }
        };
        if !exists {
            return Err(Error::Unknown {
                kind: selector.kind.clone(),
                name: selector.name.clone(),
            });
        }
        let mut matched = false;
        for (index, automation) in model
            .automations
            .iter()
            .enumerate()
            .filter(|(_, automation)| kind.matches(automation))
        {
            let matches = match (&automation.kind, selector.kind.as_str()) {
                (_, "automation") => automation.name == selector.name,
                (AutomationKind::Scorer { scorers, .. }, "scorer") => scorers.contains(&selector.name),
                (AutomationKind::Topics { facets }, "facet") => facets.contains(&selector.name),
                _ => false,
            };
            if matches {
                selected.insert(index);
                matched = true;
            }
        }
        if !matched {
            return Err(Error::NoAutomations {
                kind: selector.kind.clone(),
                name: selector.name.clone(),
                include_saved: false,
            });
        }
    }
    Ok(selected.into_iter().map(|index| &model.automations[index]).collect())
}

pub fn datasets<'a>(model: &'a Model, selectors: &[ResourceSelector]) -> Result<Vec<&'a Dataset>, Error> {
    if selectors.is_empty() {
        return Ok(model.datasets.iter().collect());
    }
    let mut selected = BTreeSet::new();
    for selector in selectors {
        if selector.kind != "dataset" {
            return Err(Error::Incompatible {
                kind: selector.kind.clone(),
                target: "dataset sync",
            });
        }
        let index = model
            .datasets
            .iter()
            .position(|dataset| dataset.name == selector.name)
            .ok_or_else(|| Error::Unknown {
                kind: "dataset".to_owned(),
                name: selector.name.clone(),
            })?;
        selected.insert(index);
    }
    Ok(selected.into_iter().map(|index| &model.datasets[index]).collect())
}

// old names still work when selecting something to remove
pub fn automation_names(
    kind: AutomationType,
    known: &[(String, Vec<String>)],
    selectors: &[ResourceSelector],
) -> Result<BTreeSet<String>, Error> {
    let mut names = BTreeSet::new();
    if selectors.is_empty() {
        names.extend(known.iter().map(|(name, _)| name.clone()));
    }
    for selector in selectors {
        let dependency = match kind {
            AutomationType::Topics => "facet",
            AutomationType::Scorers => "scorer",
        };
        if selector.kind != "automation" && selector.kind != dependency {
            return Err(Error::Incompatible {
                kind: selector.kind.clone(),
                target: "this automation type",
            });
        }
        let matched: Vec<_> = known
            .iter()
            .filter(|(name, deps)| {
                if selector.kind == "automation" {
                    *name == selector.name
                } else {
                    deps.contains(&selector.name)
                }
            })
            .collect();
        if matched.is_empty() {
            return Err(Error::NoAutomations {
                kind: selector.kind.clone(),
                name: selector.name.clone(),
                include_saved: true,
            });
        }
        names.extend(matched.iter().map(|(name, _)| name.clone()));
    }
    Ok(names)
}

pub fn dataset_names(known: &BTreeSet<String>, selectors: &[ResourceSelector]) -> Result<BTreeSet<String>, Error> {
    if selectors.is_empty() {
        return Ok(known.clone());
    }
    let mut names = BTreeSet::new();
    for selector in selectors {
        if selector.kind != "dataset" {
            return Err(Error::Incompatible {
                kind: selector.kind.clone(),
                target: "dataset sync",
            });
        }
        if !known.contains(&selector.name) {
            return Err(Error::Unknown {
                kind: "dataset".to_owned(),
                name: selector.name.clone(),
            });
        }
        names.insert(selector.name.clone());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Model {
        let mut model = crate::providers::braintrust::mocking::compile(
            r#"
(brand:scorer) {score: 1};
(quality:scorer) {score: 1};
(one:case) {input: "hello"};
(two:case) {input: "world"};
(first:dataset) {cases: [|(one)|]};
(second:dataset) {cases: [|(two)|]};
"#,
        )
        .unwrap();
        model.facets = ["Churn risk", "Intent"]
            .into_iter()
            .map(|name| crate::dsl::Facet {
                name: name.into(),
                prompt: "Extract.".into(),
                description: None,
                no_match_pattern: None,
            })
            .collect();
        model.automations = vec![
            crate::dsl::Automation {
                name: "shared".into(),
                kind: AutomationKind::Scorer {
                    scorers: vec!["brand".into(), "quality".into()],
                    root: true,
                    span_names: vec![],
                },
                sampling_rate: 1.0,
                enabled: true,
            },
            crate::dsl::Automation {
                name: "other".into(),
                kind: AutomationKind::Scorer {
                    scorers: vec!["quality".into()],
                    root: true,
                    span_names: vec![],
                },
                sampling_rate: 1.0,
                enabled: true,
            },
            crate::dsl::Automation {
                name: "topics".into(),
                kind: AutomationKind::Topics {
                    facets: vec!["Churn risk".into(), "Intent".into()],
                },
                sampling_rate: 1.0,
                enabled: true,
            },
        ];
        model
    }

    const AGENTS: &str = r#"(source_root:task {name: "source"}) {input: source_node.input, output: "answer", identifier: range (1 , 100)};
(source_node:agent) {input: {id: source_root.identifier}, output: source_root.output};
(source_node)-[:contains]->(source_root);
(source_node)-[:next]->(source_root);
(oldest_root:task {name: "oldest"}) {input: oldest.input, output: self.input, identifier: range (1 , 100)};
(oldest:agent) {input: "oldest", output: oldest_root.output};
(oldest)-[:contains]->(oldest_root);
(oldest)-[:next]->(oldest_root);
(baseline_root:task {name: "baseline"}) {input: baseline.input, output: self.input, identifier: range (1 , 100)};
(baseline:agent) {input: "baseline", output: baseline_root.output};
(baseline)-[:contains]->(baseline_root);
(baseline)-[:next]->(baseline_root);
(candidate_root:task {name: "candidate"}) {input: candidate.input, output: self.input, identifier: range (1 , 100)};
(candidate:agent) {input: "candidate", output: candidate_root.output};
(candidate)-[:contains]->(candidate_root);
(candidate)-[:next]->(candidate_root);
(other_root:task {name: "other"}) {input: other.input, output: self.input, identifier: range (1 , 100)};
(other:agent) {input: "other", output: other_root.output};
(other)-[:contains]->(other_root);
(other)-[:next]->(other_root);
("quality":scorer) {score: 1};
("unrelated":scorer) {score: 0};
(dataset_shared_trace_source:case {name: "trace-source"}) {source: (source_root)};
(dataset_shared:dataset {name: "shared"}) {cases: [|(dataset_shared_trace_source)|]};
(dataset_other_inline:case {name: "inline"}) {input: "other"};
(dataset_other:dataset {name: "other"}) {cases: [|(dataset_other_inline)|]};
(experiment_oldest:experiment {name: "oldest"}) {task: (oldest_root), dataset: (dataset_shared), scorers: [|("quality")|]};
(experiment_baseline:experiment {name: "baseline"}) {task: (baseline_root), dataset: (dataset_shared), scorers: [|("quality")|], baseline: (experiment_oldest)};
(experiment_candidate:experiment {name: "candidate"}) {task: (candidate_root), dataset: (dataset_shared), scorers: [|("quality")|], baseline: (experiment_baseline)};
(experiment_candidate_extra:experiment {name: "candidate-extra"}) {task: (candidate_root), dataset: (dataset_shared), scorers: [|("quality")|]};
(experiment_sibling:experiment {name: "sibling"}) {task: (baseline_root), dataset: (dataset_other), scorers: [|("unrelated")|]};
(experiment_other:experiment {name: "other"}) {task: (other_root), dataset: (dataset_other), scorers: [|("unrelated")|]};"#;

    #[test]
    fn experiment_selection_keeps_baseline_dependencies_and_trace_reference_indexes() {
        let model = crate::providers::braintrust::mocking::compile(AGENTS).unwrap();
        let selected = agents(&model, &["experiment.candidate".parse().unwrap()]).unwrap();
        assert_eq!(
            selected.agents.iter().map(|agent| agent.name.as_str()).collect::<Vec<_>>(),
            ["oldest", "baseline", "candidate"]
        );
        assert_eq!(
            selected
                .experiments
                .iter()
                .map(|experiment| experiment.name.as_str())
                .collect::<Vec<_>>(),
            ["oldest", "baseline", "candidate"]
        );
        assert_eq!(selected.datasets.len(), 1);
        assert_eq!(selected.datasets[0].name, "shared");
        assert_eq!(
            selected.scorers.iter().map(|scorer| scorer.name.as_str()).collect::<Vec<_>>(),
            ["quality", "unrelated"]
        );
        assert_eq!(
            selected.traces.iter().map(|trace| trace.node).collect::<Vec<_>>(),
            model.traces.iter().map(|trace| trace.node).collect::<Vec<_>>()
        );
        assert_eq!(format!("{:?}", selected.refs), format!("{:?}", model.refs));
        assert_eq!(format!("{:?}", selected.bindings), format!("{:?}", model.bindings));
        assert_eq!(
            selected.agents[2].entrypoint,
            model
                .agents
                .iter()
                .find(|agent| agent.name == "candidate")
                .unwrap()
                .entrypoint
        );
    }

    #[test]
    fn agent_selection_includes_entrypoint_evals_and_preserves_source_order() {
        let model = crate::providers::braintrust::mocking::compile(AGENTS).unwrap();
        let selected = agents(
            &model,
            &[
                "agent.candidate".parse().unwrap(),
                "experiment.candidate".parse().unwrap(),
                "agent.candidate".parse().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(
            selected
                .experiments
                .iter()
                .map(|experiment| experiment.name.as_str())
                .collect::<Vec<_>>(),
            ["oldest", "baseline", "candidate", "candidate-extra"]
        );
        let unchanged = agents(&model, &[]).unwrap();
        assert_eq!(unchanged.agents.len(), model.agents.len());
        assert_eq!(unchanged.experiments.len(), model.experiments.len());
        assert_eq!(unchanged.scorers.len(), model.scorers.len());
        assert_eq!(unchanged.datasets.len(), model.datasets.len());
    }

    #[test]
    fn agent_selection_rejects_unknown_incompatible_and_unbound_evals() {
        let model = crate::providers::braintrust::mocking::compile(AGENTS).unwrap();
        for selector in ["agent.missing", "experiment.missing"] {
            assert!(matches!(
                agents(&model, &[selector.parse().unwrap()]),
                Err(Error::Unknown { .. })
            ));
        }
        for selector in ["scorer.quality", "dataset.shared"] {
            assert!(matches!(
                agents(&model, &[selector.parse().unwrap()]),
                Err(Error::Incompatible { .. })
            ));
        }
        let mut model = model;
        model.agents.clear();
        assert!(matches!(
            agents(&model, &["experiment.candidate".parse().unwrap()]),
            Err(Error::NoAgent { .. })
        ));
    }

    #[test]
    fn explicit_agent_selection_resolves_a_shared_entrypoint() {
        let mut model = crate::providers::braintrust::mocking::compile(
            r#"(shared_root:task {name: "shared"}) {output: "answer", input: second.input};
(second:agent) {input: null, output: shared_root.output};
(second)-[:contains]->(shared_root);
(second)-[:next]->(shared_root);
("quality":scorer) {score: 1};
(dataset_cases_one:case {name: "one"}) {input: "question"};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_one)|]};
(experiment_check:experiment {name: "check"}) {task: (shared_root), dataset: (dataset_cases), scorers: [|("quality")|]};"#,
        )
        .unwrap();
        let mut first = model.agents[0].clone();
        first.name = "first".into();
        model.agents.insert(0, first);
        assert!(matches!(
            agents(&model, &["experiment.check".parse().unwrap()]),
            Err(Error::AmbiguousAgents { .. })
        ));
        let selected = agents(&model, &["agent.second".parse().unwrap()]).unwrap();
        assert_eq!(selected.agents.len(), 1);
        assert_eq!(selected.agents[0].name, "second");
        assert_eq!(selected.experiments.len(), 1);
        assert!(matches!(
            agents(&model, &["agent.first".parse().unwrap(), "agent.second".parse().unwrap()]),
            Err(Error::AmbiguousAgents { .. })
        ));
    }

    #[test]
    fn selection_rejects_artifact_collisions_with_unselected_resources() {
        let model = crate::providers::braintrust::mocking::compile(
            r#"(first_root:task {name: "first"}) {output: "first", input: a_b.input};
(a_b:agent {name: "a-b"}) {input: null, output: first_root.output};
(a_b)-[:contains]->(first_root);
(a_b)-[:next]->(first_root);
(second_root:task {name: "second"}) {output: "second", input: a_b_1.input};
(a_b_1:agent {name: "a_b"}) {input: null, output: second_root.output};
(a_b_1)-[:contains]->(second_root);
(a_b_1)-[:next]->(second_root);
(safe_root:task {name: "safe"}) {output: "safe", input: safe.input};
(safe:agent) {input: null, output: safe_root.output};
(safe)-[:contains]->(safe_root);
(safe)-[:next]->(safe_root);
("quality":scorer) {score: 1};
(dataset_cases_one:case {name: "one"}) {input: "hello"};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_one)|]};
(experiment_check_a:experiment {name: "check-a"}) {task: (safe_root), dataset: (dataset_cases), scorers: [|("quality")|]};
(experiment_check_b:experiment {name: "check_a"}) {task: (safe_root), dataset: (dataset_cases), scorers: [|("quality")|]};
(experiment_safe:experiment {name: "safe"}) {task: (safe_root), dataset: (dataset_cases), scorers: [|("quality")|]};"#,
        )
        .unwrap();
        for selector in [
            r#"agent["a-b"]"#,
            "agent.a_b",
            r#"experiment["check-a"]"#,
            "experiment.check_a",
        ] {
            let error = agents(&model, &[selector.parse().unwrap()]).unwrap_err();
            assert!(matches!(error, Error::ArtifactCollision { .. }));
            assert!(error.to_string().contains("both generate module"));
        }
        let selected = agents(&model, &["experiment.safe".parse().unwrap()]).unwrap();
        assert_eq!(selected.agents[0].name, "safe");
        assert_eq!(selected.experiments.len(), 1);
    }

    #[test]
    fn selection_preserves_whole_rules_and_deduplicates_in_source_order() {
        let model = source();
        let selectors = ["scorer.brand".parse().unwrap(), "automation.shared".parse().unwrap()];
        let rules = automations(&model, AutomationType::Scorers, &selectors).unwrap();
        assert_eq!(rules.len(), 1);
        let AutomationKind::Scorer { scorers, .. } = &rules[0].kind else {
            panic!("scorer rule")
        };
        assert_eq!(scorers, &["brand", "quality"]);
        let rules = automations(&model, AutomationType::Topics, &["facet[\"Churn risk\"]".parse().unwrap()]).unwrap();
        assert_eq!(rules[0].name, "topics");
        assert!(matches!(&rules[0].kind, AutomationKind::Topics { facets } if facets.len() == 2));
        let rules = automations(
            &model,
            AutomationType::Scorers,
            &["automation.other".parse().unwrap(), "automation.shared".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(
            rules.iter().map(|rule| rule.name.as_str()).collect::<Vec<_>>(),
            ["shared", "other"]
        );
        let selected = datasets(
            &model,
            &[
                "dataset.second".parse().unwrap(),
                "dataset.first".parse().unwrap(),
                "dataset.second".parse().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(
            selected.iter().map(|dataset| dataset.name.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            datasets(&model, &["dataset.second".parse().unwrap()]).unwrap()[0].name,
            "second"
        );
    }

    #[test]
    fn rejects_invalid_unknown_and_incompatible_selections() {
        for source in [
            "scorer.brand.prompt",
            "dataset[0]",
            "dataset",
            "dataset.first + 1",
            "trace.first",
            "facet[\"\"]",
        ] {
            assert!(source.parse::<ResourceSelector>().is_err(), "{source}");
        }
        let model = source();
        for selector in ["scorer.missing", "facet[\"Churn risk\"]", "automation.topics"] {
            assert!(automations(&model, AutomationType::Scorers, &[selector.parse().unwrap()]).is_err());
        }
        assert!(datasets(&model, &["dataset.missing".parse().unwrap()]).is_err());
        assert!(datasets(&model, &["automation.shared".parse().unwrap()]).is_err());
    }
}
