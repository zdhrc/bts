use crate::cmd::render_diags;
use crate::conf::{Braintrust, Settings};
use crate::dsl::{self, Automation};
use crate::scg;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{collections::HashMap, fmt, fs, path::PathBuf};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// bts shape containing automations and their scorer blocks
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// show creates and updates without changing Braintrust
    #[arg(long)]
    dry_run: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(|source| Error::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        let model = dsl::compile(&source).map_err(|diags| Error::InvalidShape {
            details: render_diags(&self.from.display().to_string(), &source, &diags),
        })?;
        if model.automations.is_empty() {
            return Err(Error::NoAutomations);
        }
        let slugs = scg::resolve_slugs(&model.scorers).map_err(Error::ScorerPlan)?;
        let slugs_by_name: HashMap<_, _> = model
            .scorers
            .iter()
            .map(|scorer| scorer.name.as_str())
            .zip(slugs.iter())
            .collect();

        let config = Braintrust::load()?;
        let settings = Settings::load()?;
        let client = Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(Error::Http)?;
        let base = config.api_url.trim_end_matches('/');
        let project_id = config.project_id.to_string();

        // Resolve every dependency before writing any rule, so a missing push
        // cannot leave the file half-synced.
        let mut function_ids = HashMap::new();
        for scorer in model.automations.iter().flat_map(|automation| &automation.scorers) {
            if function_ids.contains_key(scorer) {
                continue;
            }
            let slug = slugs_by_name[scorer.as_str()];
            let objects = get_objects(
                &client,
                &config,
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
        for automation in &model.automations {
            let objects = get_objects(
                &client,
                &config,
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
            plans.push((action, automation.name.as_str(), payload));
        }

        for (action, name, payload) in plans {
            match action {
                Action::Unchanged => println!("unchanged {name}"),
                Action::Create | Action::Update => {
                    let verb = if action == Action::Create { "create" } else { "update" };
                    if self.dry_run {
                        println!("would {verb} {name}");
                    } else {
                        let response = client
                            .put(format!("{base}/v1/project_score"))
                            .bearer_auth(&config.api_key)
                            .json(&payload)
                            .send()
                            .map_err(Error::Http)?;
                        check_response(response, &format!("sync automation {name:?}"))?;
                        println!("{} {name}", if action == Action::Create { "created" } else { "updated" });
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Create,
    Update,
    Unchanged,
}

fn desired_online(automation: &Automation, ids: &HashMap<String, String>) -> Value {
    json!({
        "sampling_rate": automation.sampling_rate,
        "scorers": automation.scorers.iter().map(|slug| json!({ "type": "function", "id": ids[slug] })).collect::<Vec<_>>(),
        "status": if automation.enabled { "active" } else { "paused" },
        "apply_to_root_span": automation.root,
        "apply_to_span_names": automation.span_names,
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

fn get_objects(client: &Client, config: &Braintrust, url: &str, query: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
    let response = client
        .get(url)
        .bearer_auth(&config.api_key)
        .query(query)
        .send()
        .map_err(Error::Http)?;
    let value = check_response(response, url)?;
    value["objects"]
        .as_array()
        .cloned()
        .ok_or_else(|| Error::Api(format!("{url} returned no objects array")))
}

fn single(mut objects: Vec<Value>, kind: &str, name: &str) -> Result<Option<Value>, Error> {
    if objects.len() > 1 {
        return Err(Error::Api(format!("multiple {kind}s matched {name:?}")));
    }
    Ok(objects.pop())
}

fn check_response(response: reqwest::blocking::Response, context: &str) -> Result<Value, Error> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().map_err(Error::Http)?;
        return Err(Error::Api(format!(
            "{context}: HTTP {status}: {}",
            body.chars().take(500).collect::<String>()
        )));
    }
    response.json().map_err(Error::Http)
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: String },
    NoAutomations,
    ScorerPlan(scg::Error),
    Config(crate::conf::Error),
    Http(reqwest::Error),
    MissingScorer(String),
    Api(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => write!(formatter, "failed to read shape {}: {source}", path.display()),
            Self::InvalidShape { details } => write!(formatter, "invalid shape:\n{details}"),
            Self::NoAutomations => formatter.write_str("the shape declares no automation blocks"),
            Self::ScorerPlan(source) => source.fmt(formatter),
            Self::Config(source) => source.fmt(formatter),
            Self::Http(source) => write!(formatter, "Braintrust request failed: {source}"),
            Self::MissingScorer(name) => write!(
                formatter,
                "scorer {name:?} is not pushed in the selected Braintrust project; run `bts build` and `bt functions push` first"
            ),
            Self::Api(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::conf::Error> for Error {
    fn from(source: crate::conf::Error) -> Self {
        Self::Config(source)
    }
}
