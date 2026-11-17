//! Functions a provider supplies to the mocking consumers.
//!
//! The language knows a provider function by its descriptor alone. A [`Function`]
//! is the half that runs: an evaluator for data generation and a helper for each
//! language agents are generated in. A consumer the function gives nothing to
//! cannot run it, and [`Function::runs`] says so before anything is generated.

use super::ScorerLang;
use super::data::{AttachmentFormat, UnsupportedAttachment, Value};
use rand::Rng;
use rand_xoshiro::Xoshiro256PlusPlus;

/// What runs a compiled model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Consumer {
    /// Data generation, in Rust.
    Data,
    /// A generated agent in one language.
    Agent(ScorerLang),
    /// Generated scorer code, in the language of each scorer.
    Scorer,
}

impl Consumer {
    /// The consumer's name in diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Data => "data generation",
            Self::Agent(ScorerLang::Python) => "Python agent generation",
            Self::Agent(ScorerLang::Typescript) => "TypeScript agent generation",
            Self::Scorer => "scorer generation",
        }
    }
}

/// The implementations of one provider function.
#[derive(Debug)]
pub struct Function {
    /// The name the provider's schema declares.
    pub name: &'static str,
    /// Checks the arguments of a call beyond what the descriptor declares.
    pub check: Option<Check>,
    /// Evaluates a call during data generation.
    pub data: Option<Evaluate>,
    /// The helper a generated Python agent calls.
    pub python: Option<Helper>,
    /// The helper a generated TypeScript agent calls.
    pub typescript: Option<Helper>,
}

impl Function {
    /// The helper for agents generated in one language.
    pub fn helper(&self, lang: ScorerLang) -> Option<&Helper> {
        match lang {
            ScorerLang::Python => self.python.as_ref(),
            ScorerLang::Typescript => self.typescript.as_ref(),
        }
    }

    /// Whether a consumer can run the function.
    ///
    /// Scorer code ships without a runtime module, so it runs no provider function.
    pub fn runs(&self, consumer: Consumer) -> bool {
        match consumer {
            Consumer::Data => self.data.is_some(),
            Consumer::Agent(lang) => self.helper(lang).is_some(),
            Consumer::Scorer => false,
        }
    }
}

/// Checks the arguments of one call as compiled, and returns what is wrong with them.
pub type Check = fn(&[bts_lang::Value]) -> Result<(), String>;

/// Evaluates one call from its evaluated arguments.
pub type Evaluate = fn(&[Value], &mut Evaluation<'_>) -> Result<Value, EvaluateError>;

/// What an evaluator may use besides its arguments.
pub struct Evaluation<'a> {
    pub(crate) rng: &'a mut Xoshiro256PlusPlus,
    pub(crate) format: &'a dyn AttachmentFormat,
}

impl Evaluation<'_> {
    /// Fills a buffer from the random stream of the expression being evaluated.
    pub fn fill(&mut self, bytes: &mut [u8]) {
        self.rng.fill(bytes);
    }

    /// Draws text of the given length from a set of characters.
    pub fn random_text(&mut self, charset: &[u8], length: usize) -> String {
        random_text(charset, length, self.rng)
    }

    /// Presents a value the way the selected format writes it.
    pub fn project(&self, value: &Value) -> Result<serde_json::Value, UnsupportedAttachment> {
        value.project(self.format)
    }
}

pub(crate) fn random_text(charset: &[u8], length: usize, rng: &mut Xoshiro256PlusPlus) -> String {
    (0..length)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect()
}

/// Why a call could not be evaluated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvaluateError {
    /// An argument has the wrong shape for its position.
    Shape,
    /// The selected format cannot present an attachment among the arguments.
    UnsupportedAttachment,
    /// The arguments are not valid for the function, with the reason.
    Invalid(&'static str),
}

/// The function a generated agent calls, defined in its runtime module.
#[derive(Clone, Copy, Debug)]
pub struct Helper {
    /// The name of the function in the runtime text. A call passes the arguments in order.
    pub name: &'static str,
    /// The name generated code reads it by, when that differs.
    pub reads_as: Option<&'static str>,
    /// Whether a call passes the agent's random source after the arguments.
    ///
    /// Such a helper is defined with the runtime's own samplers.
    pub random: bool,
    /// Whether the result is a count, which typed code can rely on.
    pub counts: bool,
    /// The definition of the function, starting on a new line.
    pub text: &'static str,
}
