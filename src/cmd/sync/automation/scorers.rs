use super::{Action, Error, check_response, get_objects, single};
use crate::cmd::shared::{
    client::Client,
    state::{ResourceKind, SyncState, TrackedResource},
};
use crate::conf::{Braintrust, Settings};
use crate::dsl::{self, Automation};
use crate::scg;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(super) fn run(args: super::Args) -> Result<(), Error> {
    let model = args.compile()?;
    let config = Braintrust::load()?;
    let project_id = config.project_id.to_string();
    let mut state = SyncState::load(&args.from, &project_id, args.dry_run)?;
    let (automations, selected) = state.rules(&model, &ResourceKind::ScorerAutomation, &args.select)?;
    let removals = state.removals(&model, &ResourceKind::ScorerAutomation, &selected, args.select.is_empty());
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
    let slugs = scg::resolve_slugs(&scorers).map_err(Error::ScorerPlan)?;
    let slugs_by_name: HashMap<_, _> = scorers.iter().map(|scorer| scorer.name.as_str()).zip(slugs.iter()).collect();

    let settings = Settings::load()?;
    let client = Client::configured(config.clone(), &settings).map_err(|error| Error::Http(error.into()))?;
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
            &client,
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
            &client,
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
            Action::Unchanged => println!("unchanged {name}"),
            Action::Create | Action::Update => {
                let verb = if action == Action::Create { "create" } else { "update" };
                if args.dry_run {
                    println!("would {verb} {name}");
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
                    println!("{} {name}", if action == Action::Create { "created" } else { "updated" });
                }
            }
        }
    }
    for resource in &removals {
        state.delete(&client, resource, args.dry_run)?;
    }
    if !args.dry_run {
        state.removed(&removals, &model, &ResourceKind::ScorerAutomation, &selected)?;
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
