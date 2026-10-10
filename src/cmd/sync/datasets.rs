use crate::cmd::{
    render_diags,
    shared::{
        client::{self, Client, attachments::Comparison, writer},
        select,
        state::{ResourceKind, SyncState, TrackedResource},
    },
};
use crate::conf::{Braintrust, Settings};
use crate::dsl::{self, Accessor, Child, Dataset, DatasetSource, Model, NodeId, RefId, WriteFilter};
use crate::sdg::{self, EventBatch};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// bts module containing datasets and their source traces
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// show the reconciliation without writing to Braintrust
    #[arg(long)]
    dry_run: bool,
    /// select a dataset, repeat for more
    #[arg(long, value_name = "TRAVERSAL")]
    select: Vec<select::ResourceSelector>,
}
impl Args {
    pub fn run(self) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(other)?;
        let model = dsl::compile_module(&source, true)
            .map_err(|diags| other(render_diags(&self.from.display().to_string(), &source, &diags)))?;
        let client = Client::configured(Braintrust::load().map_err(other)?, &Settings::load().map_err(other)?)?;
        let mut state = SyncState::load(&self.from, &client.config.project_id.to_string(), self.dry_run).map_err(other)?;
        let (datasets, selected) = state.datasets(&model, &self.select).map_err(other)?;
        let removals = state.removals(&model, &ResourceKind::Dataset, &selected, self.select.is_empty());
        if datasets.is_empty() && removals.is_empty() {
            return Err(other("module declares no dataset blocks"));
        }
        // prepare every local case before accessing remote state
        let mut prepared = datasets
            .iter()
            .map(|dataset| prepare(dataset, &model, &client.config, &self.from))
            .collect::<Result<Vec<_>, _>>()?;
        let mut comparison = Comparison::default();
        for (dataset, plans) in datasets.iter().zip(&mut prepared) {
            reconcile(dataset, plans, &client, &mut comparison, self.dry_run, Some(&mut state))?;
        }
        for resource in &removals {
            state.delete(&client, resource, self.dry_run).map_err(other)?;
        }
        if !self.dry_run {
            state
                .removed(&removals, &model, &ResourceKind::Dataset, &selected)
                .map_err(other)?;
        }
        Ok(())
    }
}
struct Source {
    name: String,
    batch: EventBatch,
    events: Vec<Value>,
    changed: bool,
    stale: Vec<Value>,
    repairs: Vec<Value>,
}
struct Case {
    name: String,
    row: Value,
    data: EventBatch,
    sources: Vec<Source>,
    span_path: Option<Vec<(String, String)>>,
    default_expected: bool,
}
fn seed(key: &str) -> u64 {
    u64::from_le_bytes(sdg::stable_uuid(key).as_bytes()[..8].try_into().unwrap())
}
fn prepare(dataset: &Dataset, model: &Model, config: &Braintrust, path: &Path) -> Result<Vec<Case>, Error> {
    prepare_for_project(dataset, model, &config.project_id.to_string(), path)
}
fn prepare_for_project(dataset: &Dataset, model: &Model, project_id: &str, path: &Path) -> Result<Vec<Case>, Error> {
    dataset
        .cases
        .iter()
        .map(|case| {
            let row_id =
                sdg::stable_uuid(&format!("bts/dataset/{}/{}/{}/row", project_id, dataset.name, case.name)).to_string();
            let mut data = sdg::case_data(model, case, seed(&row_id)).map_err(other)?;
            data.resolve_attachment_paths(path).map_err(other)?;
            let mut row = serde_json::to_value(&data)?["events"][0]["input"].clone();
            row["id"] = json!(row_id);
            if row.get("metadata").is_some_and(|value| !value.is_object()) {
                return Err(other(format!("case {:?}: metadata must evaluate to an object", case.name)));
            }
            if row
                .get("tags")
                .is_some_and(|value| value.as_array().is_none_or(|tags| tags.iter().any(|tag| !tag.is_string())))
            {
                return Err(other(format!(
                    "case {:?}: tags must evaluate to an array of strings",
                    case.name
                )));
            }
            let ids = match &case.source {
                DatasetSource::Inline(_) => Vec::new(),
                DatasetSource::Trace(id) | DatasetSource::Span(id) => vec![*id],
                DatasetSource::Group(ids) => ids.clone(),
            };
            let mut sources = Vec::new();
            for (index, id) in ids.iter().enumerate() {
                let reference = &model.refs[id.0 as usize];
                let root_node = reference
                    .module
                    .ok_or_else(|| other("dataset source must have a module trace anchor"))?;
                let trace = model
                    .traces
                    .iter()
                    .find(|trace| trace.node == root_node)
                    .ok_or_else(|| other("source trace not found"))?;
                let source_key = if matches!(case.source, DatasetSource::Group(_)) {
                    format!("{row_id}/group/{index}")
                } else {
                    row_id.clone()
                };
                let filter: WriteFilter = format!(
                    "block.kind != \"trace\" || block.name == {}",
                    serde_json::to_string(&trace.name)?
                )
                .parse()
                .map_err(other)?;
                let mut batch = sdg::generate_filtered(
                    model.clone(),
                    1,
                    Duration::from_secs(3600),
                    sdg::Distribution::Linear,
                    SystemTime::now(),
                    seed(&source_key),
                    &filter,
                )
                .map_err(other)?;
                let key = format!("bts/dataset/{project_id}/{source_key}");
                batch.assign_stable_ids(&key);
                batch.resolve_attachment_paths(path).map_err(other)?;
                let events = serde_json::to_value(&batch)?["events"]
                    .as_array()
                    .cloned()
                    .ok_or_else(|| other("generated payload has no events"))?;
                sources.push(Source {
                    name: trace.name.clone(),
                    batch,
                    events,
                    changed: false,
                    stale: Vec::new(),
                    repairs: Vec::new(),
                });
            }
            let span_path = if let DatasetSource::Span(id) = case.source {
                Some(span_path(model, id)?)
            } else {
                None
            };
            Ok(Case {
                name: case.name.clone(),
                row,
                data,
                sources,
                span_path,
                default_expected: case.expected.is_none(),
            })
        })
        .collect()
}
fn span_path(model: &Model, id: RefId) -> Result<Vec<(String, String)>, Error> {
    let reference = &model.refs[id.0 as usize];
    let Accessor::Block { node, .. } = reference.accessor else {
        return Err(other("span source is not a block"));
    };
    let root = model
        .traces
        .iter()
        .find(|trace| Some(trace.node) == reference.module)
        .ok_or_else(|| other("span source trace not found"))?;
    fn find(children: &[Child], node: NodeId, path: &mut Vec<(String, String)>) -> bool {
        for child in children {
            if let Child::Span(span) = child {
                let kind = match span.kind {
                    dsl::SpanKind::Task => "task",
                    dsl::SpanKind::Llm => "llm",
                    dsl::SpanKind::Tool => "tool",
                    dsl::SpanKind::Function => "function",
                };
                path.push((kind.to_owned(), span.name.clone()));
                if span.node == node || find(&span.children, node, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    let mut path = Vec::new();
    if !find(&root.children, node, &mut path) {
        return Err(other("span source requires a static span path"));
    }
    Ok(path)
}
fn reconcile(
    dataset: &Dataset,
    plans: &mut [Case],
    client: &Client,
    comparison: &mut Comparison,
    dry_run: bool,
    mut state: Option<&mut SyncState>,
) -> Result<(), Error> {
    for case in plans.iter() {
        writer::validate_attachments(client, &case.data.attachments).map_err(other)?;
        for source in &case.sources {
            writer::validate_attachments(client, &source.batch.attachments).map_err(other)?;
        }
    }
    let project_id = client.config.project_id.to_string();
    let objects = client::checked(
        client
            .get("/v1/dataset")
            .query(&[
                ("project_id", project_id.as_str()),
                ("dataset_name", dataset.name.as_str()),
                ("limit", "2"),
            ])
            .send()?,
        "list datasets",
    )
    .map_err(other)?;
    let objects = objects["objects"]
        .as_array()
        .ok_or_else(|| other("dataset list returned no objects"))?;
    if objects.len() > 1 {
        return Err(other(format!("multiple datasets named {:?}", dataset.name)));
    }
    let remote = objects.first();
    if remote.is_some_and(|remote| remote["project_id"] != project_id || remote["name"] != dataset.name) {
        return Err(other("dataset lookup returned a different dataset or project"));
    }
    let mut dataset_id = remote
        .map(|remote| {
            remote["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| other("dataset has no id"))
        })
        .transpose()?;
    if let Some(state) = state.as_deref_mut() {
        state
            .stage(TrackedResource {
                kind: ResourceKind::Dataset,
                name: dataset.name.clone(),
                slug: None,
                id: dataset_id.clone(),
                dependencies: Vec::new(),
                pending: None,
            })
            .map_err(other)?;
        if !dry_run {
            state.save().map_err(other)?;
        }
    }
    let rows = match &dataset_id {
        Some(id) => fetch_rows(client, id)?,
        None => HashMap::new(),
    };
    let roots: Vec<String> = plans
        .iter()
        .flat_map(|case| &case.sources)
        .map(|source| root(&source.events).map(str::to_owned))
        .collect::<Result<HashSet<_>, _>>()?
        .into_iter()
        .collect();
    let traces = fetch_traces(client, &roots)?;
    for case in plans.iter_mut() {
        for source in &mut case.sources {
            let remote = traces.get(root(&source.events)?).map(Vec::as_slice).unwrap_or(&[]);
            let by_id: HashMap<&str, &Value> = remote
                .iter()
                .filter_map(|event| event["id"].as_str().map(|id| (id, event)))
                .collect();
            let mut replacements = HashMap::new();
            for event in &mut source.events {
                let current = by_id.get(event["id"].as_str().unwrap()).copied().unwrap_or(&Value::Null);
                replacements.extend(
                    comparison
                        .reconcile(client, event, current, &source.batch.attachments)
                        .map_err(other)?,
                );
            }
            source.batch.reuse_attachments(&replacements);
            source.stale = stale(&source.events, remote).map_err(other)?;
            source.repairs = reparent(&source.events, remote, &source.stale).map_err(other)?;
            source.changed = !source.stale.is_empty() || !source.repairs.is_empty();
            for event in &mut source.events {
                let current = by_id.get(event["id"].as_str().unwrap()).copied().unwrap_or(&Value::Null);
                let (changed, patch) = update(event, current).map_err(other)?;
                source.changed |= changed;
                *event = patch;
            }
        }
        finish_row(case, &client.config)?;
        let current = rows.get(case.row["id"].as_str().unwrap()).unwrap_or(&Value::Null);
        let keys = comparison
            .reconcile(client, &mut case.row, current, &case.data.attachments)
            .map_err(other)?;
        case.data.reuse_attachments(&keys);
    }
    let desired_ids: HashSet<&str> = plans.iter().map(|case| case.row["id"].as_str().unwrap()).collect();
    let stale: Vec<Value> = rows
        .keys()
        .filter(|id| !desired_ids.contains(id.as_str()))
        .map(|id| json!({"id":id, "_object_delete":true}))
        .collect();
    let description_changed =
        remote.is_some_and(|remote| remote.get("description").unwrap_or(&Value::Null) != &json!(dataset.description));
    if dry_run {
        if remote.is_none() {
            println!("would create dataset {:?}", dataset.name);
        } else if description_changed {
            println!("would update dataset description {:?}", dataset.name);
        }
        for case in plans.iter() {
            for source in &case.sources {
                if source.changed {
                    println!("would write source trace {:?}", source.name);
                }
            }
            let current = rows.get(case.row["id"].as_str().unwrap());
            println!(
                "{} {:?} / {:?}",
                if current.is_some_and(|row| row_equal(row, &case.row)) {
                    "unchanged"
                } else if current.is_some() {
                    "would update case"
                } else {
                    "would create case"
                },
                dataset.name,
                case.name
            );
        }
        for event in &stale {
            println!("would delete case {:?} / {}", dataset.name, event["id"]);
        }
        return Ok(());
    }
    if dataset_id.is_none() {
        let created = client::checked(
            client
                .post("/v1/dataset")
                .json(&json!({"project_id":project_id,"name":dataset.name,"description":dataset.description}))
                .send()?,
            "create dataset",
        )
        .map_err(other)?;
        dataset_id = Some(
            created["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| other("created dataset has no id"))?
                .to_owned(),
        );
        println!("created dataset {:?}", dataset.name);
        if let Some(state) = state {
            state
                .complete(
                    &ResourceKind::Dataset,
                    &dataset.name,
                    dataset_id.as_deref().expect("created dataset id"),
                )
                .map_err(other)?;
        }
    } else if description_changed {
        client::checked(
            client
                .patch(format!("/v1/dataset/{}", dataset_id.as_deref().unwrap()))
                .json(&json!({"description":dataset.description}))
                .retryable()
                .send()?,
            "update description",
        )
        .map_err(other)?;
        println!("updated dataset description {:?}", dataset.name);
    }
    let logs_url = format!(
        "{}/v1/project_logs/{}/insert",
        client.config.api_url.trim_end_matches('/'),
        project_id
    );
    let mut writes = Vec::new();
    for case in plans.iter() {
        for source in &case.sources {
            if source.changed {
                writer::upload(client, &source.batch.attachments).map_err(other)?;
                writer::insert_events(client, &logs_url, &source.events).map_err(other)?;
                if !source.repairs.is_empty() {
                    writer::insert_events(client, &logs_url, &source.repairs).map_err(other)?;
                }
                if !source.stale.is_empty() {
                    writer::insert_events(client, &logs_url, &source.stale).map_err(other)?;
                }
                println!("wrote source trace {:?}", source.name);
            }
        }
        let current = rows.get(case.row["id"].as_str().unwrap());
        if current.is_some_and(|row| row_equal(row, &case.row)) {
            println!("unchanged {:?} / {:?}", dataset.name, case.name);
        } else {
            writer::upload(client, &case.data.attachments).map_err(other)?;
            let mut row = case.row.clone();
            row["_is_merge"] = json!(false);
            writes.push(row);
        }
    }
    let dataset_url = format!(
        "{}/v1/dataset/{}/insert",
        client.config.api_url.trim_end_matches('/'),
        dataset_id.unwrap()
    );
    writer::insert_events(client, &dataset_url, &writes).map_err(other)?;
    for row in &writes {
        println!("synced case {:?} / {}", dataset.name, row["id"]);
    }
    // deletion is last: no failed comparison or partial upsert can prune cases
    writer::insert_events(client, &dataset_url, &stale).map_err(other)?;
    for event in &stale {
        println!("deleted case {:?} / {}", dataset.name, event["id"]);
    }
    Ok(())
}
fn finish_row(case: &mut Case, config: &Braintrust) -> Result<(), Error> {
    finish_row_for_project(case, &config.project_id.to_string())
}
fn finish_row_for_project(case: &mut Case, project_id: &str) -> Result<(), Error> {
    if let Some(path) = &case.span_path {
        let selected = select_span(&case.sources[0].events, path);
        if selected.len() != 1 {
            return Err(other(format!(
                "case {:?} selects {} generated spans",
                case.name,
                selected.len()
            )));
        }
        case.row["input"] = selected[0].get("input").cloned().unwrap_or(Value::Null);
        if case.default_expected
            && let Some(output) = selected[0].get("output")
        {
            case.row["expected"] = output.clone();
        }
        case.row["origin"] = json!({"object_type":"project_logs","object_id":project_id,"id":selected[0]["id"]});
    } else if !case.sources.is_empty() {
        let refs = case.sources.iter().map(|source| Ok(json!({"trace_ref":{"object_type":"project_logs","object_id":project_id,"root_span_id":root(&source.events)?}}))).collect::<Result<Vec<_>, Error>>()?;
        case.row["input"] = if refs.len() == 1 {
            refs.into_iter().next().unwrap()
        } else {
            json!(refs)
        };
    }
    Ok(())
}
fn select_span<'a>(events: &'a [Value], path: &[(String, String)]) -> Vec<&'a Value> {
    let Some(root) = events.first() else { return Vec::new() };
    let mut parents = vec![root];
    for (kind, name) in path {
        parents = events
            .iter()
            .filter(|event| {
                event["span_attributes"]["type"] == *kind
                    && event["span_attributes"]["name"] == *name
                    && parents.iter().any(|parent| {
                        event["span_parents"]
                            .as_array()
                            .is_some_and(|ids| ids.contains(&parent["span_id"]))
                    })
            })
            .collect();
    }
    parents
}
fn root(events: &[Value]) -> Result<&str, Error> {
    events
        .first()
        .and_then(|event| event["root_span_id"].as_str())
        .ok_or_else(|| other("generated trace has no root_span_id"))
}
fn fetch_traces(client: &Client, roots: &[String]) -> Result<HashMap<String, Vec<Value>>, Error> {
    let mut traces: HashMap<String, Vec<Value>> = HashMap::new();
    for chunk in roots.chunks(64) {
        let ids = chunk
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT id, span_id, root_span_id, span_parents, span_attributes, input, output, expected, error, metadata, tags, metrics, scores FROM project_logs('{}') WHERE root_span_id IN ({ids}) LIMIT 1000",
            client.config.project_id
        );
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let sql = cursor.as_ref().map_or_else(
                || query.clone(),
                |cursor: &String| format!("{query} OFFSET '{}'", cursor.replace('\'', "''")),
            );
            let response = client
                .post("/btql")
                .json(&json!({"query":sql,"brainstore_realtime":true}))
                .retryable()
                .send()?;
            let header = response
                .headers()
                .get("x-bt-cursor")
                .or_else(|| response.headers().get("x-amz-meta-bt_cursor"))
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let page = client::checked(response, "fetch source traces").map_err(other)?;
            for event in page["data"].as_array().ok_or_else(|| other("trace query returned no data"))? {
                let root = event["root_span_id"]
                    .as_str()
                    .ok_or_else(|| other("remote span has no root_span_id"))?;
                traces.entry(root.to_owned()).or_default().push(event.clone());
            }
            cursor = header.or_else(|| page["cursor"].as_str().filter(|value| !value.is_empty()).map(str::to_owned));
            let Some(token) = &cursor else { break };
            if !seen.insert(token.clone()) {
                return Err(other("trace query repeated its pagination cursor"));
            }
        }
    }
    Ok(traces)
}
fn row_equal(current: &Value, desired: &Value) -> bool {
    ["input", "expected", "metadata", "tags", "origin"]
        .iter()
        .all(|key| current.get(*key).unwrap_or(&Value::Null) == desired.get(*key).unwrap_or(&Value::Null))
}
pub(crate) fn fetch_rows(client: &Client, id: &str) -> Result<HashMap<String, Value>, Error> {
    fetch_snapshot(client, id).map(|(_, rows)| rows)
}
pub(crate) fn fetch_snapshot(client: &Client, id: &str) -> Result<(Option<String>, HashMap<String, Value>), Error> {
    let mut rows = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut version: Option<String> = None;
    let mut seen = HashSet::new();
    loop {
        let mut query = vec![("limit", "1000")];
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        if let Some(version) = &version {
            query.push(("version", version));
        }
        let page = client::checked(
            client.get(format!("/v1/dataset/{id}/fetch")).query(&query).send()?,
            "fetch dataset rows",
        )
        .map_err(other)?;
        let events = page["events"]
            .as_array()
            .ok_or_else(|| other("dataset fetch returned no events"))?;
        if version.is_none() {
            version = events
                .iter()
                .filter_map(|event| event["_xact_id"].as_str())
                .max_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)))
                .map(str::to_owned);
        }
        for row in events {
            let id = row["id"].as_str().ok_or_else(|| other("dataset row has no id"))?;
            // fetch walks newest to oldest, including earlier versions of rows
            rows.entry(id.to_owned()).or_insert_with(|| row.clone());
        }
        cursor = page["cursor"].as_str().filter(|value| !value.is_empty()).map(str::to_owned);
        let Some(token) = &cursor else { break };
        if !seen.insert(token.clone()) {
            return Err(other("dataset fetch repeated its pagination cursor"));
        }
    }
    Ok((version, rows))
}

