use crate::cmd::{
    logging, render_diags,
    shared::{
        client::{self, Client, writer},
        select::ResourceSelector,
    },
    sync::datasets,
};
use crate::conf::{Braintrust, Settings};
use crate::dsl::{self, Experiment, Model};
use crate::{scg, sdg};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};
use uuid::Uuid;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// bts module containing experiments, datasets, tasks, and scorers
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// select an experiment, repeat for more; include any referenced baseline
    #[arg(long, value_name = "TRAVERSAL")]
    select: Vec<ResourceSelector>,
    /// seed for task generation; does not control deployed scorer randomness
    #[arg(long)]
    seed: Option<u64>,
    /// preview local dataset cases and generated tasks, without invoking scorers
    #[arg(long)]
    dry_run: bool,
    /// print the final run summary as JSON
    #[arg(long, conflicts_with = "dry_run")]
    json: bool,
    /// print phase timings to stderr
    #[arg(long)]
    profile: bool,
}

#[derive(Debug)]
pub struct Error(String);
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
fn other(error: impl fmt::Display) -> Error {
    Error(error.to_string())
}

struct Snapshot {
    id: String,
    version: Option<String>,
    rows: Vec<Value>,
}
struct Function {
    id: String,
    version: String,
}
struct Dependencies {
    snapshots: HashMap<String, Snapshot>,
    functions: HashMap<String, Function>,
}
struct ResultTrace {
    row: Value,
    batch: sdg::EventBatch,
    events: Vec<Value>,
}
struct Plan<'a> {
    definition: &'a Experiment,
    dataset_id: String,
    dataset_version: Option<String>,
    results: Vec<ResultTrace>,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let settings = Settings::load().map_err(other)?;
        let log_path = logging::init("write-experiments", self.profile, &settings);
        let result = self.execute(&settings);
        if let Err(error) = &result {
            tracing::error!(%error, "experiment write failed");
            if let Some(path) = log_path {
                eprintln!("run log: {}", path.display());
            }
        }
        result
    }

    fn execute(self, settings: &Settings) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(other)?;
        let model =
            dsl::compile(&source).map_err(|errors| other(render_diags(&self.from.display().to_string(), &source, &errors)))?;
        let selected = select(&model, &self.select)?;
        let seed = self.seed.unwrap_or_else(rand::random);
        eprintln!("seed: {seed}");
        let now = SystemTime::now();
        if self.dry_run {
            let mut snapshots = HashMap::new();
            for experiment in &selected {
                if !snapshots.contains_key(&experiment.dataset) {
                    let dataset = model
                        .datasets
                        .iter()
                        .find(|dataset| dataset.name == experiment.dataset)
                        .unwrap();
                    snapshots.insert(
                        experiment.dataset.clone(),
                        Snapshot {
                            id: experiment.dataset.clone(),
                            version: None,
                            rows: datasets::preview_rows(dataset, &model, &self.from).map_err(other)?,
                        },
                    );
                }
            }
            let plans = prepare(&model, &selected, &snapshots, &self.from, seed, now)?;
            let preview = plans.iter().map(|plan| json!({
                "name": plan.definition.name, "description": plan.definition.description,
                "dataset": plan.definition.dataset, "baseline": plan.definition.baseline,
                "scorers": plan.definition.scorers,
                "results": plan.results.iter().map(|result| json!({"case_id": result.row["id"], "events": result.events})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"seed":seed, "dataset_source":"local", "scorers_executed":false, "experiments":preview})
                )
                .map_err(other)?
            );
            return Ok(());
        }
        let client = Client::configured(Braintrust::load().map_err(other)?, settings).map_err(other)?;
        let Dependencies { snapshots, functions } = dependencies(&client, &model, &selected)?;
        let mut plans = prepare(&model, &selected, &snapshots, &self.from, seed, now)?;
        let attachments = plans
            .iter()
            .flat_map(|plan| &plan.results)
            .flat_map(|result| result.batch.attachments.iter())
            .map(|attachment| (attachment.key.clone(), attachment.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        writer::validate_attachments(&client, &attachments).map_err(other)?;
        let calls: usize = plans
            .iter()
            .map(|plan| plan.results.len() * plan.definition.scorers.len())
            .sum();
        eprintln!("generating {} experiments; invoking scorers {calls} times", plans.len());
        writer::upload(&client, &attachments).map_err(other)?;
        score(&client, &mut plans, &functions)?;
        let mut summaries = Vec::new();
        let outcome = upload(&client, &plans, seed, &mut summaries);
        if self.json {
            let mut summary = json!({"seed":seed,"experiments":summaries});
            if let Err(error) = &outcome {
                summary["error"] = json!(error.to_string());
            }
            println!("{summary}");
        } else {
            for summary in summaries {
                if summary["status"] == "complete" {
                    println!(
                        "wrote {}: {} results, {} spans (experiment {})",
                        summary["name"].as_str().unwrap(),
                        summary["results"],
                        summary["spans"],
                        summary["id"].as_str().unwrap()
                    );
                } else {
                    println!(
                        "created {} (experiment {}); results may be incomplete",
                        summary["name"].as_str().unwrap(),
                        summary["id"].as_str().unwrap()
                    );
                }
            }
        }
        outcome
    }
}

