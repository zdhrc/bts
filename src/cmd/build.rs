use crate::cmd::{logging, render_diags};
use crate::conf::Settings;
use crate::{dsl, scg};
use std::{env, fmt, fs, path::PathBuf};

#[derive(Debug, clap::Args)]
#[command(about = "build Braintrust SDK scorer files for `bt functions push`")]
pub struct Args {
    /// bts shape file whose scorer blocks should be built
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// force the emitted language for scorers, overriding each code block's lang
    #[arg(long, value_name = "LANG", value_enum)]
    lang: Option<Lang>,

    /// directory the source files are written into; defaults to the current directory
    #[arg(long, value_name = "DIR", default_value = ".")]
    out: PathBuf,

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
        let log_path = logging::init("build", self.profile, &settings);
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            argv = %env::args().skip(1).collect::<Vec<_>>().join(" "),
            "run started",
        );

        let result = self.execute();
        if let Err(error) = &result {
            tracing::error!(%error, "run failed");
            if let Some(path) = &log_path {
                eprintln!("run log: {}", path.display());
            }
        }

        result
    }

    fn execute(self) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(|source| Error::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        let source_name = self.from.display().to_string();
        let model = tracing::info_span!("compile")
            .in_scope(|| dsl::compile(&source))
            .map_err(|diags| Error::InvalidShape {
                details: render_diags(&source_name, &source, &diags),
            })?;
        if model.scorers.is_empty() {
            return Err(Error::NoScorers);
        }

        let project = crate::conf::project_name()?;
        let lang = self.lang.map(Lang::into_model);
        let assembly =
            tracing::info_span!("assemble").in_scope(|| scg::assemble(&model.scorers, lang, &project).map_err(Error::Plan))?;
        let built = tracing::info_span!("build")
            .in_scope(|| {
                scg::build(&assembly, &self.out, |path, diff| {
                    println!("updating {}:\n{diff}", path.display());
                })
            })
            .map_err(Error::Build)?;

        for source in &built {
            let verb = match source.action {
                scg::builder::Action::Created => "created",
                scg::builder::Action::Updated => "updated",
                scg::builder::Action::Unchanged => "unchanged",
            };
            println!("{verb} {} ({})", source.path.display(), source.slugs.join(", "));
        }
        let written = built
            .iter()
            .filter(|source| source.action != scg::builder::Action::Unchanged)
            .count();
        tracing::info!(written, scorers = model.scorers.len(), "build finished");
        let names = built
            .iter()
            .map(|source| {
                source
                    .path
                    .file_name()
                    .expect("generated file has a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "from {} run: bt functions push --if-exists replace {names}",
            self.out.display()
        );

        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: String },
    NoScorers,
    Plan(scg::Error),
    Config(crate::conf::Error),
    Build(scg::builder::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => {
                write!(formatter, "failed to read shape {}: {source}", path.display())
            }
            Self::InvalidShape { details } => write!(formatter, "invalid shape:\n{details}"),
            Self::NoScorers => formatter.write_str("the shape declares no scorer blocks"),
            Self::Plan(source) => write!(formatter, "failed to build the scorers: {source}"),
            Self::Config(source) => source.fmt(formatter),
            Self::Build(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::conf::Error> for Error {
    fn from(source: crate::conf::Error) -> Self {
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
    fn out_defaults_to_the_current_directory() {
        let cli = TestCli::parse_from(["build", "--from", "a.bt"]);
        assert_eq!(cli.args.out, PathBuf::from("."));
    }
}
