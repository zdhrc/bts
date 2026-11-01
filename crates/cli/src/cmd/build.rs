use crate::config::Settings;
use crate::diag::SourceDiags;
use crate::logging;
use crate::providers::{AgentAdapter, CodeAdapter, braintrust};
use crate::{code, dsl};
use bts_core::providers::CodeProvider;
use code::AgentTarget as _;
use std::{env, fmt, fs, path::PathBuf};

#[derive(Debug, clap::Args)]
#[command(about = "build scorer files or a callable mock agent")]
pub struct Args {
    /// The artifact to generate; defaults to scorers.
    #[arg(value_enum, default_value = "scorers")]
    target: Artifact,

    /// The bts shape file to build.
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// The output language; defaults to python for agents.
    #[arg(long, value_name = "LANG", value_enum)]
    lang: Option<Lang>,

    /// The codebase root. Generated files are organized under `src/`.
    #[arg(long, value_name = "DIR", default_value = ".")]
    out: PathBuf,

    /// Print phase timings to standard error while running.
    #[arg(long)]
    profile: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Artifact {
    Scorers,
    Agent,
}

// keep command argument parsing out of the language crate
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
        let log_path = logging::init("build", self.profile, &settings);
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            argv = %env::args().skip(1).collect::<Vec<_>>().join(" "),
            "run started",
        );

        let result = match self.target {
            Artifact::Scorers => self.execute(&braintrust::Adapter),
            Artifact::Agent => self.execute_agent(&braintrust::Adapter),
        };
        if let Err(error) = &result {
            tracing::error!(%error, "run failed");
            if let Some(path) = &log_path {
                eprintln!("run log: {}", path.display());
            }
        }

        result
    }

    fn execute<A: CodeAdapter>(self, adapter: &A) -> Result<(), ExecutionError<A>> {
        let model = self.compile::<A>()?;
        if model.scorers.is_empty() {
            return Err(BuildError::NoScorers);
        }
        let provider = adapter.provider();
        let config = adapter.config().map_err(BuildError::Adapter)?;
        let lang = self.lang.map(Lang::into_model);
        let target = provider.code(&config);
        let emitter = provider.emitter();
        let assembly = tracing::info_span!("assemble")
            .in_scope(|| code::assemble(&model.scorers, lang, &emitter, &target))
            .map_err(BuildError::Plan)?;
        let files = code::writing::Directory::new(&self.out);
        let context = code::CodebaseContext {
            root: &self.out,
            files: &files,
        };
        let plan = code::plan_codebase(&assembly, &target, &context).map_err(BuildError::Codebase)?;
        self.write(plan, adapter, model.scorers.len())
    }

    fn execute_agent<A: AgentAdapter>(self, adapter: &A) -> Result<(), ExecutionError<A>>
    where
        <A::Provider as CodeProvider>::Target: code::AgentTarget,
    {
        let model = self.compile::<A>()?;
        let lang = self.lang.map(Lang::into_model).unwrap_or(dsl::ScorerLang::Python);
        let target = adapter.agent_target(&model).map_err(BuildError::Adapter)?;
        let assembly = tracing::info_span!("assemble")
            .in_scope(|| adapter.assemble_agents(&model, lang, &target))
            .map_err(BuildError::Agent)?;
        let files = code::writing::Directory::new(&self.out);
        let context = code::CodebaseContext {
            root: &self.out,
            files: &files,
        };
        let plan = target
            .agent_codebase(&assembly.files, lang, &context)
            .map_err(BuildError::Codebase)?;
        self.write(plan, adapter, model.scorers.len())
    }

    fn compile<A: CodeAdapter>(&self) -> Result<dsl::Model, ExecutionError<A>> {
        let source = fs::read_to_string(&self.from).map_err(|source| BuildError::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        let source_name = self.from.display().to_string();
        tracing::info_span!("compile")
            .in_scope(|| dsl::semantics::compile(&source))
            .map_err(|diags| BuildError::InvalidShape {
                details: SourceDiags::new(&source_name, &source, diags),
            })
    }

    fn write<A: CodeAdapter>(
        &self,
        mut plan: code::CodebasePlan,
        adapter: &A,
        scorers: usize,
    ) -> Result<(), ExecutionError<A>> {
        code::writing::format_plan(&mut plan).map_err(BuildError::Format)?;
        let built = tracing::info_span!("build")
            .in_scope(|| {
                code::writing::build(&plan, &self.out, |path, diff| {
                    println!("updating {}:\n{diff}", path.display());
                })
            })
            .map_err(BuildError::Build)?;

        for source in &built {
            let verb = match source.action {
                code::writing::Action::Created => "created",
                code::writing::Action::Updated => "updated",
                code::writing::Action::Unchanged => "unchanged",
            };
            if source.slugs.is_empty() {
                println!("{verb} {}", source.path.display());
            } else {
                println!("{verb} {} ({})", source.path.display(), source.slugs.join(", "));
            }
        }
        let written = built
            .iter()
            .filter(|source| source.action != code::writing::Action::Unchanged)
            .count();
        tracing::info!(written, scorers, "build finished");
        if matches!(self.target, Artifact::Scorers) {
            adapter.report_build(&self.out, &built);
        }

        Ok(())
    }
}

