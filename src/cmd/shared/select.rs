use crate::dsl::{self, Automation, AutomationKind, Dataset, Model};
use std::{collections::BTreeSet, str::FromStr};

#[derive(Debug, Clone)]
pub(crate) struct ResourceSelector {
    pub(crate) kind: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomationType {
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

impl FromStr for ResourceSelector {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let render = |diags: dsl::Diags| {
            diags
                .into_iter()
                .map(|diag| diag.render("--select", source))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut path = dsl::parse_traversal(source).map_err(render)?;
        if path.len() != 2 {
            return Err("--select expects a named resource traversal without field access".to_owned());
        }
        let name = path.pop().expect("two traversal segments");
        let kind = path.pop().expect("two traversal segments");
        if !matches!(kind.as_str(), "scorer" | "facet" | "automation" | "dataset" | "experiment") || name.is_empty() {
            return Err("--select expects a named scorer, facet, automation, dataset, or experiment".to_owned());
        }
        Ok(Self { kind, name })
    }
}

pub(crate) fn automations<'a>(
    model: &'a Model,
    kind: AutomationType,
    selectors: &[ResourceSelector],
) -> Result<Vec<&'a Automation>, String> {
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
                return Err(format!(
                    "--select: {} is not compatible with this automation command",
                    selector.kind
                ));
            }
        };
        if !exists {
            return Err(format!("--select: unknown {} {:?}", selector.kind, selector.name));
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
            return Err(format!(
                "--select: {} {:?} matches no automations of this type",
                selector.kind, selector.name
            ));
        }
    }
    Ok(selected.into_iter().map(|index| &model.automations[index]).collect())
}

pub(crate) fn datasets<'a>(model: &'a Model, selectors: &[ResourceSelector]) -> Result<Vec<&'a Dataset>, String> {
    if selectors.is_empty() {
        return Ok(model.datasets.iter().collect());
    }
    let mut selected = BTreeSet::new();
    for selector in selectors {
        if selector.kind != "dataset" {
            return Err(format!("--select: {} is not compatible with dataset sync", selector.kind));
        }
        let index = model
            .datasets
            .iter()
            .position(|dataset| dataset.name == selector.name)
            .ok_or_else(|| format!("--select: unknown dataset {:?}", selector.name))?;
        selected.insert(index);
    }
    Ok(selected.into_iter().map(|index| &model.datasets[index]).collect())
}

// old names still work when selecting something to remove
pub(crate) fn automation_names(
    kind: AutomationType,
    known: &[(String, Vec<String>)],
    selectors: &[ResourceSelector],
) -> Result<BTreeSet<String>, String> {
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
            return Err(format!(
                "--select: {} is not compatible with this automation command",
                selector.kind
            ));
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
            return Err(format!(
                "--select: {} {:?} matches no local or previously synced automations of this type",
                selector.kind, selector.name
            ));
        }
        names.extend(matched.iter().map(|(name, _)| name.clone()));
    }
    Ok(names)
}

pub(crate) fn dataset_names(known: &BTreeSet<String>, selectors: &[ResourceSelector]) -> Result<BTreeSet<String>, String> {
    if selectors.is_empty() {
        return Ok(known.clone());
    }
    let mut names = BTreeSet::new();
    for selector in selectors {
        if selector.kind != "dataset" {
            return Err(format!("--select: {} is not compatible with dataset sync", selector.kind));
        }
        if !known.contains(&selector.name) {
            return Err(format!("--select: unknown dataset {:?}", selector.name));
        }
        names.insert(selector.name.clone());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
        scorer "brand" { code { score = 1 } }
        scorer "quality" { code { score = 1 } }
        facet "Churn risk" { prompt = "Summarize churn risk." }
        facet "Intent" { prompt = "Summarize intent." }
        automation "shared" { type = "scorer" scorers = ["brand", "quality"] scope = "span" root = true }
        automation "other" { type = "scorer" scorers = ["quality"] scope = "span" root = true }
        automation "topics" { type = "topics" facets = ["Churn risk", "Intent"] scope = "trace" }
        dataset "first" { case "one" { input = "hello" } }
        dataset "second" { case "two" { input = "world" } }
    "#;

    #[test]
    fn selection_preserves_whole_rules_and_deduplicates_in_source_order() {
        let model = dsl::compile(SOURCE).unwrap();
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
        let model = dsl::compile(SOURCE).unwrap();
        for selector in ["scorer.missing", "facet[\"Churn risk\"]", "automation.topics"] {
            assert!(automations(&model, AutomationType::Scorers, &[selector.parse().unwrap()]).is_err());
        }
        assert!(datasets(&model, &["dataset.missing".parse().unwrap()]).is_err());
        assert!(datasets(&model, &["automation.shared".parse().unwrap()]).is_err());
    }
}
