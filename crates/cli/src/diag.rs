use crate::dsl::Diags;
use std::fmt;

// keep the original source and locations until the cli renders the failure
#[derive(Debug)]
pub struct SourceDiags {
    pub source_name: String,
    pub source: String,
    pub diags: Diags,
}

impl SourceDiags {
    pub fn new(source_name: &str, source: &str, diags: Diags) -> Self {
        Self {
            source_name: source_name.to_owned(),
            source: source.to_owned(),
            diags,
        }
    }
}

impl fmt::Display for SourceDiags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diag) in self.diags.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            formatter.write_str(&diag.render(&self.source_name, &self.source))?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceDiags {}

// command suggestions belong to the interface, while core reports the failure
pub fn braintrust_config_error(error: &bts_core::providers::braintrust::config::Error) -> String {
    match error {
        bts_core::providers::braintrust::config::Error::MissingProjectName { .. }
        | bts_core::providers::braintrust::config::Error::MissingProjectId { .. } => {
            format!("{error}; select one with `bt switch`")
        }
        _ => error.to_string(),
    }
}

pub fn build_error(error: &crate::code::Error) -> String {
    match error {
        crate::code::Error::DuplicateSlug { .. } => format!("{error}; rename one"),
        crate::code::Error::DuplicateFile { .. } => format!("{error}; change a code scorer's file stem"),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_language_diagnostics_until_rendering() {
        let source = "trace \"run\" {\n\toutput = }";
        let diags = crate::dsl::semantics::compile(source).unwrap_err();
        let expected = diags
            .iter()
            .map(|diag| diag.render("shape.bt", source))
            .collect::<Vec<_>>()
            .join("\n");
        let report = SourceDiags::new("shape.bt", source, diags.clone());
        assert_eq!(report.diags, diags);
        assert_eq!(report.source, source);
        assert_eq!(report.to_string(), expected);
    }

    #[test]
    fn adds_command_guidance_without_changing_the_sdk_error() {
        let error = bts_core::providers::braintrust::config::Error::MissingProjectId {
            path: "context.json".into(),
        };
        assert!(!error.to_string().contains("bt switch"));
        assert!(braintrust_config_error(&error).ends_with("select one with `bt switch`"));
    }
}
