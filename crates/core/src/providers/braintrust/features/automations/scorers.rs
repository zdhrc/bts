use crate::dsl::{self, Automation};
use crate::mocking::code;
use crate::providers::braintrust::client::{ApiError, checked as check_response, get_objects, single};
use crate::selection;
use crate::{
    providers::braintrust::client::Client,
    providers::braintrust::state::{ResourceKind, SyncState, TrackedResource},
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::{fmt, path::Path};

/// A compiled model, source path, and state root for scorer automation reconciliation.
pub struct Request<'a> {
    pub model: &'a dsl::Model,
    pub from: &'a Path,
    pub root: &'a Path,
    pub dry_run: bool,
    pub select: &'a [selection::ResourceSelector],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Create,
    Update,
    Unchanged,
}

/// A change or regeneration reported by an automation operation.
#[derive(Debug)]
pub enum Progress {
    Changed { action: Action, name: String, dry_run: bool },
    State(crate::providers::braintrust::state::Progress),
}

#[derive(Debug)]
pub enum Error {
    Response(ApiError),
    State(crate::providers::braintrust::state::Error),
    Selection(crate::selection::Error),
    NoAutomations,
    ScorerPlan(code::Error),
    Http(crate::providers::braintrust::client::Error),
    MissingScorer(String),
    Api(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Response(source) => source.fmt(formatter),
            Self::Selection(source) => source.fmt(formatter),
            Self::State(source) => source.fmt(formatter),
            Self::NoAutomations => formatter.write_str("the selection contains no automations of this type"),
            Self::ScorerPlan(source) => source.fmt(formatter),
            Self::Http(source) => source.fmt(formatter),
            Self::MissingScorer(name) => write!(formatter, "scorer {name:?} is not pushed in the selected Braintrust project"),
            Self::Api(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Response(source) => Some(source),
            Self::Selection(source) => Some(source),
            Self::State(source) => Some(source),
            Self::Http(source) => Some(source),
            Self::ScorerPlan(source) => Some(source),
            _ => None,
        }
    }
}
impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::Response(error)
    }
}

impl From<crate::providers::braintrust::state::Error> for Error {
    fn from(error: crate::providers::braintrust::state::Error) -> Self {
        match error {
            crate::providers::braintrust::state::Error::Selection(message) => Self::Selection(message),
            other => Self::State(other),
        }
    }
}

