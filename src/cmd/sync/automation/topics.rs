use super::{Action, Error, TopicsArgs, check_response, get_objects, single};
use crate::cmd::shared::state::{PendingOperation, ResourceKind, SyncState, TrackedResource};
use crate::{
    cmd::shared::client::Client,
    conf::{Braintrust, Settings},
    dsl::AutomationKind,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

const EMBEDDING_MODEL: &str = "brain-embedding-1";

struct FunctionPlan {
    action: Action,
    name: String,
    slug: String,
    function_type: &'static str,
    id: String,
    existing: Option<Value>,
    data: Value,
    description: Value,
}

struct AutomationPlan {
    action: Action,
    name: String,
    existing: Option<Value>,
    config: Value,
    dependencies: Vec<String>,
    regenerate_from: Option<String>,
}

pub(super) fn run(args: TopicsArgs) -> Result<(), Error> {
    let regenerate_requested = args.regenerate;
    let args = args.sync;
    let model = args.compile()?;
    let config = Braintrust::load()?;
    let client = Client::configured(config.clone(), &Settings::load()?).map_err(|error| Error::Http(error.into()))?;
    let base = config.api_url.trim_end_matches('/');
    let app = config.app_url.trim_end_matches('/');
    let project_id = config.project_id.to_string();
    let mut state = SyncState::load(&args.from, &project_id, args.dry_run)?;
    let (automations, selected) = state.rules(&model, &ResourceKind::TopicsAutomation, &args.select)?;
    let removals = state.removals(&model, &ResourceKind::TopicsAutomation, &selected, args.select.is_empty());
    if automations.is_empty() && removals.is_empty() {
        return Err(Error::NoAutomations);
    }
    let mut functions = Vec::new();
    let mut ids = HashMap::new();
    let mut slugs = HashSet::new();
    let mut changed_prompts = HashSet::new();

    // check everything before writing
    for automation in &automations {
        let AutomationKind::Topics { facets } = &automation.kind else {
            unreachable!()
        };
        for name in facets {
            if ids.contains_key(name) {
                continue;
            }
            let facet = model
                .facets
                .iter()
                .find(|facet| facet.name == *name)
                .expect("validated facet");
            let slug = crate::scg::slugify(name).ok_or_else(|| Error::Api(format!("facet {name:?} cannot form a slug")))?;
            let map_slug = format!("bts-{slug}-topic-map");
            for slug in [&slug, &map_slug] {
                if !slugs.insert(slug.clone()) {
                    return Err(Error::Api(format!("multiple selected resources use function slug {slug:?}")));
                }
            }
            let existing = lookup_function(&client, &slug, name, "facet", "facet")?;
            if existing
                .as_ref()
                .is_some_and(|function| function["function_data"]["prompt"] != facet.prompt)
            {
                changed_prompts.insert(name.clone());
            }
            let mut data = existing
                .as_ref()
                .map(|value| value["function_data"].clone())
                .unwrap_or_else(|| json!({}));
            data["type"] = json!("facet");
            data["prompt"] = json!(facet.prompt);
            data.as_object_mut()
                .expect("validated function data")
                .remove("no_match_pattern");
            if let Some(pattern) = &facet.no_match_pattern {
                data["no_match_pattern"] = json!(pattern);
            }
            let facet_plan = plan_function(name, &slug, "facet", existing, data, json!(facet.description))?;
            let facet_id = facet_plan.id.clone();
            functions.push(facet_plan);

            let map_name = format!("{name} topics");
            let existing = lookup_function(&client, &map_slug, &map_name, "classifier", "topic_map")?;
            let mut data = existing
                .as_ref()
                .map(|value| value["function_data"].clone())
                .unwrap_or_else(|| json!({}));
            // keep the generated map bits
            data["type"] = json!("topic_map");
            data["source_facet"] = json!(name);
            data["source_facet_function"] = json!({ "type": "function", "id": facet_id });
            if data.get("embedding_model").is_none() {
                data["embedding_model"] = json!(EMBEDDING_MODEL);
            }
            let description = existing
                .as_ref()
                .map(|value| value["description"].clone())
                .unwrap_or(Value::Null);
            let map_plan = plan_function(&map_name, &map_slug, "classifier", existing, data, description)?;
            ids.insert(name.clone(), (facet_id, map_plan.id.clone()));
            functions.push(map_plan);
        }
    }

    let mut rules = Vec::new();
    for automation in automations {
        let AutomationKind::Topics { facets } = &automation.kind else {
            unreachable!()
        };
        let response = client
            .post(format!("{app}/api/project_automation/get"))
            .json(&json!({ "project_id": project_id, "name": automation.name, "limit": 2 }))
            .retryable()
            .send()
            .map_err(Error::Http)?;
        let value = check_response(response, "lookup Topics automation")?;
        let objects = value
            .as_array()
            .cloned()
            .ok_or_else(|| Error::Api("Topics lookup returned no array".to_owned()))?;
        let existing = single(objects, "Topics automation", &automation.name)?;
        if let Some(rule) = &existing {
            if rule["project_id"] != project_id || rule["name"] != automation.name || rule["config"]["event_type"] != "topic" {
                return Err(Error::Api(format!(
                    "automation {:?} conflicts with an existing resource",
                    automation.name
                )));
            }
            object_id(rule)?;
        }
        let mut desired = existing.as_ref().map(|rule| rule["config"].clone()).unwrap_or_else(|| {
            json!({
                "rerun_seconds": 86400,
                "backfill_time_range": "1d",
                "relabel_overlap_seconds": 3600,
                "data_scope": { "type": "project_logs" },
            })
        });
        if desired
            .get("data_scope")
            .is_some_and(|scope| !scope.is_null() && scope["type"] != "project_logs")
        {
            return Err(Error::Api(format!(
                "Topics automation {:?} must target project logs",
                automation.name
            )));
        }
        desired["event_type"] = json!("topic");
        if desired["sampling_rate"].as_f64() != Some(automation.sampling_rate) {
            desired["sampling_rate"] = json!(automation.sampling_rate);
        }
        desired["status"] = json!(if automation.enabled { "active" } else { "paused" });
        if desired["scope"]["type"] != "trace" {
            desired["scope"] = json!({ "type": "trace", "idle_seconds": 600 });
        }
        desired["facet_functions"] = json!(
            facets
                .iter()
                .map(|name| json!({ "type": "function", "id": ids[name].0 }))
                .collect::<Vec<_>>()
        );
        let prior_maps = desired["topic_map_functions"].as_array().cloned().unwrap_or_default();
        desired["topic_map_functions"] = json!(
            facets
                .iter()
                .map(|name| {
                    let id = &ids[name].1;
                    prior_maps
                        .iter()
                        .find(|map| map["function"]["type"] == "function" && map["function"]["id"] == *id)
                        .cloned()
                        .unwrap_or_else(|| json!({ "function": { "type": "function", "id": id } }))
                })
                .collect::<Vec<_>>()
        );
        let action = match &existing {
            None => Action::Create,
            Some(rule) if rule["config"] == desired => Action::Unchanged,
            Some(_) => Action::Update,
        };
        let regenerate_from = if regenerate_requested && facets.iter().any(|name| changed_prompts.contains(name)) {
            Some(regeneration_start(&desired)?)
        } else {
            state.regeneration(&automation.name).map(str::to_owned)
        };
        rules.push(AutomationPlan {
            action,
            name: automation.name.clone(),
            existing,
            config: desired,
            dependencies: facets.clone(),
            regenerate_from,
        });
    }

    // save the plan first so a retry can finish it
    for plan in &functions {
        let kind = if plan.function_type == "facet" {
            ResourceKind::Facet
        } else {
            ResourceKind::TopicMap
        };
        let name = if kind == ResourceKind::TopicMap {
            plan.name.strip_suffix(" topics").expect("map name")
        } else {
            &plan.name
        };
        state.stage(TrackedResource {
            kind,
            name: name.to_owned(),
            slug: Some(plan.slug.clone()),
            id: plan.existing.as_ref().map(object_id).transpose()?.map(str::to_owned),
            dependencies: Vec::new(),
            pending: None,
        })?;
    }
    for plan in &rules {
        state.stage(TrackedResource {
            kind: ResourceKind::TopicsAutomation,
            name: plan.name.clone(),
            slug: None,
            id: plan.existing.as_ref().map(object_id).transpose()?.map(str::to_owned),
            dependencies: plan.dependencies.clone(),
            pending: plan
                .regenerate_from
                .clone()
                .map(|start_xact_id| PendingOperation::Regenerate { start_xact_id }),
        })?;
    }
    if !args.dry_run {
        state.save()?;
    }

    let mut created_ids = HashMap::new();
    for mut plan in functions {
        if plan.action == Action::Unchanged || args.dry_run {
            print_action(plan.action, &plan.name, args.dry_run);
            continue;
        }
        resolve_ids(&mut plan.data, &created_ids);
        let response = if plan.existing.is_some() {
            client
                .patch(format!("{base}/v1/function/{}", plan.id))
                .json(&json!({ "function_data": plan.data, "description": plan.description }))
                .retryable()
                .send()
        } else {
            client
                .put(format!("{base}/v1/function"))
                .json(&json!({ "project_id": project_id, "name": plan.name, "slug": plan.slug,
                    "function_type": plan.function_type, "function_data": plan.data, "description": plan.description }))
                .retryable()
                .send()
        }
        .map_err(Error::Http)?;
        let value = check_response(response, &format!("sync function {:?}", plan.name))?;
        let id = object_id(&value)?;
        if value["project_id"] != project_id || value["slug"] != plan.slug || value["function_type"] != plan.function_type {
            return Err(Error::Api(format!(
                "function write returned a different resource for {:?}",
                plan.name
            )));
        }
        if plan.existing.is_some() && id != plan.id {
            return Err(Error::Api(format!("function write changed the identity of {:?}", plan.name)));
        }
        created_ids.insert(plan.id, id.to_owned());
        let kind = if plan.function_type == "facet" {
            ResourceKind::Facet
        } else {
            ResourceKind::TopicMap
        };
        let name = if kind == ResourceKind::TopicMap {
            plan.name.strip_suffix(" topics").expect("map name")
        } else {
            &plan.name
        };
        state.complete(&kind, name, id)?;
        print_action(plan.action, &plan.name, false);
    }
    for mut plan in rules {
        if args.dry_run {
            print_action(plan.action, &plan.name, args.dry_run);
            if plan.regenerate_from.is_some() {
                println!("would regenerate {:?} over its existing Topics window", plan.name);
            }
            continue;
        }
        resolve_ids(&mut plan.config, &created_ids);
        if plan.action == Action::Unchanged {
            print_action(plan.action, &plan.name, false);
            if let Some(start) = &plan.regenerate_from {
                regenerate(&client, object_id(plan.existing.as_ref().expect("existing rule"))?, start)?;
                state.regenerated(&plan.name)?;
                println!("queued regeneration {:?}", plan.name);
            }
            continue;
        }
        let (path, payload) = match plan.existing {
            Some(rule) => (
                "patch_id",
                json!({ "id": object_id(&rule)?, "name": plan.name, "config": plan.config }),
            ),
            None => (
                "register",
                json!({ "project_id": project_id, "project_automation_name": plan.name, "config": plan.config, "update": false }),
            ),
        };
        let response = client
            .post(format!("{app}/api/project_automation/{path}"))
            .json(&payload)
            .send()
            .map_err(Error::Http)?;
        let result = check_response(response, &format!("sync Topics automation {:?}", plan.name))?;
        let rule = if path == "register" {
            &result["project_automation"]
        } else {
            &result
        };
        object_id(rule)?;
        if rule["project_id"] != project_id || rule["name"] != plan.name || rule["config"]["event_type"] != "topic" {
            return Err(Error::Api(format!(
                "automation write returned a different resource for {:?}",
                plan.name
            )));
        }
        print_action(plan.action, &plan.name, false);
        state.complete(&ResourceKind::TopicsAutomation, &plan.name, object_id(rule)?)?;
        if let Some(start) = &plan.regenerate_from {
            regenerate(&client, object_id(rule)?, start)?;
            state.regenerated(&plan.name)?;
            println!("queued regeneration {:?}", plan.name);
        }
    }
    for resource in &removals {
        state.delete(&client, resource, args.dry_run)?;
    }
    if !args.dry_run {
        state.removed(&removals, &model, &ResourceKind::TopicsAutomation, &selected)?;
    }
    Ok(())
}

fn regeneration_start(config: &Value) -> Result<String, Error> {
    let window = config["backfill_time_range"].as_str().unwrap_or("3d");
    let seconds = crate::conf::parse_duration(window).map_err(Error::Api)?.as_secs();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| Error::Api(error.to_string()))?
        .as_secs();
    let timestamp = now.saturating_sub(seconds);
    Ok(((0x0de1_u64 << 48) | (timestamp << 16)).saturating_sub(1).to_string())
}

