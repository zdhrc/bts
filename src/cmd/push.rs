use crate::cmd::{logging, render_diags};
use crate::conf::{Braintrust, Settings};
use crate::{dsl, scg};
use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Debug, clap::Args)]
#[command(about = "push scorers from bts shapes to Braintrust, creating or updating them by slug")]
pub struct Args {
    /// bts shape files whose scorer blocks should be pushed
    #[arg(value_name = "PATH", num_args = 1.., required = true)]
    paths: Vec<PathBuf>,

    /// force the emitted language for code scorers, overriding each block's lang
    #[arg(long, value_name = "LANG", value_enum)]
    lang: Option<Lang>,

    /// show what would be created or updated without pushing; still reads
    /// from Braintrust, so credentials are required
    #[arg(long)]
    dry_run: bool,

    /// print the final run summary as JSON on stdout
    #[arg(long, conflicts_with = "dry_run")]
    json: bool,

    /// print phase timings to stderr while running
    #[arg(long)]
    profile: bool,
}

// clap-facing mirror so the dsl stays clap-free
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Lang {
    Python,
    Typescript,
}

impl Lang {
    fn into_model(self) -> dsl::ScorerLang {
        match self {
            Self::Python => dsl::ScorerLang::Python,
            Self::Typescript => dsl::ScorerLang::Typescript,
        }
    }
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let settings = Settings::load()?;
        let log_path = logging::init("push", self.profile, &settings);
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            argv = %env::args().skip(1).collect::<Vec<_>>().join(" "),
            "run started",
        );

        let result = self.execute(&settings, log_path.as_deref());
        if let Err(error) = &result {
            tracing::error!(%error, "run failed");
            if let Some(path) = &log_path {
                eprintln!("run log: {}", path.display());
            }
        }

        result
    }

    fn execute(self, settings: &Settings, log_path: Option<&Path>) -> Result<(), Error> {
        let started = Instant::now();

        let mut scorers = Vec::new();
        for path in &self.paths {
            let source = fs::read_to_string(path).map_err(|source| Error::ReadShape {
                path: path.clone(),
                source,
            })?;
            let source_name = path.display().to_string();
            let model = tracing::info_span!("compile", shape = %source_name)
                .in_scope(|| dsl::compile(&source))
                .map_err(|diags| Error::InvalidShape {
                    details: render_diags(&source_name, &source, &diags),
                })?;
            if model.scorers.is_empty() {
                eprintln!("warning: {source_name} declares no scorers");
            }
            scorers.extend(model.scorers);
        }
        if scorers.is_empty() {
            return Err(Error::NoScorers);
        }

        let lang = self.lang.map(Lang::into_model);
        let components = tracing::info_span!("plan").in_scope(|| scg::plan(&scorers, lang).map_err(Error::Plan))?;

        let mut config = Braintrust::from_env()?;
        config.request_timeout = settings.request_timeout;
        tracing::info!(project_id = %config.project_id, api_url = %config.api_url, dry_run = self.dry_run, "reconciling with braintrust");
        let outcomes = tracing::info_span!("push")
            .in_scope(|| scg::reconcile(&config, &components, self.dry_run))
            .map_err(Error::Push)?;

        let created = count(&outcomes, |action| matches!(action, scg::Action::Create));
        let updated = count(&outcomes, |action| matches!(action, scg::Action::Update { .. }));
        let unchanged = count(&outcomes, |action| matches!(action, scg::Action::Unchanged));
        tracing::info!(created, updated, unchanged, "reconcile finished");

        if self.json {
            let summary = Summary {
                project_id: config.project_id.to_string(),
                components: outcomes.iter().map(ComponentSummary::new).collect(),
                created,
                updated,
                unchanged,
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                log: log_path.map(|path| path.display().to_string()),
            };
            println!("{}", serde_json::to_string(&summary)?);
            return Ok(());
        }

        for outcome in &outcomes {
            let verb = match (&outcome.action, self.dry_run) {
                (scg::Action::Create, false) => "created",
                (scg::Action::Create, true) => "would create",
                (scg::Action::Update { .. }, false) => "updated",
                (scg::Action::Update { .. }, true) => "would update",
                (scg::Action::Unchanged, _) => "unchanged",
            };
            let form = match outcome.lang {
                Some(dsl::ScorerLang::Python) => "python",
                Some(dsl::ScorerLang::Typescript) => "typescript",
                None => "judge",
            };
            println!("scorer {} ({form})  {verb}", outcome.slug);
            if let scg::Action::Update { diff } = &outcome.action {
                println!("{diff}");
            }
        }
        if self.dry_run {
            println!("{created} would be created, {updated} would be updated, {unchanged} unchanged");
            println!("dry run: nothing was pushed");
        } else {
            println!(
                "{created} created, {updated} updated, {unchanged} unchanged in project {}",
                config.project_id
            );
        }

        Ok(())
    }
}

fn count(outcomes: &[scg::Outcome], matches: impl Fn(&scg::Action) -> bool) -> usize {
    outcomes.iter().filter(|outcome| matches(&outcome.action)).count()
}

#[derive(serde::Serialize)]
struct Summary {
    project_id: String,
    components: Vec<ComponentSummary>,
    created: usize,
    updated: usize,
    unchanged: usize,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    log: Option<String>,
}

#[derive(serde::Serialize)]
struct ComponentSummary {
    name: String,
    slug: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    lang: Option<&'static str>,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

impl ComponentSummary {
    fn new(outcome: &scg::Outcome) -> Self {
        Self {
            name: outcome.name.clone(),
            slug: outcome.slug.clone(),
            kind: "scorer",
            lang: outcome.lang.map(|lang| match lang {
                dsl::ScorerLang::Python => "python",
                dsl::ScorerLang::Typescript => "typescript",
            }),
            action: match outcome.action {
                scg::Action::Create => "created",
                scg::Action::Update { .. } => "updated",
                scg::Action::Unchanged => "unchanged",
            },
            id: outcome.id.clone(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: String },
    NoScorers,
    Plan(scg::Error),
    Config(crate::conf::Error),
    Push(scg::client::Error),
    Encode(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => {
                write!(formatter, "failed to read shape {}: {source}", path.display())
            }
            Self::InvalidShape { details } => write!(formatter, "invalid shape:\n{details}"),
            Self::NoScorers => formatter.write_str("no scorer blocks found in the given shapes"),
            Self::Plan(source) => write!(formatter, "failed to plan the push: {source}"),
            Self::Config(source) => source.fmt(formatter),
            Self::Push(source) => write!(formatter, "failed to push to Braintrust: {source}"),
            Self::Encode(source) => write!(formatter, "failed to encode the summary: {source}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::conf::Error> for Error {
    fn from(source: crate::conf::Error) -> Self {
        Self::Config(source)
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn parses_multiple_positional_paths() {
        let cli = TestCli::parse_from(["push", "a.bt", "b.bt"]);
        assert_eq!(cli.args.paths.len(), 2);
    }

    #[test]
    fn requires_at_least_one_path() {
        assert!(TestCli::try_parse_from(["push"]).is_err());
    }

    #[test]
    fn parses_lang_values() {
        let cli = TestCli::parse_from(["push", "--lang", "typescript", "a.bt"]);
        assert!(matches!(cli.args.lang, Some(Lang::Typescript)));
    }

    #[test]
    fn json_conflicts_with_dry_run() {
        assert!(TestCli::try_parse_from(["push", "--dry-run", "--json", "a.bt"]).is_err());
    }
}
