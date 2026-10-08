use crate::cmd::{
    client::{self, Client, attachments::Comparison, writer},
    render_diags,
};
use crate::conf::{Braintrust, Settings};
use crate::dsl::{self, Accessor, Child, Dataset, DatasetSource, Model, NodeId, RefId, WriteFilter};
use crate::sdg::{self, EventBatch};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
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
}
impl Args {
    pub fn run(self) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(other)?;
        let model =
            dsl::compile(&source).map_err(|diags| other(render_diags(&self.from.display().to_string(), &source, &diags)))?;
        if model.datasets.is_empty() {
            return Err(other("module declares no dataset blocks"));
        }
        let client = Client::configured(Braintrust::load().map_err(other)?, &Settings::load().map_err(other)?)?;
        // prepare every local case before accessing remote state
        let mut prepared = model
            .datasets
            .iter()
            .map(|dataset| prepare(dataset, &model, &client.config, &self.from))
            .collect::<Result<Vec<_>, _>>()?;
        let mut comparison = Comparison::default();
        for (dataset, plans) in model.datasets.iter().zip(&mut prepared) {
            reconcile(dataset, plans, &client, &mut comparison, self.dry_run)?;
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
    dataset
        .cases
        .iter()
        .map(|case| {
            let row_id = sdg::stable_uuid(&format!(
                "bts/dataset/{}/{}/{}/row",
                config.project_id, dataset.name, case.name
            ))
            .to_string();
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
                let key = format!("bts/dataset/{}/{source_key}", config.project_id);
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
                .map(str::to_owned)
                .ok_or_else(|| other("dataset has no id"))
        })
        .transpose()?;
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
            source.stale = stale_source_rows(&source.events, remote);
            source.changed = !source.stale.is_empty() || !trace_equal(&source.events, remote);
            if source.changed {
                for event in &mut source.events {
                    // replacement writes clear fields removed locally; keep
                    // asynchronous score keys not authored by this module
                    let current = by_id.get(event["id"].as_str().unwrap()).copied();
                    if let Some(scores) = current.and_then(|event| event["scores"].as_object()) {
                        let mut merged = scores.clone();
                        if let Some(local) = event["scores"].as_object() {
                            merged.extend(local.clone());
                        }
                        event["scores"] = Value::Object(merged);
                    }
                    event["_is_merge"] = json!(false);
                }
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
                .ok_or_else(|| other("created dataset has no id"))?
                .to_owned(),
        );
        println!("created dataset {:?}", dataset.name);
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
        case.row["origin"] =
            json!({"object_type":"project_logs","object_id":config.project_id.to_string(),"id":selected[0]["id"]});
    } else if !case.sources.is_empty() {
        let refs = case.sources.iter().map(|source| Ok(json!({"trace_ref":{"object_type":"project_logs","object_id":config.project_id.to_string(),"root_span_id":root(&source.events)?}}))).collect::<Result<Vec<_>, Error>>()?;
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
fn stale_source_rows(local: &[Value], remote: &[Value]) -> Vec<Value> {
    let desired_ids: HashSet<&str> = local.iter().filter_map(|event| event["id"].as_str()).collect();
    // dataset sources use our deterministic UUID v8 rows within their owned
    // root. online spans have separate ids and stay outside reconciliation.
    remote
        .iter()
        .filter_map(|event| event["id"].as_str())
        .filter(|id| !desired_ids.contains(id) && uuid::Uuid::parse_str(id).is_ok_and(|id| id.get_version_num() == 8))
        .map(|id| json!({"id":id,"_object_delete":true}))
        .collect()
}

fn trace_equal(local: &[Value], remote: &[Value]) -> bool {
    let by_id: HashMap<&str, &Value> = remote
        .iter()
        .filter_map(|event| event["id"].as_str().map(|id| (id, event)))
        .collect();
    local.iter().all(|event| {
        event["id"].as_str().and_then(|id| by_id.get(id)).is_some_and(|found| {
            canonical(event) == canonical(found)
                && event["scores"]
                    .as_object()
                    .is_none_or(|scores| scores.iter().all(|(key, value)| found["scores"][key] == *value))
        })
    })
}
fn canonical(event: &Value) -> Value {
    let mut fields = Map::new();
    for key in [
        "span_id",
        "root_span_id",
        "span_parents",
        "input",
        "output",
        "expected",
        "error",
        "metadata",
        "tags",
    ] {
        if let Some(value) = event.get(key).filter(|value| !value.is_null()) {
            fields.insert(key.to_owned(), value.clone());
        }
    }
    if !fields.contains_key("span_parents") {
        fields.insert("span_parents".to_owned(), json!([]));
    }
    if let Some(attrs) = event.get("span_attributes") {
        fields.insert(
            "span_attributes".to_owned(),
            json!({"name":attrs["name"],"type":attrs["type"]}),
        );
    }
    if let Some(metrics) = event["metrics"].as_object() {
        let mut metrics = metrics.clone();
        for key in ["start", "end", "duration"] {
            metrics.remove(key);
        }
        if !metrics.is_empty() {
            fields.insert("metrics".to_owned(), Value::Object(metrics));
        }
    }
    Value::Object(fields)
}
fn row_equal(current: &Value, desired: &Value) -> bool {
    ["input", "expected", "metadata", "tags", "origin"]
        .iter()
        .all(|key| current.get(*key).unwrap_or(&Value::Null) == desired.get(*key).unwrap_or(&Value::Null))
}
fn fetch_rows(client: &Client, id: &str) -> Result<HashMap<String, Value>, Error> {
    let mut rows = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    loop {
        let mut query = vec![("limit", "1000")];
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        let page = client::checked(
            client.get(format!("/v1/dataset/{id}/fetch")).query(&query).send()?,
            "fetch dataset rows",
        )
        .map_err(other)?;
        for row in page["events"]
            .as_array()
            .ok_or_else(|| other("dataset fetch returned no events"))?
        {
            let id = row["id"].as_str().ok_or_else(|| other("dataset row has no id"))?;
            rows.insert(id.to_owned(), row.clone());
        }
        cursor = page["cursor"].as_str().filter(|value| !value.is_empty()).map(str::to_owned);
        let Some(token) = &cursor else { break };
        if !seen.insert(token.clone()) {
            return Err(other("dataset fetch repeated its pagination cursor"));
        }
    }
    Ok(rows)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::client::tests::{Reply, serve};
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
        reconcile(&dataset, &mut plans, &client, &mut Comparison::default(), false).unwrap();
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
        assert!(reconcile(&dataset, &mut plans, &client, &mut Comparison::default(), false).is_err());
        for _ in 0..3 {
            assert!(!requests.recv().unwrap().contains("_object_delete"));
        }
        assert!(requests.recv_timeout(Duration::from_millis(20)).is_err());
    }
    #[test]
    fn removes_sparse_owned_rows_without_deleting_online_spans() {
        let current = sdg::stable_uuid("source/row/0").to_string();
        let removed = sdg::stable_uuid("source/row/100000").to_string();
        let online = uuid::Uuid::new_v4().to_string();
        let local = vec![json!({"id":current})];
        let remote = vec![json!({"id":current}), json!({"id":removed}), json!({"id":online})];
        assert_eq!(
            stale_source_rows(&local, &remote),
            vec![json!({"id":removed,"_object_delete":true})]
        );
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
}