fn select<'a>(model: &'a Model, selectors: &[ResourceSelector]) -> Result<Vec<&'a Experiment>, Error> {
    if model.experiments.is_empty() {
        return Err(other("module declares no experiment blocks"));
    }
    let mut names = Vec::new();
    if selectors.is_empty() {
        names.extend(model.experiments.iter().map(|experiment| experiment.name.as_str()));
    } else {
        for selector in selectors {
            if selector.kind != "experiment" {
                return Err(other("--select expects an experiment traversal"));
            }
            if !model.experiments.iter().any(|experiment| experiment.name == selector.name) {
                return Err(other(format!("--select: unknown experiment {:?}", selector.name)));
            }
            names.push(selector.name.as_str());
        }
    }
    fn visit<'a>(
        name: &str,
        names: &[&str],
        model: &'a Model,
        seen: &mut HashSet<String>,
        selected: &mut Vec<&'a Experiment>,
    ) -> Result<(), Error> {
        if !seen.insert(name.to_owned()) {
            return Ok(());
        }
        let experiment = model.experiments.iter().find(|experiment| experiment.name == name).unwrap();
        if let Some(baseline) = &experiment.baseline {
            if !names.contains(&baseline.as_str()) {
                return Err(other(format!(
                    "experiment {name:?} requires baseline {baseline:?} in the same write; add --select 'experiment[\"{baseline}\"]'"
                )));
            }
            visit(baseline, names, model, seen, selected)?;
        }
        selected.push(experiment);
        Ok(())
    }
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for name in &names {
        visit(name, &names, model, &mut seen, &mut selected)?;
    }
    Ok(selected)
}

fn lookup(client: &Client, kind: &str, name: &str, key: &str, value: &str) -> Result<Value, Error> {
    let project = client.config.project_id.to_string();
    let response = client::checked(
        client
            .get(format!("/v1/{kind}"))
            .query(&[("project_id", project.as_str()), (key, value), ("limit", "2")])
            .send()
            .map_err(other)?,
        "resolve experiment dependency",
    )
    .map_err(other)?;
    let objects = response["objects"]
        .as_array()
        .ok_or_else(|| other("dependency lookup returned no objects"))?;
    if objects.len() != 1 {
        return Err(other(format!(
            "expected one synced {kind} {name:?}, found {}; sync datasets and build/push scorers before writing experiments",
            objects.len()
        )));
    }
    let object = &objects[0];
    if object["project_id"] != project
        || object[if key.ends_with("_name") { "name" } else { key }] != value
        || object["id"].as_str().is_none_or(str::is_empty)
    {
        return Err(other(format!("{kind} lookup returned a different resource for {name:?}")));
    }
    Ok(object.clone())
}

