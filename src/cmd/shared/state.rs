use super::client;
use crate::{
    cmd::shared::{
        client::Client,
        select::{self, ResourceSelector},
    },
    conf,
    dsl::{Automation, AutomationKind, Dataset, Model},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceKind {
    Dataset,
    TopicsAutomation,
    ScorerAutomation,
    Facet,
    TopicMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrackedResource {
    pub kind: ResourceKind,
    pub name: String,
    pub slug: Option<String>,
    pub id: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub pending: Option<PendingOperation>,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ShapeState {
    pub resources: Vec<TrackedResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PendingOperation {
    Regenerate { start_xact_id: String },
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SyncState {
    version: u32,
    projects: BTreeMap<String, BTreeMap<String, ShapeState>>,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    project: String,
    #[serde(skip)]
    shape: String,
    #[serde(skip)]
    _lock: Option<fs::File>,
}

#[derive(Debug)]
pub(crate) enum Error {
    Api(String),
    Http(client::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(message) => f.write_str(message),
            Self::Http(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}

fn io(error: impl std::fmt::Display) -> Error {
    Error::Api(format!("sync state: {error}"))
}

fn check_response(response: client::Response, context: &str) -> Result<serde_json::Value, Error> {
    client::checked(response, context).map_err(Error::Api)
}

fn get_objects(client: &Client, url: &str, query: &[(&str, &str)]) -> Result<Vec<serde_json::Value>, Error> {
    let response = client.get(url).query(query).send().map_err(Error::Http)?;
    check_response(response, "lookup tracked resource")?["objects"]
        .as_array()
        .cloned()
        .ok_or_else(|| Error::Api("resource lookup returned no objects array".to_owned()))
}

fn single(mut objects: Vec<serde_json::Value>, kind: &str, name: &str) -> Result<Option<serde_json::Value>, Error> {
    if objects.len() > 1 {
        return Err(Error::Api(format!("multiple {kind}s matched {name:?}")));
    }
    Ok(objects.pop())
}

impl SyncState {
    pub fn load(from: &Path, project: &str, dry_run: bool) -> Result<Self, Error> {
        let root = conf::project_root().map_err(io)?;
        let source = fs::canonicalize(from).map_err(io)?;
        let canonical_root = fs::canonicalize(&root).map_err(io)?;
        let shape = source
            .strip_prefix(&canonical_root)
            .unwrap_or(&source)
            .to_string_lossy()
            .into_owned();
        let path = root.join(".bt/bts/state.json");
        Self::open(path, project, shape, dry_run)
    }

    fn open(path: PathBuf, project: &str, shape: String, dry_run: bool) -> Result<Self, Error> {
        // one writer at a time, even across commands and projects
        let lock = if dry_run {
            None
        } else {
            fs::create_dir_all(path.parent().expect("state directory")).map_err(io)?;
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path.with_extension("lock"))
                .map_err(io)?;
            file.try_lock()
                .map_err(|error| io(format!("another sync may be running: {error}")))?;
            Some(file)
        };
        let mut state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).map_err(io)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                version: 1,
                projects: BTreeMap::new(),
                path: PathBuf::new(),
                project: String::new(),
                shape: String::new(),
                _lock: None,
            },
            Err(error) => return Err(io(error)),
        };
        if state.version != 1 {
            return Err(io("unsupported state version"));
        }
        state.path = path;
        state.project = project.to_owned();
        state.shape = shape;
        state._lock = lock;
        state
            .projects
            .entry(project.to_owned())
            .or_default()
            .entry(state.shape.clone())
            .or_default();
        Ok(state)
    }

    fn shape(&self) -> &ShapeState {
        &self.projects[&self.project][&self.shape]
    }
    fn shape_mut(&mut self) -> &mut ShapeState {
        self.projects
            .get_mut(&self.project)
            .expect("project state")
            .get_mut(&self.shape)
            .expect("shape state")
    }

    // track what we own, read the actual content from braintrust
    pub fn save(&self) -> Result<(), Error> {
        if self._lock.is_none() {
            return Err(io("cannot save a dry-run state"));
        }
        let temp = self.path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(io)?;
            file.write_all(&serde_json::to_vec_pretty(self).map_err(io)?).map_err(io)?;
            file.sync_all().map_err(io)?;
            fs::rename(&temp, &self.path).map_err(io)?;
            fs::File::open(self.path.parent().expect("state directory"))
                .map_err(io)?
                .sync_all()
                .map_err(io)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    pub fn rules<'a>(
        &self,
        model: &'a Model,
        kind: &ResourceKind,
        selectors: &[ResourceSelector],
    ) -> Result<(Vec<&'a Automation>, HashSet<String>), Error> {
        let selection_type = match kind {
            ResourceKind::TopicsAutomation => select::AutomationType::Topics,
            ResourceKind::ScorerAutomation => select::AutomationType::Scorers,
            _ => unreachable!(),
        };
        let locals = select::automations(model, selection_type, &[]).map_err(Error::Api)?;
        let mut known: Vec<(String, Vec<String>)> = self
            .shape()
            .resources
            .iter()
            .filter(|r| &r.kind == kind)
            .map(|r| (r.name.clone(), r.dependencies.clone()))
            .collect();
        for rule in &locals {
            let dependencies = match &rule.kind {
                AutomationKind::Topics { facets } => facets,
                AutomationKind::Scorer { scorers, .. } => scorers,
            };
            if let Some((_, prior)) = known.iter_mut().find(|(name, _)| *name == rule.name) {
                prior.extend(dependencies.iter().cloned());
            } else {
                known.push((rule.name.clone(), dependencies.clone()));
            }
        }
        let names: HashSet<_> = select::automation_names(selection_type, &known, selectors)
            .map_err(Error::Api)?
            .into_iter()
            .collect();
        Ok((locals.into_iter().filter(|rule| names.contains(&rule.name)).collect(), names))
    }

    pub fn datasets<'a>(
        &self,
        model: &'a Model,
        selectors: &[ResourceSelector],
    ) -> Result<(Vec<&'a Dataset>, HashSet<String>), Error> {
        let local = select::datasets(model, &[]).map_err(Error::Api)?;
        let mut known = local
            .iter()
            .map(|d| d.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        known.extend(
            self.shape()
                .resources
                .iter()
                .filter(|r| r.kind == ResourceKind::Dataset)
                .map(|r| r.name.clone()),
        );
        let names: HashSet<_> = select::dataset_names(&known, selectors)
            .map_err(Error::Api)?
            .into_iter()
            .collect();
        Ok((local.into_iter().filter(|d| names.contains(&d.name)).collect(), names))
    }

    pub fn get(&self, kind: &ResourceKind, name: &str) -> Option<&TrackedResource> {
        self.shape().resources.iter().find(|r| &r.kind == kind && r.name == name)
    }

    pub fn regeneration(&self, name: &str) -> Option<&str> {
        match self.get(&ResourceKind::TopicsAutomation, name)?.pending.as_ref()? {
            PendingOperation::Regenerate { start_xact_id } => Some(start_xact_id),
        }
    }

    pub fn stage(&mut self, mut resource: TrackedResource) -> Result<(), Error> {
        if resource.id.as_ref().is_some_and(String::is_empty) {
            return Err(Error::Api(format!("resource {:?} has an empty id", resource.name)));
        }
        if let Some(prior) = self
            .shape_mut()
            .resources
            .iter_mut()
            .find(|r| r.kind == resource.kind && r.name == resource.name)
        {
            if prior.id.is_some() && resource.id.is_some() && prior.id != resource.id {
                return Err(Error::Api(format!(
                    "remote identity changed for {:?}; refusing to claim the replacement",
                    resource.name
                )));
            }
            if resource.pending.is_none() {
                resource.pending = prior.pending.clone();
            }
            resource.dependencies.extend(prior.dependencies.iter().cloned());
            resource.dependencies.sort();
            resource.dependencies.dedup();
            *prior = resource;
        } else {
            self.shape_mut().resources.push(resource);
        }
        Ok(())
    }

    pub fn complete(&mut self, kind: &ResourceKind, name: &str, id: &str) -> Result<(), Error> {
        let resource = self
            .shape_mut()
            .resources
            .iter_mut()
            .find(|r| &r.kind == kind && r.name == name)
            .expect("staged resource");
        if resource.id.as_deref().is_some_and(|prior| prior != id) {
            return Err(Error::Api(format!("write changed the identity of {name:?}")));
        }
        resource.id = Some(id.to_owned());
        self.save()
    }

    pub fn regenerated(&mut self, name: &str) -> Result<(), Error> {
        self.shape_mut()
            .resources
            .iter_mut()
            .find(|r| r.kind == ResourceKind::TopicsAutomation && r.name == name)
            .expect("staged rule")
            .pending = None;
        self.save()
    }

    pub fn removals(&self, model: &Model, kind: &ResourceKind, selected: &HashSet<String>, all: bool) -> Vec<TrackedResource> {
        let mut local_names: HashSet<_> = model
            .automations
            .iter()
            .filter(|r| {
                matches!(
                    (&r.kind, kind),
                    (AutomationKind::Topics { .. }, ResourceKind::TopicsAutomation)
                        | (AutomationKind::Scorer { .. }, ResourceKind::ScorerAutomation)
                )
            })
            .map(|r| r.name.as_str())
            .collect();
        if *kind == ResourceKind::Dataset {
            local_names.extend(model.datasets.iter().map(|d| d.name.as_str()));
        }
        let mut removed: Vec<_> = self
            .shape()
            .resources
            .iter()
            .filter(|r| &r.kind == kind && selected.contains(&r.name) && !local_names.contains(r.name.as_str()))
            .cloned()
            .collect();
        if *kind == ResourceKind::TopicsAutomation {
            let local_facets: HashSet<_> = model.facets.iter().map(|f| f.name.as_str()).collect();
            let affected: HashSet<_> = self
                .shape()
                .resources
                .iter()
                .filter(|r| r.kind == ResourceKind::TopicsAutomation && selected.contains(&r.name))
                .flat_map(|r| r.dependencies.iter())
                .collect();
            for resource in &self.shape().resources {
                if matches!(resource.kind, ResourceKind::Facet | ResourceKind::TopicMap)
                    && !local_facets.contains(resource.name.as_str())
                    && (all || affected.contains(&resource.name))
                {
                    removed.push(resource.clone());
                }
            }
        }
        removed.sort_by_key(|r| match r.kind {
            ResourceKind::Dataset | ResourceKind::TopicsAutomation | ResourceKind::ScorerAutomation => 0,
            ResourceKind::TopicMap => 1,
            ResourceKind::Facet => 2,
        });
        removed
    }

    pub fn delete(&mut self, client: &Client, resource: &TrackedResource, dry_run: bool) -> Result<(), Error> {
        if client.config.project_id.to_string() != self.project {
            return Err(Error::Api("sync state belongs to a different project".to_owned()));
        }
        // another shape might still need it
        if self.projects[&self.project]
            .iter()
            .filter(|(shape, _)| *shape != &self.shape)
            .any(|(_, state)| {
                state.resources.iter().any(|r| {
                    r.kind == resource.kind
                        && r.name == resource.name
                        && (r.id.is_none() || resource.id.is_none() || r.id == resource.id)
                })
            })
        {
            println!("kept shared resource {:?}: another shape still owns it", resource.name);
            return Ok(());
        }
        let base = client.config.api_url.trim_end_matches('/');
        let app = client.config.app_url.trim_end_matches('/');
        let project = client.config.project_id.to_string();
        let objects = match resource.kind {
            ResourceKind::TopicsAutomation => {
                let response = client
                    .post(format!("{app}/api/project_automation/get"))
                    .json(&json!({"project_id":project,"name":resource.name,"limit":2}))
                    .retryable()
                    .send()
                    .map_err(Error::Http)?;
                check_response(response, "lookup removed automation")?
                    .as_array()
                    .cloned()
                    .ok_or_else(|| Error::Api("Topics lookup returned no array".to_owned()))?
            }
            ResourceKind::Dataset => get_objects(
                client,
                &format!("{base}/v1/dataset"),
                &[("project_id", &project), ("dataset_name", &resource.name), ("limit", "2")],
            )?,
            ResourceKind::ScorerAutomation => get_objects(
                client,
                &format!("{base}/v1/project_score"),
                &[
                    ("project_id", &project),
                    ("project_score_name", &resource.name),
                    ("limit", "2"),
                ],
            )?,
            ResourceKind::Facet | ResourceKind::TopicMap => get_objects(
                client,
                &format!("{base}/v1/function"),
                &[
                    ("project_id", &project),
                    (
                        "slug",
                        resource
                            .slug
                            .as_deref()
                            .ok_or_else(|| Error::Api(format!("tracked function {:?} has no slug", resource.name)))?,
                    ),
                    ("limit", "2"),
                ],
            )?,
        };
        if let Some(remote) = single(objects, "removed resource", &resource.name)? {
            let expected_name = if resource.kind == ResourceKind::TopicMap {
                format!("{} topics", resource.name)
            } else {
                resource.name.clone()
            };
            let valid_type = match resource.kind {
                ResourceKind::Dataset => true,
                ResourceKind::TopicsAutomation => remote["config"]["event_type"] == "topic",
                ResourceKind::ScorerAutomation => remote["score_type"] == "online",
                ResourceKind::Facet => remote["function_type"] == "facet" && remote["function_data"]["type"] == "facet",
                ResourceKind::TopicMap => {
                    remote["function_type"] == "classifier" && remote["function_data"]["type"] == "topic_map"
                }
            };
            let id = remote["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| Error::Api("removed resource has no id".to_owned()))?;
            if remote["project_id"] != project
                || remote["name"] != expected_name
                || !valid_type
                || resource.id.as_deref().is_some_and(|expected| expected != id)
            {
                return Err(Error::Api(format!(
                    "removed resource {:?} conflicts with the remote identity",
                    resource.name
                )));
            }
            let label = match resource.kind {
                ResourceKind::Dataset => "dataset",
                ResourceKind::Facet => "facet",
                ResourceKind::TopicMap => "topic map",
                _ => "automation",
            };
            if dry_run {
                println!("would delete {label} {:?}", resource.name);
                return Ok(());
            }
            let response = if resource.kind == ResourceKind::TopicsAutomation {
                client
                    .post(format!("{app}/api/project_automation/delete_id"))
                    .json(&json!({"id":id}))
                    .send()
            } else {
                let object = if resource.kind == ResourceKind::Dataset {
                    "dataset"
                } else if resource.kind == ResourceKind::ScorerAutomation {
                    "project_score"
                } else {
                    "function"
                };
                client.delete(format!("{base}/v1/{object}/{id}")).retryable().send()
            }
            .map_err(Error::Http)?;
            let status = response.status();
            if !status.is_success() && status.as_u16() != 404 {
                check_response(response, "delete removed resource")?;
            }
            println!("deleted {label} {:?}", resource.name);
        }
        Ok(())
    }

    pub fn removed(
        &mut self,
        resources: &[TrackedResource],
        model: &Model,
        kind: &ResourceKind,
        selected: &HashSet<String>,
    ) -> Result<(), Error> {
        self.shape_mut().resources.retain(|r| {
            !resources
                .iter()
                .any(|removed| r.kind == removed.kind && r.name == removed.name)
        });
        for resource in &mut self.shape_mut().resources {
            if &resource.kind == kind
                && matches!(kind, ResourceKind::TopicsAutomation | ResourceKind::ScorerAutomation)
                && selected.contains(&resource.name)
                && let Some(rule) = model.automations.iter().find(|rule| rule.name == resource.name)
            {
                resource.dependencies = match &rule.kind {
                    AutomationKind::Topics { facets } => facets.clone(),
                    AutomationKind::Scorer { scorers, .. } => scorers.clone(),
                };
            }
        }
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("bts-state-{}", uuid::Uuid::new_v4()))
            .join("state.json")
    }

    fn owned(kind: ResourceKind, name: &str, dependencies: &[&str]) -> TrackedResource {
        TrackedResource {
            kind,
            name: name.to_owned(),
            slug: None,
            id: Some(format!("{name}-id")),
            dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
            pending: None,
        }
    }

    #[test]
    fn one_file_keeps_all_projects_shapes_and_resource_kinds() {
        let path = path();
        for (project, shape, kind, name) in [
            ("p1", "a.bt", ResourceKind::Dataset, "cases"),
            ("p2", "a.bt", ResourceKind::TopicsAutomation, "topics"),
            ("p1", "b.bt", ResourceKind::ScorerAutomation, "scores"),
            ("p1", "a.bt", ResourceKind::Facet, "intent"),
        ] {
            let mut state = SyncState::open(path.clone(), project, shape.to_owned(), false).unwrap();
            state.stage(owned(kind, name, &[])).unwrap();
            state.save().unwrap();
        }
        let data: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(data["version"], 1);
        assert_eq!(data["projects"].as_object().unwrap().len(), 2);
        assert_eq!(data["projects"]["p1"].as_object().unwrap().len(), 2);
        assert_eq!(data["projects"]["p1"]["a.bt"]["resources"].as_array().unwrap().len(), 2);
        assert_eq!(data["projects"]["p2"]["a.bt"]["resources"][0]["kind"], "topics_automation");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn pending_regeneration_survives_saves_from_other_commands() {
        let path = path();
        let mut state = SyncState::open(path.clone(), "p", "a.bt".to_owned(), false).unwrap();
        let mut resource = owned(ResourceKind::TopicsAutomation, "topics", &["intent"]);
        resource.pending = Some(PendingOperation::Regenerate {
            start_xact_id: "123".to_owned(),
        });
        state.stage(resource).unwrap();
        state.save().unwrap();
        drop(state);
        let mut state = SyncState::open(path.clone(), "p", "a.bt".to_owned(), false).unwrap();
        state.stage(owned(ResourceKind::Dataset, "cases", &[])).unwrap();
        state.save().unwrap();
        drop(state);
        let state = SyncState::open(path.clone(), "p", "a.bt".to_owned(), true).unwrap();
        assert_eq!(state.regeneration("topics"), Some("123"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn writers_lock_the_shared_file_and_release_it_when_done() {
        let path = path();
        let state = SyncState::open(path.clone(), "p1", "a.bt".to_owned(), false).unwrap();
        assert!(SyncState::open(path.clone(), "p2", "b.bt".to_owned(), false).is_err());
        assert!(SyncState::open(path.clone(), "p2", "b.bt".to_owned(), true).is_ok());
        drop(state);
        assert!(SyncState::open(path.clone(), "p2", "b.bt".to_owned(), false).is_ok());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn dry_runs_do_not_create_or_save_state() {
        let path = path();
        let mut state = SyncState::open(path.clone(), "p", "a.bt".to_owned(), true).unwrap();
        state.stage(owned(ResourceKind::Dataset, "cases", &[])).unwrap();
        assert!(state.save().is_err());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn selected_sync_keeps_unselected_removed_rules_and_facets() {
        let model = crate::dsl::compile(
            r#"facet "new" { prompt = "Extract." } automation "new-rule" { type = "topics" facets = ["new"] scope = "trace" }"#,
        )
        .unwrap();
        let path = path();
        let mut state = SyncState::open(path.clone(), "p", "a.bt".to_owned(), true).unwrap();
        state.shape_mut().resources = vec![
            owned(ResourceKind::TopicsAutomation, "old-rule", &["old"]),
            owned(ResourceKind::Facet, "old", &[]),
            owned(ResourceKind::TopicMap, "old", &[]),
        ];
        let (_, selected) = state
            .rules(
                &model,
                &ResourceKind::TopicsAutomation,
                &["automation[\"new-rule\"]".parse().unwrap()],
            )
            .unwrap();
        assert!(
            state
                .removals(&model, &ResourceKind::TopicsAutomation, &selected, false)
                .is_empty()
        );
        let (_, selected) = state.rules(&model, &ResourceKind::TopicsAutomation, &[]).unwrap();
        assert_eq!(
            state.removals(&model, &ResourceKind::TopicsAutomation, &selected, true).len(),
            3
        );
    }

    #[test]
    fn changing_bindings_keeps_old_dependencies_until_cleanup_succeeds() {
        let model = crate::dsl::compile(
            r#"facet "new" { prompt = "Extract." } automation "rule" { type = "topics" facets = ["new"] scope = "trace" }"#,
        )
        .unwrap();
        let mut state = SyncState::open(path(), "p", "a.bt".to_owned(), true).unwrap();
        state.shape_mut().resources = vec![
            owned(ResourceKind::TopicsAutomation, "rule", &["old"]),
            owned(ResourceKind::Facet, "old", &[]),
            owned(ResourceKind::TopicMap, "old", &[]),
        ];
        state.stage(owned(ResourceKind::TopicsAutomation, "rule", &["new"])).unwrap();
        let (_, selected) = state
            .rules(&model, &ResourceKind::TopicsAutomation, &["automation.rule".parse().unwrap()])
            .unwrap();
        assert_eq!(
            state
                .removals(&model, &ResourceKind::TopicsAutomation, &selected, false)
                .len(),
            2
        );
    }
}