fn regenerate(client: &Client, id: &str, start: &str) -> Result<(), Error> {
    let base = client.config.api_url.trim_end_matches('/');
    let object = format!("project_logs:{}", client.config.project_id);
    let payload = json!({"automation_id":id,"object_id":object});
    let response = client
        .post(format!("{base}/brainstore/automation/upsert-object-cursor"))
        .json(&payload)
        .retryable()
        .send()
        .map_err(Error::Http)?;
    check_response(response, "initialize regeneration cursor")?;
    let response = client
        .post(format!("{base}/brainstore/automation/reset-cursors"))
        .json(&json!({"automation_id":id,"object_id":object,"start_xact_id":start}))
        .retryable()
        .send()
        .map_err(Error::Http)?;
    if check_response(response, "regenerate Topics")?["success"] != true {
        return Err(Error::Api("Topics regeneration was not queued".to_owned()));
    }
    Ok(())
}

fn lookup_function(
    client: &Client,
    slug: &str,
    name: &str,
    function_type: &str,
    data_type: &str,
) -> Result<Option<Value>, Error> {
    let config = &client.config;
    let base = config.api_url.trim_end_matches('/');
    let project = config.project_id.to_string();
    let objects = get_objects(
        client,
        &format!("{base}/v1/function"),
        &[("project_id", project.as_str()), ("slug", slug), ("limit", "2")],
    )?;
    let existing = single(objects, "function", slug)?;
    if let Some(function) = &existing {
        if function["project_id"] != project
            || function["slug"] != slug
            || function["name"] != name
            || function["function_type"] != function_type
            || function["function_data"]["type"] != data_type
        {
            return Err(Error::Api(format!(
                "function slug {slug:?} conflicts with an existing resource"
            )));
        }
        object_id(function)?;
    }
    Ok(existing)
}

