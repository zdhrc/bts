//! In-memory library API over the existing compiler, planner, and materializer.
use crate::{dsl, sdg};
use serde::Serialize;
use std::time::SystemTime;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Render the same language specification used by `bts setup skill`.
/// CLI-only installation and command help are omitted.
pub fn language_reference() -> String {
    format!(
        "<!-- BTS release {} -->\n{}",
        version(),
        crate::reference::render_skill(&dsl::spec::SPEC, "")
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// UTF-8 byte offsets, half-open. Line/column are one-based Unicode scalar positions.
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub phase: String,
    pub message: String,
    pub location: Option<Location>,
}

fn diagnostic(source: &str, diag: dsl::Diag) -> Diagnostic {
    fn position(source: &str, offset: usize) -> (usize, usize) {
        let prefix = &source[..offset];
        (
            prefix.matches('\n').count() + 1,
            prefix.chars().rev().take_while(|&ch| ch != '\n').count() + 1,
        )
    }
    let start = diag.r#where.start;
    let end = diag.r#where.end;
    let (line, column) = position(source, start);
    let (end_line, end_column) = position(source, end);
    Diagnostic {
        phase: diag.when.to_string(),
        message: diag.what,
        location: Some(Location {
            start,
            end,
            line,
            column,
            end_line,
            end_column,
        }),
    }
}

pub fn check(source: &str) -> Vec<Diagnostic> {
    match dsl::compile(source) {
        Ok(_) => Vec::new(),
        Err(diags) => diags.into_iter().map(|diag| diagnostic(source, diag)).collect(),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Distribution {
    #[default]
    Linear,
    Sine,
}

/// Explicit generation inputs; no clock, configuration, or environment is consulted.
#[derive(Debug)]
pub struct GenerationOptions {
    pub count: usize,
    pub seed: u64,
    pub start: SystemTime,
    pub end: SystemTime,
    pub distribution: Distribution,
}

/// The generator's native event batch, before any filesystem or publication work.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct EventBatch(sdg::EventBatch);

impl EventBatch {
    /// Consumers decide how to handle unresolved file attachments.
    pub fn has_attachments(&self) -> bool {
        !self.0.attachments.is_empty()
    }
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub message: String,
    pub diagnostics: Vec<Diagnostic>,
}

impl Error {
    pub(crate) fn message(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            diagnostics: Vec::new(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

pub fn generate(source: &str, options: GenerationOptions) -> Result<EventBatch, Error> {
    if options.count == 0 {
        return Err(Error::message("count must be greater than zero"));
    }
    let GenerationOptions {
        count,
        seed,
        start,
        end,
        distribution,
    } = options;
    let over = end
        .duration_since(start)
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| Error::message("window end must be after its start"))?;
    let model = dsl::compile(source).map_err(|diags| Error {
        message: "source is invalid".into(),
        diagnostics: diags.into_iter().map(|diag| diagnostic(source, diag)).collect(),
    })?;
    let distribution = match distribution {
        Distribution::Linear => sdg::Distribution::Linear,
        Distribution::Sine => sdg::Distribution::Sine,
    };
    let events = sdg::generate(model, count, over, distribution, end, seed).map_err(|error| match error {
        sdg::Error::Plan(error) => Error {
            message: error.to_string(),
            diagnostics: vec![diagnostic(
                source,
                dsl::Diag {
                    when: dsl::DiagPhase::Generation,
                    what: error.to_string(),
                    r#where: error.range,
                },
            )],
        },
        other => Error::message(other),
    })?;
    Ok(EventBatch(events))
}
