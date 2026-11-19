use crate::dsl::{Experiment, Model};
use crate::mocking::data::Format as _;
use crate::mocking::{code, data};
use crate::providers::braintrust::features::datasets;
use crate::providers::braintrust::mocking::data as braintrust_data;
use crate::{
    providers::braintrust::client::{self, Client, writer},
    selection::ResourceSelector,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::Path,
    time::{Instant, SystemTime},
};
use uuid::Uuid;

#[derive(Debug)]
pub enum Error {
    Context { context: String, source: Box<Error> },
    Dataset(datasets::Error),
    Build(code::Error),
    Http(client::Error),
    Response(client::ApiError),
    Write(writer::Error),
    Generate(data::Error),
    Format(braintrust_data::Error),
    Io(std::io::Error),
    Json(serde_json::Error),

    InvalidData(String),
    Selection(String),
    MissingBaseline { experiment: String, baseline: String },
    Dependency { kind: String, name: String, count: usize },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Dataset(source) => source.fmt(f),
            Self::Build(source) => source.fmt(f),
            Self::Http(source) => source.fmt(f),
            Self::Response(source) => source.fmt(f),
            Self::Write(source) => source.fmt(f),
            Self::Generate(source) => source.fmt(f),
            Self::Format(source) => source.fmt(f),
            Self::Io(source) => source.fmt(f),
            Self::Json(source) => source.fmt(f),

            Self::InvalidData(message) | Self::Selection(message) => f.write_str(message),
            Self::MissingBaseline { experiment, baseline } => write!(
                f,
                "experiment {experiment:?} requires baseline {baseline:?} in the same write"
            ),
            Self::Dependency { kind, name, count } => write!(f, "expected one synced {kind} {name:?}, found {count}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Context { source, .. } => Some(source.as_ref()),
            Self::Dataset(source) => Some(source),
            Self::Build(source) => Some(source),
            Self::Http(source) => Some(source),
            Self::Response(source) => Some(source),
            Self::Write(source) => Some(source),
            Self::Generate(source) => Some(source),
            Self::Format(source) => Some(source),
            Self::Io(source) => Some(source),
            Self::Json(source) => Some(source),
            _ => None,
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidData(message.into())
}

struct Snapshot {
    id: String,
    // the project its row ids were made for, which is how a row is traced back to its case
    project: String,
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
    batch: data::EventBatch,
    events: Vec<Value>,
}
struct Plan<'a> {
    definition: &'a Experiment,
    dataset_id: String,
    dataset_version: Option<String>,
    results: Vec<ResultTrace>,
}

/// Checks experiment selectors and required baselines without accessing files or services.
/// Empty selectors include every experiment. Returns an error when no experiments
/// exist, a selector is invalid, or a selected experiment's baseline is not selected.
pub fn validate_selection(model: &Model, selectors: &[ResourceSelector]) -> Result<(), Error> {
    select(model, selectors).map(|_| ())
}

