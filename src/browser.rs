//! Filesystem-free API shared by native parity tests and the WebAssembly bindings.
use crate::{dsl, sdg};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Distribution {
    #[default]
    Linear,
    Sine,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationOptions {
    pub count: u32,
    /// Decimal u64 string, preserving all 64 bits across JavaScript's number boundary.
    pub seed: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub distribution: Distribution,
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

pub fn generate(source: &str, options: GenerationOptions) -> Result<Value, Error> {
    if options.count == 0 {
        return Err(Error::message("count must be greater than zero"));
    }
    if options.seed.is_empty() || !options.seed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::message("seed must be a decimal u64 string"));
    }
    let seed = options.seed.parse::<u64>().map_err(Error::message)?;
    let timestamp = |value: &str| -> Result<SystemTime, Error> {
        let date = chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| Error::message("timestamp must be RFC 3339 with an explicit offset"))?;
        // Keep conversion, arithmetic, and formatting in a portable, supported range.
        if !(0..=253_402_300_799).contains(&date.timestamp()) {
            return Err(Error::message("timestamp must be between 1970 and 9999 UTC"));
        }
        Ok(SystemTime::from(date))
    };
    let start = timestamp(&options.start)?;
    let end = timestamp(&options.end)?;
    let over = end
        .duration_since(start)
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| Error::message("window end must be after its start"))?;
    let model = dsl::compile(source).map_err(|diags| Error {
        message: "source is invalid".into(),
        diagnostics: diags.into_iter().map(|diag| diagnostic(source, diag)).collect(),
    })?;
    let distribution = match options.distribution {
        Distribution::Linear => sdg::Distribution::Linear,
        Distribution::Sine => sdg::Distribution::Sine,
    };
    let events = sdg::generate(model, options.count as usize, over, distribution, end, seed).map_err(|error| match error {
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
    if events.has_attachments() {
        return Err(Error::message(
            "file attachments require native BTS; browser generation cannot resolve local files",
        ));
    }
    serde_json::to_value(events).map_err(Error::message)
}