pub(crate) fn preview_rows(dataset: &Dataset, model: &Model, path: &Path) -> Result<Vec<Value>, Error> {
    let mut cases = prepare_for_project(dataset, model, "preview", path)?;
    for case in &mut cases {
        finish_row_for_project(case, "preview")?;
    }
    Ok(cases.into_iter().map(|case| case.row).collect())
}
fn other(error: impl fmt::Display) -> Error {
    Error::Other(error.to_string())
}
#[derive(Debug)]
pub enum Error {
    Http(client::Error),
    Json(serde_json::Error),
    Other(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Other(error) => f.write_str(error),
        }
    }
}
impl std::error::Error for Error {}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.into())
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<client::Error> for Error {
    fn from(error: client::Error) -> Self {
        Self::Http(error.without_url())
    }
}

const OWNER: &str = "_bts_dataset_source";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u8,
    root_span_id: String,
    fields: BTreeSet<Vec<String>>,
    tags: BTreeSet<String>,
}

fn ownership(event: &Value) -> Result<Option<Ownership>, String> {
    let Some(value) = event["metadata"].get(OWNER) else {
        return Ok(None);
    };
    let owner: Ownership = serde_json::from_value(value.clone()).map_err(|error| format!("source ownership: {error}"))?;
    if owner.version != 1
        || event["root_span_id"] != owner.root_span_id
        || owner.fields.iter().any(|path| {
            path.is_empty()
                || !matches!(
                    path[0].as_str(),
                    "span_id"
                        | "root_span_id"
                        | "span_parents"
                        | "input"
                        | "output"
                        | "expected"
                        | "error"
                        | "metadata"
                        | "metrics"
                        | "scores"
                        | "span_attributes"
                )
                || (matches!(path[0].as_str(), "metadata" | "metrics" | "scores" | "span_attributes") && path.len() < 2)
                || (path[0] == "metadata" && path[1] == OWNER)
        })
    {
        return Err("source ownership does not match its span".to_owned());
    }
    Ok(Some(owner))
}