pub type Error = ExecutionError<braintrust::Adapter>;

type ExecutionError<A> = BuildError<
    <A as CodeAdapter>::Error,
    <<<A as CodeAdapter>::Provider as CodeProvider>::Emitter as code::Emitter>::Error,
    <<<A as CodeAdapter>::Provider as CodeProvider>::Target as code::Target>::Error,
>;

#[derive(Debug)]
pub enum BuildError<A, E, T> {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: SourceDiags },
    NoScorers,
    Agent(String),
    Format(String),
    Plan(code::AssemblyError<E, T>),
    Codebase(T),
    Config(crate::config::Error),
    Adapter(A),
    Build(code::writing::Error),
}

impl<A: fmt::Display, E: fmt::Display, T: fmt::Display> fmt::Display for BuildError<A, E, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => {
                write!(formatter, "failed to read shape {}: {source}", path.display())
            }
            Self::InvalidShape { details } => write!(formatter, "invalid shape:\n{details}"),
            Self::Agent(reason) => write!(formatter, "failed to build agents: {reason}"),
            Self::Format(reason) => write!(formatter, "failed to format generated code: {reason}"),
            Self::NoScorers => formatter.write_str("the shape declares no scorer blocks"),
            Self::Plan(source) => {
                let message = match source {
                    code::AssemblyError::Plan(source) => crate::diag::build_error(source),
                    _ => source.to_string(),
                };
                write!(formatter, "failed to build the scorers: {message}")
            }
            Self::Codebase(source) => source.fmt(formatter),
            Self::Config(source) => source.fmt(formatter),
            Self::Adapter(source) => source.fmt(formatter),
            Self::Build(source) => source.fmt(formatter),
        }
    }
}

impl<A, E, T> std::error::Error for BuildError<A, E, T>
where
    A: std::error::Error + 'static,
    E: std::error::Error + 'static,
    T: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidShape { details } => Some(details),
            Self::ReadShape { source, .. } => Some(source),
            Self::Plan(source) => Some(source),
            Self::Codebase(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::Adapter(source) => Some(source),
            Self::Build(source) => Some(source),
            _ => None,
        }
    }
}

impl<A, E, T> From<crate::config::Error> for BuildError<A, E, T> {
    fn from(source: crate::config::Error) -> Self {
        Self::Config(source)
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
    fn requires_a_shape() {
        assert!(TestCli::try_parse_from(["build"]).is_err());
    }

    #[test]
    fn parses_lang_and_out() {
        let cli = TestCli::parse_from(["build", "--from", "a.bt", "--lang", "typescript", "--out", "gen"]);
        assert!(matches!(cli.args.lang, Some(Lang::Typescript)));
        assert_eq!(cli.args.out, PathBuf::from("gen"));
    }

    #[test]
    fn parses_agent_target() {
        let cli = TestCli::parse_from(["build", "agent", "--from", "a.bt"]);
        assert!(matches!(cli.args.target, Artifact::Agent));
        let cli = TestCli::parse_from(["build", "--from", "a.bt"]);
        assert!(matches!(cli.args.target, Artifact::Scorers));
    }

    #[test]
    fn out_defaults_to_the_current_directory() {
        let cli = TestCli::parse_from(["build", "--from", "a.bt"]);
        assert_eq!(cli.args.out, PathBuf::from("."));
    }
    #[test]
    fn scorer_execution_accepts_an_adapter_without_agent_support() {
        struct ScorerOnly;
        impl CodeAdapter for ScorerOnly {
            type Provider = bts_core::providers::braintrust::Bundle;
            type Error = std::io::Error;
            fn provider(&self) -> Self::Provider {
                bts_core::providers::braintrust::Bundle
            }
            fn config(&self) -> Result<String, Self::Error> {
                Err(std::io::Error::other("scorer settings"))
            }
            fn report_build(&self, _: &std::path::Path, _: &[code::writing::Built]) {
                unreachable!()
            }
        }
        let path = std::env::temp_dir().join(format!("bts-scorer-adapter-{}.bt", uuid::Uuid::new_v4()));
        fs::write(&path, r#"scorer "quality" { code { score = 1 } }"#).unwrap();
        let cli = TestCli::parse_from(["build", "--from", path.to_str().unwrap()]);
        let result = cli.args.execute(&ScorerOnly);
        fs::remove_file(path).unwrap();
        assert!(matches!(result, Err(BuildError::Adapter(error)) if error.to_string() == "scorer settings"));
    }
}