/// Reconciles selected scoring rules with already deployed scorer functions.
///
/// The model must retain validated scorer bindings. This operation owns state
/// locking and recovery. Dry runs read remote resources without modifying them
/// or saving state. Failures retain their causes and may follow earlier remote
/// changes. Shared resources remain owned by any other source that needs them.
pub fn run(args: Request<'_>, client: &Client, report: &mut dyn FnMut(Progress)) -> Result<(), Error> {
    let model = args.model;
    let config = client.config();
    let project_id = config.project_id.to_string();
    let mut state = SyncState::load(args.root, args.from, &project_id, args.dry_run)?;
    let (automations, selected) = state.rules(model, &ResourceKind::ScorerAutomation, args.select)?;
    let removals = state.removals(model, &ResourceKind::ScorerAutomation, &selected, args.select.is_empty());
    if automations.is_empty() && removals.is_empty() {
        return Err(Error::NoAutomations);
    }
    let names: HashSet<_> = automations
        .iter()
        .flat_map(|automation| match &automation.kind {
            dsl::AutomationKind::Scorer { scorers, .. } => scorers.iter(),
            _ => unreachable!(),
        })
        .collect();
    let scorers: Vec<_> = model
        .scorers
        .iter()
        .filter(|scorer| names.contains(&scorer.name))
        .cloned()
        .collect();
    let slugs = crate::providers::braintrust::mocking::code::resolve_slugs(&scorers).map_err(Error::ScorerPlan)?;
    let slugs_by_name: HashMap<_, _> = scorers.iter().map(|scorer| scorer.name.as_str()).zip(slugs.iter()).collect();

    let base = config.api_url.trim_end_matches('/');

    // check that all the scorers are pushed before writing
    let mut function_ids = HashMap::new();
    for scorer in automations.iter().flat_map(|automation| match &automation.kind {
        dsl::AutomationKind::Scorer { scorers, .. } => scorers.as_slice(),
        _ => unreachable!(),
    }) {
        if function_ids.contains_key(scorer) {
            continue;
        }
        let slug = slugs_by_name[scorer.as_str()];
        let objects = get_objects(
            client,
            &format!("{base}/v1/function"),
            &[("project_id", project_id.as_str()), ("slug", slug.as_str()), ("limit", "2")],
        )?;
        let function = single(objects, "scorer function", scorer)?.ok_or_else(|| Error::MissingScorer(scorer.clone()))?;
        if function["project_id"] != project_id || function["slug"] != *slug || function["function_type"] != "scorer" {
            return Err(Error::Api(format!(
                "function {scorer:?} is not a scorer in the selected project"
            )));
        }
        let id = function["id"]
            .as_str()
            .ok_or_else(|| Error::Api(format!("function {scorer:?} has no id")))?;
        function_ids.insert(scorer.clone(), id.to_owned());
    }

    let mut plans = Vec::new();
    for automation in &automations {
        let objects = get_objects(
            client,
            &format!("{base}/v1/project_score"),
            &[
                ("project_id", project_id.as_str()),
                ("project_score_name", automation.name.as_str()),
                ("limit", "2"),
            ],
        )?;
        let existing = single(objects, "automation", &automation.name)?;
        if existing
            .as_ref()
            .is_some_and(|rule| rule["project_id"] != project_id || rule["name"] != automation.name)
        {
            return Err(Error::Api(format!(
                "project score lookup returned a rule other than {:?} in the selected project",
                automation.name
            )));
        }
        if existing.as_ref().is_some_and(|rule| rule["score_type"] != "online") {
            return Err(Error::Api(format!(
                "project score {:?} already exists with a different type",
                automation.name
            )));
        }
        let online = desired_online(automation, &function_ids);
        let action = match &existing {
            None => Action::Create,
            Some(rule) if online_matches(rule, &online) => Action::Unchanged,
            Some(_) => Action::Update,
        };
        let mut config_value = existing
            .as_ref()
            .and_then(|rule| rule.get("config"))
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let config_object = config_value.as_object_mut().expect("config is an object");
        let mut online_value = config_object
            .get("online")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        online_value
            .as_object_mut()
            .expect("online is an object")
            .extend(online.as_object().expect("desired online is an object").clone());
        config_object.insert("online".to_owned(), online_value);
        let mut payload = json!({
            "project_id": project_id,
            "name": automation.name,
            "score_type": "online",
            "config": config_value,
        });
        if let Some(rule) = &existing {
            for field in ["description", "categories"] {
                if let Some(value) = rule.get(field) {
                    payload[field] = value.clone();
                }
            }
        }
        let dependencies = match &automation.kind {
            dsl::AutomationKind::Scorer { scorers, .. } => scorers.clone(),
            _ => unreachable!(),
        };
        state.stage(TrackedResource {
            kind: ResourceKind::ScorerAutomation,
            name: automation.name.clone(),
            slug: None,
            id: existing.as_ref().and_then(|rule| rule["id"].as_str()).map(str::to_owned),
            dependencies,
            pending: None,
        })?;
        plans.push((action, automation.name.as_str(), payload));
    }
    if !args.dry_run {
        state.save()?;
    }

    for (action, name, payload) in plans {
        match action {
            Action::Unchanged => report(Progress::Changed {
                action,
                name: name.to_owned(),
                dry_run: args.dry_run,
            }),
            Action::Create | Action::Update => {
                if args.dry_run {
                    report(Progress::Changed {
                        action,
                        name: name.to_owned(),
                        dry_run: true,
                    });
                } else {
                    let response = client
                        .put(format!("{base}/v1/project_score"))
                        .json(&payload)
                        .retryable()
                        .send()
                        .map_err(Error::Http)?;
                    let result = check_response(response, &format!("sync automation {name:?}"))?;
                    let id = result["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| Error::Api(format!("automation write for {name:?} returned no id")))?;
                    state.complete(&ResourceKind::ScorerAutomation, name, id)?;
                    report(Progress::Changed {
                        action,
                        name: name.to_owned(),
                        dry_run: false,
                    });
                }
            }
        }
    }
    for resource in &removals {
        state.delete(client, resource, args.dry_run, &mut |event| report(Progress::State(event)))?;
    }
    if !args.dry_run {
        state.removed(&removals, model, &ResourceKind::ScorerAutomation, &selected)?;
    }
    Ok(())
}

fn desired_online(automation: &Automation, ids: &HashMap<String, String>) -> Value {
    let dsl::AutomationKind::Scorer {
        scorers,
        root,
        span_names,
    } = &automation.kind
    else {
        unreachable!()
    };
    json!({
        "sampling_rate": automation.sampling_rate,
        "scorers": scorers.iter().map(|slug| json!({ "type": "function", "id": ids[slug] })).collect::<Vec<_>>(),
        "status": if automation.enabled { "active" } else { "paused" },
        "apply_to_root_span": root,
        "apply_to_span_names": span_names,
        "scope": { "type": "span" },
    })
}

fn online_matches(rule: &Value, desired: &Value) -> bool {
    let online = &rule["config"]["online"];
    let span_names = online["apply_to_span_names"].as_array().cloned().unwrap_or_default();
    let scorer_refs_match = match (online["scorers"].as_array(), desired["scorers"].as_array()) {
        (Some(actual), Some(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual["type"] == expected["type"] && actual["id"] == expected["id"])
        }
        _ => false,
    };
    online["sampling_rate"].as_f64() == desired["sampling_rate"].as_f64()
        && scorer_refs_match
        && online["status"].as_str().unwrap_or("active") == desired["status"].as_str().unwrap()
        && online["apply_to_root_span"].as_bool().unwrap_or(false) == desired["apply_to_root_span"].as_bool().unwrap()
        && Value::Array(span_names) == desired["apply_to_span_names"]
        && online["scope"].get("type").and_then(Value::as_str).unwrap_or("span") == "span"
}