fn dependencies(client: &Client, model: &Model, selected: &[&Experiment]) -> Result<Dependencies, Error> {
    let mut snapshots = HashMap::new();
    let mut functions = HashMap::new();
    let scorers = model
        .scorers
        .iter()
        .filter(|scorer| selected.iter().any(|experiment| experiment.scorers.contains(&scorer.name)))
        .cloned()
        .collect::<Vec<_>>();
    let slugs = scg::resolve_slugs(&scorers).map_err(other)?;
    for (scorer, slug) in scorers.iter().zip(slugs) {
        let function = lookup(client, "function", &scorer.name, "slug", &slug)?;
        if function["function_type"] != "scorer" {
            return Err(other(format!("function {:?} is not a scorer", scorer.name)));
        }
        functions.insert(
            scorer.name.clone(),
            Function {
                id: function["id"].as_str().unwrap().to_owned(),
                version: function["_xact_id"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| other(format!("scorer {:?} returned no version", scorer.name)))?
                    .to_owned(),
            },
        );
    }
    for experiment in selected {
        if snapshots.contains_key(&experiment.dataset) {
            continue;
        }
        let dataset = lookup(client, "dataset", &experiment.dataset, "dataset_name", &experiment.dataset)?;
        let id = dataset["id"].as_str().unwrap().to_owned();
        let (version, rows) = datasets::fetch_snapshot(client, &id).map_err(other)?;
        let mut rows = rows
            .into_values()
            .filter(|row| row["_object_delete"] != true)
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        if rows.is_empty() {
            return Err(other(format!("dataset {:?} has no rows", experiment.dataset)));
        }
        if version.is_none() {
            return Err(other(format!(
                "dataset {:?} returned no snapshot version",
                experiment.dataset
            )));
        }
        snapshots.insert(experiment.dataset.clone(), Snapshot { id, version, rows });
    }
    Ok(Dependencies { snapshots, functions })
}

fn prepare<'a>(
    model: &Model,
    selected: &[&'a Experiment],
    snapshots: &HashMap<String, Snapshot>,
    path: &Path,
    seed: u64,
    now: SystemTime,
) -> Result<Vec<Plan<'a>>, Error> {
    let mut uploads: HashMap<(String, String), String> = HashMap::new();
    selected
        .iter()
        .map(|experiment| {
            let snapshot = &snapshots[&experiment.dataset];
            if snapshot.rows.is_empty() {
                return Err(other(format!("dataset {:?} has no cases", experiment.dataset)));
            }
            let mut results = Vec::new();
            for (index, row) in snapshot.rows.iter().enumerate() {
                if row.get("input").is_none() {
                    return Err(other(format!("dataset row {} has no input", row["id"])));
                }
                if row
                    .get("metadata")
                    .is_some_and(|metadata| !metadata.is_object() && !metadata.is_null())
                    || row.get("tags").is_some_and(|tags| {
                        !tags.is_null() && tags.as_array().is_none_or(|tags| tags.iter().any(|tag| !tag.is_string()))
                    })
                {
                    return Err(other(format!("dataset row {} has invalid metadata or tags", row["id"])));
                }
                // stable row identity keeps generation independent of selectors and pagination order
                let row_seed = seed
                    ^ u64::from_le_bytes(
                        sdg::stable_uuid(&format!("{}/{}", experiment.dataset, row["id"].as_str().unwrap_or(""))).as_bytes()
                            [..8]
                            .try_into()
                            .unwrap(),
                    );
                let mut batch = sdg::experiment_result(model, experiment.task, row, index, row_seed, now)
                    .map_err(|error| other(format!("experiment {:?}, row {}: {error}", experiment.name, row["id"])))?;
                batch.resolve_attachment_paths(path).map_err(other)?;
                let mut reuse = HashMap::new();
                for attachment in &batch.attachments {
                    let identity = (attachment.path.clone(), attachment.content_type.clone());
                    if let Some(key) = uploads.get(&identity) {
                        reuse.insert(attachment.key.clone(), key.clone());
                    } else {
                        uploads.insert(identity, attachment.key.clone());
                    }
                }
                batch.reuse_attachments(&reuse);
                let events = serde_json::to_value(&batch).map_err(other)?["events"]
                    .as_array()
                    .unwrap()
                    .clone();
                results.push(ResultTrace {
                    row: row.clone(),
                    batch,
                    events,
                });
            }
            Ok(Plan {
                definition: experiment,
                dataset_id: snapshot.id.clone(),
                dataset_version: snapshot.version.clone(),
                results,
            })
        })
        .collect()
}

