use super::codebase::{CodebaseContext, CodebasePlan};
use crate::dsl::{JudgeOption, ScorerArg, ScorerLang, ScorerStep, Value};

/// The inputs for one scorer function.
pub struct Item<'m> {
    pub name: &'m str,
    /// The provider's identifier, which may differ from the emitted function name.
    /// It must satisfy the selected emitter's requirements.
    pub slug: &'m str,
    pub score: &'m Value,
    pub steps: &'m [ScorerStep],
}

/// Emits functions for a supported language.
pub trait Emitter {
    type Error: std::error::Error + 'static;

    /// Emits one function identifier per input item, in the same order.
    ///
    /// Each identifier must be nonempty, distinct, valid in the selected
    /// language, and name the corresponding function in the returned source.
    fn emit(&self, lang: ScorerLang, items: &[Item<'_>]) -> Result<Module, Self::Error>;
}

/// Emitted source and the exported function corresponding to each input item.
pub struct Module {
    pub source: String,
    /// Function identifiers in the same order as the emitter's input items.
    /// Each identifier names a distinct function defined in `source`.
    pub functions: Vec<String>,
}

/// Scorers grouped into one source file by their authored file stem.
pub struct CodeGroup<'a> {
    pub lang: ScorerLang,
    pub stem: &'a str,
    pub items: &'a [Item<'a>],
}

/// A judge definition passed to a target without choosing a template syntax.
pub struct Judge<'a> {
    pub lang: ScorerLang,
    pub name: &'a str,
    pub slug: &'a str,
    pub model: &'a str,
    pub prompt: &'a Value,
    pub options: &'a [JudgeOption],
}

/// Adds integration-specific source and codebase conventions to emitted code.
pub trait Target {
    type Error: std::error::Error + 'static;

    /// Returns a provider-defined identifier that satisfies the selected emitter's requirements.
    ///
    /// Returning `None` rejects the name. Assembly also rejects duplicate slugs.
    fn slug(&self, name: &str) -> Option<String>;
    /// Returns the implementation and integration files for one emitted module.
    fn code(&self, group: CodeGroup<'_>, module: Module) -> Result<Vec<SourceFile>, Self::Error>;
    fn judge(&self, judge: Judge<'_>) -> Result<SourceFile, Self::Error>;
    fn codebase(&self, assembly: &Assembly, context: &CodebaseContext<'_>) -> Result<CodebasePlan, Self::Error>;
}

pub struct Assembly {
    pub files: Vec<SourceFile>,
}

pub struct SourceFile {
    pub name: String,
    pub contents: String,
    pub slugs: Vec<String>,
    pub lang: ScorerLang,
}

/// Returns the shared name for a scorer input.
pub fn arg_name(arg: ScorerArg) -> &'static str {
    match arg {
        ScorerArg::Input => "input",
        ScorerArg::Output => "output",
        ScorerArg::Expected => "expected",
        ScorerArg::Metadata => "metadata",
    }
}

/// Adds agent instrumentation and package conventions without requiring scorer-only targets to implement them.
pub trait AgentTarget: Target {
    /// Source implementing the generated runtime's span hooks in the selected language.
    fn tracing(&self, lang: ScorerLang) -> &str;

    /// Plans agent, evaluation, and supporting source files against caller-supplied existing files.
    /// Source names are paths relative to the codebase root.
    fn agent_codebase(
        &self,
        sources: &[SourceFile],
        lang: ScorerLang,
        context: &CodebaseContext<'_>,
    ) -> Result<CodebasePlan, Self::Error>;
}