fn plan_function(
    name: &str,
    slug: &str,
    function_type: &'static str,
    existing: Option<Value>,
    data: Value,
    description: Value,
) -> Result<FunctionPlan, Error> {
    let action = match &existing {
        None => Action::Create,
        Some(function) if function["function_data"] == data && function["description"] == description => Action::Unchanged,
        Some(_) => Action::Update,
    };
    let id = match &existing {
        Some(function) => object_id(function)?.to_owned(),
        None => format!("bts:new:{slug}"),
    };
    Ok(FunctionPlan {
        action,
        name: name.to_owned(),
        slug: slug.to_owned(),
        function_type,
        id,
        existing,
        data,
        description,
    })
}

fn object_id(value: &Value) -> Result<&str, Error> {
    value["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Api("Braintrust resource has no id".to_owned()))
}

fn resolve_ids(value: &mut Value, ids: &HashMap<String, String>) {
    match value {
        Value::Object(object) => {
            if object.get("type").is_some_and(|kind| kind == "function")
                && let Some(id) = object.get("id").and_then(Value::as_str)
                && let Some(resolved) = ids.get(id)
            {
                object.insert("id".to_owned(), json!(resolved));
            }
            for value in object.values_mut() {
                resolve_ids(value, ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                resolve_ids(value, ids);
            }
        }
        _ => {}
    }
}

fn print_action(action: Action, name: &str, dry_run: bool) {
    let verb = match (action, dry_run) {
        (Action::Unchanged, _) => "unchanged",
        (Action::Create, true) => "would create",
        (Action::Update, true) => "would update",
        (Action::Create, false) => "created",
        (Action::Update, false) => "updated",
    };
    println!("{verb} {name}");
}