fn fields(value: &Value, path: Vec<String>, paths: &mut BTreeSet<Vec<String>>) {
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            let mut path = path.clone();
            path.push(key.clone());
            fields(value, path, paths);
        }
    } else {
        paths.insert(path);
    }
}

fn read<'a>(event: &'a Value, path: &[String]) -> &'a Value {
    path.iter().fold(event, |value, key| &value[key])
}

fn set(event: &mut Value, path: &[String], value: Value) {
    let mut at = event;
    for key in &path[..path.len() - 1] {
        if !at.is_object() {
            *at = json!({});
        }
        at = &mut at[key];
    }
    if !at.is_object() {
        *at = json!({});
    }
    at[&path[path.len() - 1]] = value;
}

// own the module's fields; leave server additions out of the update entirely
fn update(local: &Value, remote: &Value) -> Result<(bool, Value), String> {
    if local["metadata"].get(OWNER).is_some() {
        return Err(format!("metadata.{OWNER} is reserved for dataset source ownership"));
    }
    let mut paths = BTreeSet::new();
    for key in [
        "span_id",
        "root_span_id",
        "span_parents",
        "input",
        "output",
        "expected",
        "error",
    ] {
        if local.get(key).is_some() {
            paths.insert(vec![key.to_owned()]);
        }
    }
    for key in ["metadata", "metrics", "scores", "span_attributes"] {
        if let Some(object) = local[key].as_object() {
            for (name, value) in object {
                if key == "metrics" && matches!(name.as_str(), "start" | "end" | "duration") {
                    continue;
                }
                fields(value, vec![key.to_owned(), name.clone()], &mut paths);
            }
        }
    }
    let tags: BTreeSet<String> = local["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_owned))
        .collect();
    let desired = Ownership {
        version: 1,
        root_span_id: local["root_span_id"].as_str().ok_or("source has no root_span_id")?.to_owned(),
        fields: paths,
        tags,
    };
    let previous = ownership(remote)?;
    let mut changed = previous.as_ref() != Some(&desired);
    let mut patch = local.clone();
    let mut merge_paths = desired.fields.clone();
    for path in &desired.fields {
        let left = read(local, path);
        let right = read(remote, path);
        let empty_parents = path == &["span_parents"] && left.as_array().is_some_and(Vec::is_empty) && right.is_null();
        changed |= !empty_parents && left != right;
    }
    let mut deleted_tags = BTreeSet::new();
    if let Some(previous) = previous {
        for path in previous.fields.difference(&desired.fields) {
            // a new ancestor replaces this field; a new descendant is written
            // below after clearing the old value
            changed |= !read(remote, path).is_null();
            set(&mut patch, path, Value::Null);
            merge_paths.insert(path.clone());
        }
        deleted_tags = previous.tags.difference(&desired.tags).cloned().collect();
    }
    for path in &desired.fields {
        set(&mut patch, path, read(local, path).clone());
    }
    let remote_tags: BTreeSet<String> = remote["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_owned))
        .collect();
    changed |= !desired.tags.is_subset(&remote_tags) || !deleted_tags.is_disjoint(&remote_tags);
    if !deleted_tags.is_empty() {
        patch["_array_delete"] = json!([{"path":["tags"],"delete":deleted_tags}]);
    }
    if !remote.is_null() {
        patch.as_object_mut().unwrap().remove("created");
        if let Some(metrics) = patch["metrics"].as_object_mut() {
            for key in ["start", "end", "duration"] {
                metrics.remove(key);
            }
        }
    }
    set(&mut patch, &["metadata".to_owned(), OWNER.to_owned()], json!(desired));
    merge_paths.insert(vec!["metadata".to_owned(), OWNER.to_owned()]);
    patch["_is_merge"] = json!(true);
    patch["_merge_paths"] = json!(merge_paths);
    Ok((changed, patch))
}

fn stale(local: &[Value], remote: &[Value]) -> Result<Vec<Value>, String> {
    let ids: BTreeSet<&str> = local.iter().filter_map(|event| event["id"].as_str()).collect();
    let root = local
        .first()
        .and_then(|event| event["root_span_id"].as_str())
        .ok_or("source has no root")?;
    let mut stale = Vec::new();
    for event in remote {
        let id = event["id"].as_str().ok_or("remote span has no id")?;
        if !ids.contains(id) && ownership(event)?.is_some_and(|owner| owner.root_span_id == root) {
            stale.push(json!({"id":id,"_object_delete":true}));
        }
    }
    Ok(stale)
}

// an enrichment span can outlive an authored parent; reconnect it before
// deleting that parent so the trace still has a complete tree
fn reparent(local: &[Value], remote: &[Value], deleted: &[Value]) -> Result<Vec<Value>, String> {
    let root = local
        .first()
        .and_then(|event| event["root_span_id"].as_str())
        .ok_or("source has no root")?;
    let deleted_ids: BTreeSet<&str> = deleted.iter().filter_map(|event| event["id"].as_str()).collect();
    let removed: std::collections::HashMap<&str, &Value> = remote
        .iter()
        .filter(|event| event["id"].as_str().is_some_and(|id| deleted_ids.contains(id)))
        .map(|event| Ok((event["span_id"].as_str().ok_or("removed source span has no span_id")?, event)))
        .collect::<Result<_, String>>()?;
    fn ancestors<'a>(
        parent: &'a str,
        removed: &std::collections::HashMap<&str, &'a Value>,
        visiting: &mut BTreeSet<&'a str>,
        root: &'a str,
        out: &mut BTreeSet<&'a str>,
    ) -> Result<(), String> {
        let Some(event) = removed.get(parent) else {
            out.insert(parent);
            return Ok(());
        };
        if !visiting.insert(parent) {
            return Err("removed source spans have cyclic parent links".to_owned());
        }
        if let Some(parents) = event["span_parents"].as_array().filter(|parents| !parents.is_empty()) {
            for parent in parents {
                ancestors(
                    parent.as_str().ok_or("source parent is not a span id")?,
                    removed,
                    visiting,
                    root,
                    out,
                )?;
            }
        } else {
            out.insert(root);
        }
        visiting.remove(parent);
        Ok(())
    }
    let mut updates = Vec::new();
    for event in remote {
        if ownership(event)?.is_some() {
            continue;
        }
        let Some(parents) = event["span_parents"].as_array() else {
            continue;
        };
        if !parents
            .iter()
            .any(|parent| parent.as_str().is_some_and(|id| removed.contains_key(id)))
        {
            continue;
        }
        let mut surviving = BTreeSet::new();
        for parent in parents {
            ancestors(
                parent.as_str().ok_or("enrichment parent is not a span id")?,
                &removed,
                &mut BTreeSet::new(),
                root,
                &mut surviving,
            )?;
        }
        updates.push(json!({"id":event["id"],"span_parents":surviving,"_is_merge":true,"_merge_paths":[["span_parents"]]}));
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::shared::client::tests::{Reply, serve};
    #[test]
    fn batches_trace_reads_and_follows_header_cursors_past_one_thousand_spans() {
        let first = (0..1000)
            .map(|index| json!({"id":index.to_string(),"root_span_id":"one"}))
            .collect::<Vec<_>>();
        let (client, requests) = serve(vec![
            Reply {
                status: 200,
                body: json!({"data":first}).to_string(),
                headers: "x-bt-cursor: next-page\r\n".to_owned(),
            },
            Reply::json(json!({"data":[{"id":"last","root_span_id":"one"},{"id":"other","root_span_id":"two"}]})),
        ]);
        let traces = fetch_traces(&client, &["one".to_owned(), "two".to_owned()]).unwrap();
        assert_eq!(traces["one"].len(), 1001);
        assert_eq!(traces["two"].len(), 1);
        assert!(requests.recv().unwrap().contains("root_span_id IN ('one','two')"));
        assert!(requests.recv().unwrap().contains("OFFSET 'next-page'"));
    }

    #[test]
    fn dataset_pagination_keeps_the_newest_row_and_pins_its_snapshot() {
        let (client, requests) = serve(vec![
            Reply::json(json!({"events":[{"id":"one","input":"new","_xact_id":"100"}],"cursor":"next"})),
            Reply::json(
                json!({"events":[{"id":"one","input":"old","_xact_id":"90"},{"id":"two","input":"other","_xact_id":"80"}]}),
            ),
        ]);
        let rows = fetch_rows(&client, "dataset").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows["one"]["input"], "new");
        assert!(!requests.recv().unwrap().contains("version="));
        let second = requests.recv().unwrap();
        assert!(second.contains("cursor=next"));
        assert!(second.contains("version=100"));
    }

    #[test]
    fn grouped_sources_keep_order_and_have_distinct_native_trace_references() {
        let config = Braintrust::new("test".to_owned(), uuid::Uuid::nil());
        let model = dsl::compile(
            r#"trace "one" { input = "question" } dataset "cases" { case "group" { traces = [trace["one"], trace["one"]] } }"#,
        )
        .unwrap();
        let mut plans = prepare(&model.datasets[0], &model, &config, Path::new("/tmp/cases.bt")).unwrap();
        let case = &mut plans[0];
        finish_row(case, &config).unwrap();
        let refs = case.row["input"].as_array().unwrap();
        assert_eq!(refs.len(), 2);
        for (reference, source) in refs.iter().zip(&case.sources) {
            assert_eq!(reference["trace_ref"]["object_type"], "project_logs");
            assert_eq!(reference["trace_ref"]["object_id"], config.project_id.to_string());
            assert_eq!(reference["trace_ref"]["root_span_id"], root(&source.events).unwrap());
        }
        assert_ne!(refs[0]["trace_ref"]["root_span_id"], refs[1]["trace_ref"]["root_span_id"]);
    }
    fn inline_plan(client: &Client) -> (Dataset, Vec<Case>) {
        let model = dsl::compile(r#"dataset "cases" { case "one" { input = "new" } }"#).unwrap();
        let plans = prepare(&model.datasets[0], &model, &client.config, Path::new("/tmp/cases.bt")).unwrap();
        (model.datasets[0].clone(), plans)
    }
    #[test]
    fn replaces_surviving_rows_then_deletes_absent_cases() {
        let (mut client, requests) = serve(vec![
            Reply::json(json!({"objects":[{"id":"dataset","project_id":"$PROJECT","name":"cases"}]})),
            Reply::json(json!({"events":[{"id":"old-case","input":"old"}]})),
            Reply::json(json!({"row_ids":["new"]})),
            Reply::json(json!({"row_ids":["old-case"]})),
        ]);
        client.config.project_id = uuid::Uuid::nil();
        let (dataset, mut plans) = inline_plan(&client);
        reconcile(&dataset, &mut plans, &client, &mut Comparison::default(), false, None).unwrap();
        assert!(requests.recv().unwrap().starts_with("GET /v1/dataset?"));
        assert!(requests.recv().unwrap().starts_with("GET /v1/dataset/dataset/fetch?"));
        let upsert = requests.recv().unwrap();
        assert!(upsert.contains("\"_is_merge\":false"));
        assert!(upsert.contains("\"input\":\"new\""));
        let deletion = requests.recv().unwrap();
        assert!(deletion.contains("\"_object_delete\":true"));
        assert!(deletion.contains("old-case"));
    }
    #[test]
    fn failed_upserts_do_not_delete_cases() {
        let (mut client, requests) = serve(vec![
            Reply::json(json!({"objects":[{"id":"dataset","project_id":"$PROJECT","name":"cases"}]})),
            Reply::json(json!({"events":[{"id":"old-case","input":"old"}]})),
            Reply {
                status: 500,
                body: "{}".to_owned(),
                headers: String::new(),
            },
        ]);
        client.config.retry_attempts = 1;
        let (dataset, mut plans) = inline_plan(&client);
        assert!(reconcile(&dataset, &mut plans, &client, &mut Comparison::default(), false, None).is_err());
        for _ in 0..3 {
            assert!(!requests.recv().unwrap().contains("_object_delete"));
        }
        assert!(requests.recv_timeout(Duration::from_millis(20)).is_err());
    }

    #[test]
    fn failed_enrichment_repairs_do_not_delete_their_authored_parents() {
        let model = dsl::compile(r#"trace "one" {} dataset "cases" { case "one" { trace = trace["one"] } }"#).unwrap();
        let config = Braintrust::new("test".to_owned(), uuid::Uuid::nil());
        let mut plans = prepare(&model.datasets[0], &model, &config, Path::new("/tmp/cases.bt")).unwrap();
        let local = &plans[0].sources[0].events[0];
        let (_, current) = update(local, &Value::Null).unwrap();
        let (_, old) = update(
            &json!({"id":"old-source","span_id":"old-span","root_span_id":local["root_span_id"],"span_parents":[local["span_id"]]}),
            &Value::Null,
        )
        .unwrap();
        let enrichment = json!({"id":"enrichment","root_span_id":local["root_span_id"],"span_parents":["old-span"]});
        let (mut client, requests) = serve(vec![
            Reply::json(json!({"objects":[{"id":"dataset","project_id":"$PROJECT","name":"cases"}]})),
            Reply::json(json!({"events":[]})),
            Reply::json(json!({"data":[current,old,enrichment]})),
            Reply::json(json!({"row_ids":[local["id"]]})),
            Reply {
                status: 500,
                body: "{}".to_owned(),
                headers: String::new(),
            },
        ]);
        client.config.retry_attempts = 1;
        assert!(
            reconcile(
                &model.datasets[0],
                &mut plans,
                &client,
                &mut Comparison::default(),
                false,
                None
            )
            .is_err()
        );
        for _ in 0..5 {
            assert!(!requests.recv().unwrap().contains("_object_delete"));
        }
        assert!(requests.recv_timeout(Duration::from_millis(20)).is_err());
    }
    #[test]
    fn span_path_uses_parentage_when_names_repeat() {
        let events = vec![
            json!({"id":"root","span_id":"root"}),
            json!({"id":"first","span_id":"first","span_parents":["root"],"span_attributes":{"type":"task","name":"first"}}),
            json!({"id":"second","span_id":"second","span_parents":["root"],"span_attributes":{"type":"task","name":"second"}}),
            json!({"id":"wrong","span_id":"wrong","span_parents":["second"],"span_attributes":{"type":"tool","name":"lookup"}}),
            json!({"id":"right","span_id":"right","span_parents":["first"],"span_attributes":{"type":"tool","name":"lookup"}}),
        ];
        let path = [
            ("task".to_owned(), "first".to_owned()),
            ("tool".to_owned(), "lookup".to_owned()),
        ];
        assert_eq!(select_span(&events, &path)[0]["id"], "right");
    }

    fn saved(local: &Value) -> Value {
        let (_, mut event) = update(local, &Value::Null).unwrap();
        event.as_object_mut().unwrap().remove("_is_merge");
        event.as_object_mut().unwrap().remove("_merge_paths");
        event
    }

    #[test]
    fn clears_removed_scores_and_preserves_online_scores() {
        let before = json!({"id":"row","root_span_id":"root","scores":{"local":0.7}});
        let mut remote = saved(&before);
        remote["scores"]["online"] = json!(0.9);
        let local = json!({"id":"row","root_span_id":"root"});
        let (changed, patch) = update(&local, &remote).unwrap();
        assert!(changed);
        assert_eq!(patch["scores"]["local"], Value::Null);
        assert!(patch["scores"].get("online").is_none());
        assert_eq!(patch["_is_merge"], true);
        assert!(!patch["_merge_paths"].as_array().unwrap().contains(&json!(["scores"])));
        remote["scores"]["local"] = Value::Null;
        remote["metadata"][OWNER] = patch["metadata"][OWNER].clone();
        assert!(!update(&local, &remote).unwrap().0);
    }

    #[test]
    fn ignores_enrichment_and_only_clears_previously_owned_fields() {
        let local = json!({"id":"row","root_span_id":"root","metadata":{"nested":{"authored":1}},"metrics":{"tokens":4},"tags":["local"],"span_attributes":{"name":"run","type":"task"}});
        let mut remote = saved(&local);
        remote["metadata"]["nested"]["topics"] = json!(["billing"]);
        remote["metadata"]["patterns"] = json!(["pattern"]);
        remote["metrics"]["enrichment_cost"] = json!(0.01);
        remote["tags"] = json!(["local", "server"]);
        remote["span_attributes"]["enrichment"] = json!(true);
        assert!(!update(&local, &remote).unwrap().0);
        let after = json!({"id":"row","root_span_id":"root","output":"new","span_attributes":{"name":"run","type":"task"}});
        let (_, patch) = update(&after, &remote).unwrap();
        assert_eq!(patch["metadata"]["nested"]["authored"], Value::Null);
        assert!(patch["metadata"]["nested"].get("topics").is_none());
        assert!(patch["metadata"].get("patterns").is_none());
        assert!(patch["metrics"].get("enrichment_cost").is_none());
        assert!(patch["span_attributes"].get("enrichment").is_none());
        assert_eq!(patch["_array_delete"], json!([{"path":["tags"],"delete":["local"]}]));
    }

    #[test]
    fn deletes_only_marked_source_rows_regardless_of_uuid_version() {
        let current = saved(&json!({"id":"current","root_span_id":"root"}));
        let removed = saved(&json!({"id":"old","root_span_id":"root"}));
        let enrichment = json!({"id":crate::sdg::stable_uuid("enrichment").to_string(),"root_span_id":"root"});
        let remote = vec![current.clone(), removed, enrichment];
        assert_eq!(
            stale(std::slice::from_ref(&current), &remote).unwrap(),
            vec![json!({"id":"old","_object_delete":true})]
        );
    }

    #[test]
    fn legacy_sources_are_adopted_without_claiming_unknown_scores() {
        let local = json!({"id":"row","root_span_id":"root","scores":{"local":0.5}});
        let mut remote = local.clone();
        remote["scores"]["online"] = json!(0.9);
        let (changed, patch) = update(&local, &remote).unwrap();
        assert!(changed);
        assert_eq!(
            patch["metadata"][OWNER]["fields"],
            json!([["root_span_id"], ["scores", "local"]])
        );
        assert!(patch["scores"].get("online").is_none());
    }

    #[test]
    fn rejects_reserved_or_invalid_ownership_before_writing() {
        let local = json!({"id":"row","root_span_id":"root"});
        let mut remote = saved(&local);
        remote["metadata"][OWNER]["fields"] = json!([["metadata"]]);
        assert!(update(&local, &remote).is_err());
        assert!(update(&remote, &Value::Null).is_err());
    }

    #[test]
    fn accepts_null_root_parents_returned_by_braintrust() {
        let local = json!({"id":"root","root_span_id":"root","span_parents":[]});
        let mut remote = saved(&local);
        remote["span_parents"] = Value::Null;
        assert!(!update(&local, &remote).unwrap().0);
    }

    #[test]
    fn empty_authored_objects_leave_room_for_enrichment() {
        let local = json!({"id":"row","root_span_id":"root","metadata":{"nested":{}}});
        let mut remote = saved(&local);
        remote["metadata"]["nested"]["topics"] = json!(["billing"]);
        assert!(!update(&local, &remote).unwrap().0);
        let (_, patch) = update(&local, &remote).unwrap();
        assert!(
            !patch["_merge_paths"]
                .as_array()
                .unwrap()
                .contains(&json!(["metadata", "nested"]))
        );
    }

    #[test]
    fn reconnects_enrichment_to_the_nearest_surviving_ancestor() {
        let root = saved(&json!({"id":"root-row","span_id":"root","root_span_id":"root"}));
        let parent = saved(&json!({"id":"parent-row","span_id":"parent","root_span_id":"root","span_parents":["root"]}));
        let child = saved(&json!({"id":"child-row","span_id":"child","root_span_id":"root","span_parents":["parent"]}));
        let enrichment = json!({"id":"enrichment","span_id":"enrichment","root_span_id":"root","span_parents":["child","other"],"metadata":{"topics":["billing"]}});
        let remote = vec![root.clone(), parent, child, enrichment];
        let local = std::slice::from_ref(&root);
        let deleted = stale(local, &remote).unwrap();
        let updates = reparent(local, &remote, &deleted).unwrap();
        assert_eq!(
            updates,
            vec![json!({"id":"enrichment","span_parents":["other","root"],"_is_merge":true,"_merge_paths":[["span_parents"]]})]
        );
        assert!(updates[0].get("metadata").is_none());
    }
}