fn scores(value: &Value, fallback: &str) -> Result<Map<String, Value>, Error> {
    fn add(value: &Value, fallback: &str, scores: &mut Map<String, Value>) -> Result<(), Error> {
        if let Some(values) = value.as_array() {
            if values.is_empty() {
                return Err(other("scorer returned no scores"));
            }
            for value in values {
                add(value, fallback, scores)?;
            }
            return Ok(());
        }
        let (name, score) = if value.is_number() {
            (fallback, value)
        } else {
            (value["name"].as_str().unwrap_or(fallback), &value["score"])
        };
        if name.is_empty()
            || score
                .as_f64()
                .is_none_or(|score| !score.is_finite() || !(0.0..=1.0).contains(&score))
        {
            return Err(other(format!("scorer {fallback:?} returned an invalid score")));
        }
        if scores.insert(name.to_owned(), score.clone()).is_some() {
            return Err(other(format!("duplicate score name {name:?}")));
        }
        Ok(())
    }
    let mut result = Map::new();
    add(value, fallback, &mut result)?;
    Ok(result)
}

fn score(client: &Client, plans: &mut [Plan<'_>], functions: &HashMap<String, Function>) -> Result<(), Error> {
    for plan in plans {
        for result in &mut plan.results {
            let root = result.events[0].clone();
            let input = json!({"input":root["input"],"output":root["output"],"expected":root["expected"],"metadata":root.get("metadata").cloned().unwrap_or_else(|| json!({}))});
            let mut recorded = Map::new();
            for name in &plan.definition.scorers {
                let context = format!(
                    "experiment {:?}, row {}, scorer {name:?}",
                    plan.definition.name, result.row["id"]
                );
                let function = &functions[name];
                let payload = json!({"input":input,"stream":false,"version":function.version});
                let started = Instant::now();
                let response = client::checked(
                    client
                        .post(format!("/v1/function/{}/invoke", function.id))
                        .json(&payload)
                        .send()
                        .map_err(|error| other(format!("{context}: {error}")))?,
                    &context,
                )
                .map_err(other)?;
                let returned = scores(&response, name).map_err(|error| other(format!("{context}: {error}")))?;
                for (name, value) in &returned {
                    if recorded.insert(name.clone(), value.clone()).is_some() {
                        return Err(other(format!("{context}: multiple scorers returned score {name:?}")));
                    }
                }
                let start = result
                    .events
                    .iter()
                    .filter_map(|event| event["metrics"]["end"].as_f64())
                    .fold(f64::NEG_INFINITY, f64::max);
                result.events.push(json!({
                    "id":Uuid::new_v4().to_string(), "span_id":Uuid::new_v4().to_string(), "root_span_id":root["root_span_id"], "span_parents":[root["span_id"]],
                    "span_attributes":{"name":name,"type":"scorer","purpose":"scorer"},
                    "input":input, "output":response, "scores":returned,
                    "metadata":{"function_id":function.id,"function_version":function.version},
                    "metrics":{"start":start,"end":start + started.elapsed().as_secs_f64()},
                }));
            }
            result.events[0]["scores"] = Value::Object(recorded);
        }
    }
    Ok(())
}

fn upload(client: &Client, plans: &[Plan<'_>], seed: u64, summaries: &mut Vec<Value>) -> Result<(), Error> {
    let run = Uuid::new_v4().to_string();
    let mut ids = HashMap::new();
    for plan in plans {
        let name = format!("{}-{}", plan.definition.name, &run[..8]);
        let context = format!("experiment {:?}, run {name:?}", plan.definition.name);
        let mut payload = json!({"project_id":client.config.project_id.to_string(),"name":name,"dataset_id":plan.dataset_id,"dataset_version":plan.dataset_version,"ensure_new":true,"metadata":{"synthetic":true,"bts_seed":seed,"bts_experiment":plan.definition.name,"bts_run":run}});
        if let Some(description) = &plan.definition.description {
            payload["description"] = json!(description);
        }
        if let Some(baseline) = &plan.definition.baseline {
            payload["base_exp_id"] = json!(ids[baseline]);
        }
        let experiment = client::checked(
            client
                .post("/v1/experiment")
                .json(&payload)
                .send()
                .map_err(|error| other(format!("{context}: {error}")))?,
            &format!("{context}: create experiment"),
        )
        .map_err(other)?;
        let id = experiment["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| other(format!("{context}: created experiment has no id")))?;
        ids.insert(plan.definition.name.clone(), id.to_owned());
        summaries.push(json!({"id":id,"name":name,"definition":plan.definition.name,"status":"incomplete","dataset_id":plan.dataset_id,"dataset_version":plan.dataset_version}));
        let mut events = Vec::new();
        for result in &plan.results {
            let mut trace = result.events.clone();
            trace[0]["origin"] = json!({"object_type":"dataset","object_id":plan.dataset_id,"id":result.row["id"],"_xact_id":result.row["_xact_id"]});
            events.extend(trace);
        }
        writer::insert_events(
            client,
            &format!("{}/v1/experiment/{id}/insert", client.config.api_url.trim_end_matches('/')),
            &events,
        )
        .map_err(|error| other(format!("{context} (experiment {id}): insert results: {error}")))?;
        let summary = summaries.last_mut().unwrap();
        summary["status"] = json!("complete");
        summary["results"] = json!(plan.results.len());
        summary["spans"] = json!(events.len());
    }
    tracing::info!(experiments = summaries.len(), "experiment write finished");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::shared::client::tests::{Reply, serve};

    const SOURCE: &str = r#"
        trace "old" { input = "fallback" output = "wrong" }
        trace "new" { input = "fallback" output = trace.input }
        dataset "cases" { case "local" { input = "local value" } }
        scorer "quality" { code { score = input == output ? 1 : 0 } }
        experiment "fixed" { dataset = dataset.cases task = trace.new scorers = [scorer.quality] baseline = experiment.baseline }
        experiment "baseline" { dataset = dataset.cases task = trace.old scorers = [scorer.quality] }
    "#;

    fn body(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn selection_is_strict_and_orders_selected_baselines_first() {
        let model = dsl::compile(SOURCE).unwrap();
        let selected = select(
            &model,
            &["experiment.fixed".parse().unwrap(), "experiment.baseline".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(
            selected.iter().map(|experiment| experiment.name.as_str()).collect::<Vec<_>>(),
            ["baseline", "fixed"]
        );
        let selected = select(&model, &["experiment.baseline".parse().unwrap()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert!(select(&model, &["experiment.fixed".parse().unwrap()]).is_err());
        assert!(select(&model, &["experiment.missing".parse().unwrap()]).is_err());
        assert!(select(&model, &["dataset.cases".parse().unwrap()]).is_err());
    }

    #[test]
    fn validates_numeric_named_and_multiple_scorer_results() {
        assert_eq!(
            scores(&json!(0.5), "fallback").unwrap(),
            Map::from_iter([("fallback".to_owned(), json!(0.5))])
        );
        assert_eq!(
            scores(
                &json!({"name":"quality","score":1,"metadata":{"reason":"correct"}}),
                "fallback"
            )
            .unwrap()["quality"],
            1
        );
        assert_eq!(
            scores(&json!([{"name":"a","score":0},{"name":"b","score":1}]), "fallback")
                .unwrap()
                .len(),
            2
        );
        for value in [
            json!(null),
            json!([]),
            json!(1.1),
            json!(-0.1),
            json!({"error":"failed"}),
            json!({"score":"1"}),
            json!([{"name":"a","score":0},{"name":"a","score":1}]),
        ] {
            assert!(scores(&value, "fallback").is_err(), "{value}");
        }
    }

    #[test]
    fn reuses_attachment_uploads_across_cases_and_experiments() {
        let source = SOURCE
            .replace("output = \"wrong\"", "output = attachment(\"photo.png\", \"image/png\")")
            .replace("output = trace.input", "output = attachment(\"photo.png\", \"image/png\")");
        let model = dsl::compile(&source).unwrap();
        let selected = select(&model, &[]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                id: "dataset".to_owned(),
                version: Some("10".to_owned()),
                rows: vec![json!({"id":"one","input":"one"}), json!({"id":"two","input":"two"})],
            },
        )]);
        let plans = prepare(&model, &selected, &snapshots, Path::new("shape.bt"), 42, SystemTime::now()).unwrap();
        let uploads = plans
            .iter()
            .flat_map(|plan| &plan.results)
            .flat_map(|result| result.batch.attachments.iter())
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 1);
        for result in plans.iter().flat_map(|plan| &plan.results) {
            assert_eq!(result.events[0]["output"]["key"], uploads[0].key);
        }
    }

    #[test]
    fn scorer_failure_prevents_experiment_creation() {
        let model = dsl::compile(SOURCE).unwrap();
        let selected = select(&model, &["experiment.baseline".parse().unwrap()]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                id: "dataset".to_owned(),
                version: Some("10".to_owned()),
                rows: vec![json!({"id":"one","input":"one"})],
            },
        )]);
        let mut plans = prepare(&model, &selected, &snapshots, Path::new("shape.bt"), 42, SystemTime::now()).unwrap();
        let functions = HashMap::from([(
            "quality".to_owned(),
            Function {
                id: "scorer".to_owned(),
                version: "20".to_owned(),
            },
        )]);
        let (client, requests) = serve(vec![Reply::json(json!({"error":"scorer failed"}))]);
        let error = score(&client, &mut plans, &functions).unwrap_err().to_string();
        assert!(error.contains("experiment \"baseline\""));
        assert!(error.contains("row \"one\""));
        assert!(error.contains("scorer \"quality\""));
        let requests = requests.into_iter().collect::<Vec<_>>();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /v1/function/scorer/invoke "));
    }

    #[test]
    fn invokes_scorers_and_uploads_linked_baseline_results_from_remote_rows() {
        let model = dsl::compile(SOURCE).unwrap();
        let selected = select(
            &model,
            &["experiment.fixed".parse().unwrap(), "experiment.baseline".parse().unwrap()],
        )
        .unwrap();
        let (client, requests) = serve(vec![
            Reply::json(
                json!({"objects":[{"id":"function","project_id":"$PROJECT","slug":"quality","function_type":"scorer","_xact_id":"20"}]}),
            ),
            Reply::json(json!({"objects":[{"id":"dataset","project_id":"$PROJECT","name":"cases"}]})),
            Reply::json(
                json!({"events":[{"id":"reviewed","input":"reviewed input","expected":"reviewed input","metadata":{"reviewed":true},"_xact_id":"10"}]}),
            ),
            Reply::json(json!({"name":"quality","score":0})),
            Reply::json(json!({"name":"quality","score":1,"metadata":{"reason":"matched"}})),
            Reply::json(json!({"id":"baseline-id"})),
            Reply::json(json!({"row_ids":["root","scorer"]})),
            Reply::json(json!({"id":"fixed-id"})),
            Reply::json(json!({"row_ids":["root","scorer"]})),
        ]);
        let Dependencies { snapshots, functions } = dependencies(&client, &model, &selected).unwrap();
        let mut plans = prepare(&model, &selected, &snapshots, Path::new("shape.bt"), 42, SystemTime::now()).unwrap();
        score(&client, &mut plans, &functions).unwrap();
        let mut summaries = Vec::new();
        upload(&client, &plans, 42, &mut summaries).unwrap();
        assert_eq!(summaries.len(), 2);
        let requests = requests.into_iter().collect::<Vec<_>>();
        assert_eq!(requests.len(), 9);
        let baseline_score = body(&requests[3]);
        assert!(requests[3].starts_with("POST /v1/function/function/invoke "));
        assert_eq!(baseline_score["version"], "20");
        assert_eq!(baseline_score["input"]["input"], "reviewed input");
        assert_eq!(baseline_score["input"]["output"], "wrong");
        assert_eq!(body(&requests[4])["input"]["output"], "reviewed input");
        let baseline_create = body(&requests[5]);
        assert_eq!(baseline_create["dataset_id"], "dataset");
        assert_eq!(baseline_create["dataset_version"], "10");
        assert_eq!(baseline_create["metadata"]["synthetic"], true);
        let fixed_create = body(&requests[7]);
        assert_eq!(fixed_create["base_exp_id"], "baseline-id");
        let results = body(&requests[8]);
        assert_eq!(results["events"][0]["scores"]["quality"], 1);
        assert_eq!(results["events"][0]["origin"]["id"], "reviewed");
        assert_eq!(results["events"][0]["origin"]["_xact_id"], "10");
        assert_eq!(results["events"][1]["span_attributes"]["purpose"], "scorer");
        assert_eq!(results["events"][1]["output"]["metadata"]["reason"], "matched");
        assert_eq!(results["events"][1]["span_parents"], json!([results["events"][0]["span_id"]]));
    }

    #[test]
    fn missing_deployed_scorer_fails_before_creating_any_experiment() {
        let model = dsl::compile(SOURCE).unwrap();
        let selected = select(&model, &[]).unwrap();
        let (client, requests) = serve(vec![Reply::json(json!({"objects":[]}))]);
        assert!(dependencies(&client, &model, &selected).is_err());
        assert!(requests.recv().unwrap().starts_with("GET /v1/function?"));
    }

    #[test]
    fn failed_insert_reports_completed_and_incomplete_experiments() {
        let model = dsl::compile(SOURCE).unwrap();
        let selected = select(&model, &[]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                id: "dataset".to_owned(),
                version: Some("10".to_owned()),
                rows: vec![json!({"id":"one","input":"question","_xact_id":"10"})],
            },
        )]);
        let plans = prepare(&model, &selected, &snapshots, Path::new("shape.bt"), 42, SystemTime::now()).unwrap();
        let (client, requests) = serve(vec![
            Reply::json(json!({"id":"baseline-id"})),
            Reply::json(json!({"row_ids":["root"]})),
            Reply::json(json!({"id":"fixed-id"})),
            Reply {
                status: 400,
                body: "invalid event".to_owned(),
                headers: String::new(),
            },
        ]);
        let mut summaries = Vec::new();
        let error = upload(&client, &plans, 42, &mut summaries).unwrap_err().to_string();
        assert!(error.contains("experiment \"fixed\""));
        assert!(error.contains("fixed-id"));
        assert!(error.contains("insert results"));
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0]["id"], "baseline-id");
        assert_eq!(summaries[0]["status"], "complete");
        assert_eq!(summaries[1]["id"], "fixed-id");
        assert_eq!(summaries[1]["status"], "incomplete");
        assert!(summaries[1].get("results").is_none());
        assert_eq!(requests.into_iter().count(), 4);
    }
}
