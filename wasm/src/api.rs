//! Browser option decoding and policy. All language work is delegated to BTS.
pub use bts::api::{Diagnostic, Error, EventBatch, check, language_reference, version};
use serde::Deserialize;
use std::time::SystemTime;

#[derive(Debug, Default, Deserialize)]
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
    pub seed: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub distribution: Distribution,
}

pub(crate) fn error(message: impl ToString) -> Error {
    Error {
        message: message.to_string(),
        diagnostics: Vec::new(),
    }
}

pub fn generate(source: &str, options: GenerationOptions) -> Result<EventBatch, Error> {
    if options.seed.is_empty() || !options.seed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(error("seed must be a decimal u64 string"));
    }
    let seed = options.seed.parse::<u64>().map_err(error)?;
    let timestamp = |value: &str| -> Result<SystemTime, Error> {
        let date = chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| error("timestamp must be RFC 3339 with an explicit offset"))?;
        if !(0..=253_402_300_799).contains(&date.timestamp()) {
            return Err(error("timestamp must be between 1970 and 9999 UTC"));
        }
        Ok(SystemTime::from(date))
    };
    let options = bts::api::GenerationOptions {
        count: options.count as usize,
        seed,
        start: timestamp(&options.start)?,
        end: timestamp(&options.end)?,
        distribution: match options.distribution {
            Distribution::Linear => bts::api::Distribution::Linear,
            Distribution::Sine => bts::api::Distribution::Sine,
        },
    };
    let batch = bts::api::generate(source, options)?;
    if batch.has_attachments() {
        return Err(error(
            "file attachments require native BTS; browser generation cannot resolve local files",
        ));
    }
    Ok(batch)
}