/// Generates a preview from local dataset definitions without invoking scorers or making API calls.
/// Uses `path` to resolve attachment paths and `seed` and `now` for local generation.
/// The returned JSON includes the selected experiments and their generated result events.
/// Returns selection, generation, attachment path, or encoding errors before returning a preview.
pub fn preview(model: &Model, selectors: &[ResourceSelector], path: &Path, seed: u64, now: SystemTime) -> Result<Value, Error> {
    let selected = select(model, selectors)?;
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
                    project: datasets::PREVIEW_PROJECT.to_owned(),
                    version: None,
                    rows: datasets::preview_rows(dataset, model, path, now).map_err(Error::from)?,
                },
            );
        }
    }
    let plans = prepare(model, &selected, &snapshots, path, seed, now)?;
    let preview = plans.iter().map(|plan| json!({
                "name": plan.definition.name, "description": plan.definition.description,
                "dataset": plan.definition.dataset, "baseline": plan.definition.baseline,
                "scorers": plan.definition.scorers,
                "results": plan.results.iter().map(|result| json!({"case_id": result.row["id"], "events": result.events})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>();
    Ok(json!({"seed":seed, "dataset_source":"local", "scorers_executed":false, "experiments":preview}))
}

/// The write status of an experiment created during a run.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Status {
    /// The experiment was created, but its results were not fully acknowledged.
    Incomplete,
    /// The write completed with these acknowledged result and span counts.
    Complete { results: usize, spans: usize },
}

/// The known result of creating and writing one experiment.
#[derive(Debug, serde::Serialize)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub definition: String,
    #[serde(flatten)]
    pub status: Status,
    pub dataset_id: String,
    pub dataset_version: Option<String>,
}

/// Results retained after experiment creation and result upload have been attempted.
pub struct Outcome {
    /// Experiments with acknowledged IDs, including any whose result upload failed.
    pub summaries: Vec<Summary>,
    /// Whether all selected experiments and their results were written successfully.
    /// A failure does not roll back earlier writes or remove the recorded experiments.
    pub result: Result<(), Error>,
}

/// Generates results from synced dataset rows, invokes deployed scorers, and writes new experiments.
/// Required baselines must be selected and are written before dependent experiments.
/// Uses `path`, `seed`, and `now` for local generation, then reports the planned experiment
/// and scorer call counts before uploading attachments or invoking scorers.
///
/// Returns an outer error if selection, dependency loading, preparation, attachment upload,
/// or scoring fails. Attachments or scorer calls may already have completed in those cases.
/// Once experiment creation starts, returns an [`Outcome`] containing the known experiment
/// IDs and any creation or result upload error. Callers must inspect [`Outcome::result`].
pub fn run(
    model: &Model,
    selectors: &[ResourceSelector],
    path: &Path,
    seed: u64,
    now: SystemTime,
    client: &Client,
    report: &mut dyn FnMut(usize, usize),
) -> Result<Outcome, Error> {
    let selected = select(model, selectors)?;
    let Dependencies { snapshots, functions } = dependencies(client, model, &selected)?;
    let mut plans = prepare(model, &selected, &snapshots, path, seed, now)?;
    let attachments = plans
        .iter()
        .flat_map(|plan| &plan.results)
        .flat_map(|result| result.batch.attachments.iter())
        .map(|attachment| (attachment.key.clone(), attachment.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    writer::validate_attachments(client, &attachments).map_err(Error::from)?;
    let calls: usize = plans
        .iter()
        .map(|plan| plan.results.len() * plan.definition.scorers.len())
        .sum();
    report(plans.len(), calls);
    writer::upload(client, &attachments).map_err(Error::from)?;
    score(client, &mut plans, &functions)?;
    let mut summaries = Vec::new();
    let outcome = upload(client, &plans, seed, &mut summaries);
    Ok(Outcome {
        summaries,
        result: outcome,
    })
}

fn select<'a>(model: &'a Model, selectors: &[ResourceSelector]) -> Result<Vec<&'a Experiment>, Error> {
    if model.experiments.is_empty() {
        return Err(invalid("module declares no experiment blocks"));
    }
    let mut names = Vec::new();
    if selectors.is_empty() {
        names.extend(model.experiments.iter().map(|experiment| experiment.name.as_str()));
    } else {
        for selector in selectors {
            if selector.kind != "experiment" {
                return Err(Error::Selection("expected an experiment traversal".to_owned()));
            }
            if !model.experiments.iter().any(|experiment| experiment.name == selector.name) {
                return Err(Error::Selection(format!("unknown experiment {:?}", selector.name)));
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
                return Err(Error::MissingBaseline {
                    experiment: name.to_owned(),
                    baseline: baseline.clone(),
                });
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
    let project = client.config().project_id.to_string();
    let response = client::checked(
        client
            .get(format!("/v1/{kind}"))
            .query(&[("project_id", project.as_str()), (key, value), ("limit", "2")])
            .send()
            .map_err(Error::from)?,
        "resolve experiment dependency",
    )
    .map_err(Error::from)?;
    let objects = response["objects"]
        .as_array()
        .ok_or_else(|| invalid("dependency lookup returned no objects"))?;
    if objects.len() != 1 {
        return Err(Error::Dependency {
            kind: kind.to_owned(),
            name: name.to_owned(),
            count: objects.len(),
        });
    }
    let object = &objects[0];
    if object["project_id"] != project
        || object[if key.ends_with("_name") { "name" } else { key }] != value
        || object["id"].as_str().is_none_or(str::is_empty)
    {
        return Err(invalid(format!("{kind} lookup returned a different resource for {name:?}")));
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
    let slugs = crate::providers::braintrust::mocking::code::resolve_slugs(&scorers).map_err(Error::from)?;
    for (scorer, slug) in scorers.iter().zip(slugs) {
        let function = lookup(client, "function", &scorer.name, "slug", &slug)?;
        if function["function_type"] != "scorer" {
            return Err(invalid(format!("function {:?} is not a scorer", scorer.name)));
        }
        functions.insert(
            scorer.name.clone(),
            Function {
                id: function["id"].as_str().unwrap().to_owned(),
                version: function["_xact_id"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid(format!("scorer {:?} returned no version", scorer.name)))?
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
        let (version, rows) = datasets::fetch_snapshot(client, &id).map_err(Error::from)?;
        let rows = rows
            .into_values()
            .filter(|row| row["_object_delete"] != true)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Err(invalid(format!("dataset {:?} has no rows", experiment.dataset)));
        }
        if version.is_none() {
            return Err(invalid(format!(
                "dataset {:?} returned no snapshot version",
                experiment.dataset
            )));
        }
        let project = client.config().project_id.to_string();
        snapshots.insert(
            experiment.dataset.clone(),
            Snapshot {
                id,
                project,
                version,
                rows,
            },
        );
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
                return Err(invalid(format!("dataset {:?} has no cases", experiment.dataset)));
            }
            // a row is the case that syncs to its id, and a row no case syncs to was added to the dataset by hand
            let declared = (model.datasets.iter())
                .find(|dataset| dataset.name == experiment.dataset)
                .map(|dataset| {
                    (dataset.cases.iter().enumerate())
                        .map(|(position, case)| {
                            let id = datasets::row_id(&snapshot.project, &dataset.name, &case.name);
                            (id, (position, case.name.as_str()))
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            // cases come in the order declared and other rows after them by id, so a preview and a write place a row alike
            let mut rows = (snapshot.rows.iter())
                .map(|row| (declared.get(row["id"].as_str().unwrap_or("")).copied(), row))
                .collect::<Vec<_>>();
            rows.sort_by(|(left_case, left), (right_case, right)| {
                let place = |case: &Option<(usize, &str)>| case.map_or((true, 0), |(position, _)| (false, position));
                (place(left_case), left["id"].as_str()).cmp(&(place(right_case), right["id"].as_str()))
            });
            let mut results = Vec::new();
            for (index, (case, row)) in rows.into_iter().enumerate() {
                if row.get("input").is_none() {
                    return Err(invalid(format!("dataset row {} has no input", row["id"])));
                }
                if row
                    .get("metadata")
                    .is_some_and(|metadata| !metadata.is_object() && !metadata.is_null())
                    || row.get("tags").is_some_and(|tags| {
                        !tags.is_null() && tags.as_array().is_none_or(|tags| tags.iter().any(|tag| !tag.is_string()))
                    })
                {
                    return Err(invalid(format!("dataset row {} has invalid metadata or tags", row["id"])));
                }
                // a row is drawn by the name of its case, which a preview and a synced dataset share, and otherwise by its id
                let identity = match case {
                    Some((_, name)) => format!("{}/case/{name}", experiment.dataset),
                    None => format!("{}/{}", experiment.dataset, row["id"].as_str().unwrap_or("")),
                };
                let row_seed = seed ^ u64::from_le_bytes(data::stable_uuid(&identity).as_bytes()[..8].try_into().unwrap());
                let mut batch =
                    data::experiment_result(model, experiment.task, row, index, row_seed, now, &braintrust_data::Format)
                        .map_err(|error| Error::Context {
                            context: format!("experiment {:?}, row {}", experiment.name, row["id"]),
                            source: Box::new(error.into()),
                        })?;
                batch.resolve_attachment_paths(path).map_err(Error::from)?;
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
                let events = serde_json::to_value(braintrust_data::Format.encode(&batch).map_err(Error::Format)?)
                    .map_err(Error::from)?["events"]
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
                return Err(invalid("scorer returned no scores"));
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
            return Err(invalid(format!("scorer {fallback:?} returned an invalid score")));
        }
        if scores.insert(name.to_owned(), score.clone()).is_some() {
            return Err(invalid(format!("duplicate score name {name:?}")));
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
                        .map_err(|error| Error::Context {
                            context: context.clone(),
                            source: Box::new(error.into()),
                        })?,
                    &context,
                )
                .map_err(Error::from)?;
                let returned = scores(&response, name).map_err(|error| Error::Context {
                    context: context.clone(),
                    source: Box::new(error),
                })?;
                for (name, value) in &returned {
                    if recorded.insert(name.clone(), value.clone()).is_some() {
                        return Err(invalid(format!("{context}: multiple scorers returned score {name:?}")));
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

fn upload(client: &Client, plans: &[Plan<'_>], seed: u64, summaries: &mut Vec<Summary>) -> Result<(), Error> {
    let run = Uuid::new_v4().to_string();
    let mut ids = HashMap::new();
    for plan in plans {
        let name = format!("{}-{}", plan.definition.name, &run[..8]);
        let context = format!("experiment {:?}, run {name:?}", plan.definition.name);
        let mut payload = json!({"project_id":client.config().project_id.to_string(),"name":name,"dataset_id":plan.dataset_id,"dataset_version":plan.dataset_version,"ensure_new":true,"metadata":{"synthetic":true,"bts_seed":seed,"bts_experiment":plan.definition.name,"bts_run":run}});
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
                .map_err(|error| Error::Context {
                    context: context.clone(),
                    source: Box::new(error.into()),
                })?,
            &format!("{context}: create experiment"),
        )
        .map_err(Error::from)?;
        let id = experiment["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid(format!("{context}: created experiment has no id")))?;
        ids.insert(plan.definition.name.clone(), id.to_owned());
        summaries.push(Summary {
            id: id.to_owned(),
            name,
            definition: plan.definition.name.clone(),
            status: Status::Incomplete,
            dataset_id: plan.dataset_id.clone(),
            dataset_version: plan.dataset_version.clone(),
        });
        let mut events = Vec::new();
        for result in &plan.results {
            let mut trace = result.events.clone();
            trace[0]["origin"] = json!({"object_type":"dataset","object_id":plan.dataset_id,"id":result.row["id"],"_xact_id":result.row["_xact_id"]});
            events.extend(trace);
        }
        writer::insert_events(
            client,
            &format!("{}/v1/experiment/{id}/insert", client.config().api_url.trim_end_matches('/')),
            &events,
        )
        .map_err(|error| Error::Context {
            context: format!("{context} (experiment {id}): insert results"),
            source: Box::new(error.into()),
        })?;
        let summary = summaries.last_mut().unwrap();
        summary.status = Status::Complete {
            results: plan.results.len(),
            spans: events.len(),
        };
    }
    tracing::info!(experiments = summaries.len(), "experiment write finished");
    Ok(())
}

impl From<datasets::Error> for Error {
    fn from(source: datasets::Error) -> Self {
        Self::Dataset(source)
    }
}
impl From<code::Error> for Error {
    fn from(source: code::Error) -> Self {
        Self::Build(source)
    }
}
impl From<client::Error> for Error {
    fn from(source: client::Error) -> Self {
        Self::Http(source)
    }
}
impl From<client::ApiError> for Error {
    fn from(source: client::ApiError) -> Self {
        Self::Response(source)
    }
}
impl From<writer::Error> for Error {
    fn from(source: writer::Error) -> Self {
        Self::Write(source)
    }
}
impl From<data::Error> for Error {
    fn from(source: data::Error) -> Self {
        Self::Generate(source)
    }
}
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}
impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::InvalidData(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::braintrust::client::tests::{Reply, serve};

    const SOURCE: &str = r#"(old_root:task {name: "old"}) {input: old.input, output: "wrong"};
(old:agent) {input: "fallback", output: old_root.output};
(old)-|:next|->(old_root);
(new_root:task {name: "new"}) {input: new.input, output: new_root.input};
(new:agent) {input: "fallback", output: new_root.output};
(new)-|:next|->(new_root);
(dataset_cases_local:case {name: "local"}) {input: "local value"};
(dataset_cases:dataset {name: "cases"}) {cases: [(dataset_cases_local)]};
("quality":scorer) {score: context.input == context.output ? 1 : 0};
(experiment_fixed:experiment {name: "fixed"}) {dataset: (dataset_cases), task: (new_root), scorers: [("quality")], baseline: (experiment_baseline)};
(experiment_baseline:experiment {name: "baseline"}) {dataset: (dataset_cases), task: (old_root), scorers: [("quality")]};"#;

    fn body(request: &str) -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn selection_is_strict_and_orders_selected_baselines_first() {
        let model = crate::providers::braintrust::mocking::compile(SOURCE).unwrap();
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
            .replace("output: \"wrong\"", "output: attachment(\"photo.png\", \"image/png\")")
            .replace("output: new_root.input", "output: attachment(\"photo.png\", \"image/png\")");
        let model = crate::providers::braintrust::mocking::compile(&source).unwrap();
        let selected = select(&model, &[]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                project: "project".to_owned(),
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
        let model = crate::providers::braintrust::mocking::compile(SOURCE).unwrap();
        let selected = select(&model, &["experiment.baseline".parse().unwrap()]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                project: "project".to_owned(),
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
        let model = crate::providers::braintrust::mocking::compile(SOURCE).unwrap();
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
    fn a_preview_draws_what_a_write_draws_from_the_synced_rows() {
        // the task samples and reads its place in the run, so a row drawn or placed differently would show
        let source = r#"(root:task {name: "answer"}) {input: answer.input, output: {code: hex(8), place: context.trace_index, said: root.input}};
(answer:agent) {input: "fallback", output: root.output};
(answer)-|:next|->(root);
(first:case {name: "first"}) {input: hex(6)};
(second:case {name: "second"}) {input: hex(6)};
(third:case {name: "third"}) {input: hex(6)};
(cases:dataset {name: "cases"}) {cases: [(first), (second), (third)]};
("quality":scorer) {score: 1};
(run:experiment {name: "run"}) {dataset: (cases), task: (root), scorers: [("quality")]};"#;
        let model = crate::providers::braintrust::mocking::compile(source).unwrap();
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(7200);
        let path = Path::new("shape.bt");
        let preview = preview(&model, &[], path, 42, now).unwrap();
        let previewed = preview["experiments"][0]["results"].as_array().unwrap();
        // the rows as a project holds them: under its own ids, in the order of those ids, with one added by hand
        let dataset = &model.datasets[0];
        let mut rows = datasets::preview_rows(dataset, &model, path, now).unwrap();
        for (row, case) in rows.iter_mut().zip(&dataset.cases) {
            row["id"] = json!(datasets::row_id("project-1", &dataset.name, &case.name));
        }
        rows.push(json!({"id": "added-by-hand", "input": "typed in"}));
        rows.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        let snapshot = Snapshot {
            id: "dataset".to_owned(),
            project: "project-1".to_owned(),
            version: Some("10".to_owned()),
            rows,
        };
        let selected = select(&model, &[]).unwrap();
        let snapshots = HashMap::from([("cases".to_owned(), snapshot)]);
        let plans = prepare(&model, &selected, &snapshots, path, 42, now).unwrap();
        let written = &plans[0].results;
        assert_eq!(written.len(), 4);
        // a span id is made new on every run, and everything else is drawn
        let drawn = |events: &Value| {
            let mut events = events.clone();
            for event in events.as_array_mut().unwrap() {
                for key in ["id", "span_id", "root_span_id", "span_parents"] {
                    event.as_object_mut().unwrap().remove(key);
                }
            }
            events
        };
        let mut codes = HashSet::new();
        for (index, (written, previewed)) in written.iter().zip(previewed).enumerate() {
            assert_eq!(drawn(&json!(written.events)), drawn(&previewed["events"]), "row {index}");
            assert_eq!(written.events[0]["output"]["place"], index);
            assert_eq!(written.events[0]["output"]["said"], written.row["input"]);
            codes.insert(written.events[0]["output"]["code"].as_str().unwrap().to_owned());
        }
        assert_eq!(codes.len(), 3);
        // the row no case syncs to comes last and is drawn by its id
        assert_eq!(written[3].row["id"], "added-by-hand");
        assert_eq!(written[3].events[0]["output"]["place"], 3);
    }

    #[test]
    fn missing_deployed_scorer_fails_before_creating_any_experiment() {
        let model = crate::providers::braintrust::mocking::compile(SOURCE).unwrap();
        let selected = select(&model, &[]).unwrap();
        let (client, requests) = serve(vec![Reply::json(json!({"objects":[]}))]);
        assert!(dependencies(&client, &model, &selected).is_err());
        assert!(requests.recv().unwrap().starts_with("GET /v1/function?"));
    }

    #[test]
    fn failed_insert_reports_completed_and_incomplete_experiments() {
        let model = crate::providers::braintrust::mocking::compile(SOURCE).unwrap();
        let selected = select(&model, &[]).unwrap();
        let snapshots = HashMap::from([(
            "cases".to_owned(),
            Snapshot {
                project: "project".to_owned(),
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
        assert_eq!(summaries[0].id, "baseline-id");
        assert!(matches!(summaries[0].status, Status::Complete { results: 1, .. }));
        assert_eq!(summaries[1].id, "fixed-id");
        assert_eq!(summaries[1].status, Status::Incomplete);
        let encoded = serde_json::to_value(&summaries).unwrap();
        assert_eq!(encoded[0]["status"], "complete");
        assert_eq!(encoded[0]["results"], 1);
        assert_eq!(encoded[1]["status"], "incomplete");
        assert!(encoded[1].get("results").is_none());
        assert_eq!(requests.into_iter().count(), 4);
    }
}
