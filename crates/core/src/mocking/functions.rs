//! Functions a provider supplies to the mocking consumers.
//!
//! The language knows a provider function by its descriptor alone. A [`Function`]
//! is the half that runs: an evaluator for data generation and a helper for each
//! language agents are generated in. A helper may also say how scorer code calls
//! it. A consumer the function gives nothing to cannot run it, and
//! [`Function::runs`] says so before anything is generated.

use super::ScorerLang;
use super::data::{AttachmentFormat, UnsupportedAttachment, Value};
use rand::Rng;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::borrow::Cow;

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
#[derive(Clone, Copy, Debug)]
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

    /// The name scorer code in one language calls the function by.
    pub fn scorer(&self, lang: ScorerLang) -> Option<&'static str> {
        self.helper(lang)?.scorer
    }

    /// Whether a consumer can run the function.
    ///
    /// Scorer code ships without a runtime module, so it runs a function only when
    /// the helpers say how a scorer calls it and bring the text that defines it.
    pub fn runs(&self, consumer: Consumer) -> bool {
        match consumer {
            Consumer::Data => self.data.is_some(),
            Consumer::Agent(lang) => self.helper(lang).is_some(),
            Consumer::Scorer => [ScorerLang::Python, ScorerLang::Typescript]
                .into_iter()
                .all(|lang| self.scorer(lang).is_some()),
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
    pub(crate) seed: u64,
    pub(crate) trace_index: usize,
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

    /// Presents a value as plain data, which no attachment has.
    pub fn plain(&self, value: &Value) -> Result<serde_json::Value, UnsupportedAttachment> {
        value.project(&())
    }

    /// The random stream of the expression being evaluated.
    pub fn random(&mut self) -> &mut impl Rng {
        &mut *self.rng
    }

    /// The seed of the run and the index of the trace being generated.
    ///
    /// A value drawn from these alone is the same wherever and however often it is
    /// evaluated in one trace, and a generated agent given the seed can draw it too.
    pub fn address(&self) -> (u64, usize) {
        (self.seed, self.trace_index)
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
    Invalid(Cow<'static, str>),
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
    /// Whether a call needs the seed of the run.
    ///
    /// The random source such a helper is passed carries the seed as `seed` and
    /// which trace of the run the agent is as `trace`. An agent that calls one takes
    /// an optional seed and trace and wraps its random source with `seeded`. It
    /// reads its trace index with `trace_of`, or `traceOf` in TypeScript. The
    /// helper's text defines both.
    pub seeded: bool,
    /// Whether the result is a count, which typed code can rely on.
    pub counts: bool,
    /// Whether the result is text, which typed code can rely on.
    pub texts: bool,
    /// The definition of the function, starting on a new line.
    ///
    /// It is empty when the definition is part of a runtime the helper names.
    pub text: &'static str,
    /// The operation the helper performs for this function, when one helper serves several.
    ///
    /// A call then passes the operation, the arguments as one list, and the random
    /// source when the helper takes one.
    pub operation: Option<&'static str>,
    /// The text several helpers of one provider are defined in, included once.
    pub runtime: Option<Runtime>,
    /// The name scorer code calls, which takes what an agent's call takes.
    ///
    /// A scorer file has no runtime module, so the helper needs a runtime to bring
    /// its definition along.
    pub scorer: Option<&'static str>,
}

/// The text several helpers share, which generated code carries once.
///
/// Two helpers share a runtime when they name the same one.
#[derive(Clone, Copy, Debug)]
pub struct Runtime {
    /// The name the runtime is known by, unique among the providers in use.
    pub name: &'static str,
    /// The text for one language.
    ///
    /// The flag says whether the text is for an agent's runtime module. There it
    /// also defines the helpers agents call, over the agent runtime's own
    /// functions. A scorer file gets the text that stands alone.
    pub source: fn(ScorerLang, bool) -> String,
}

impl PartialEq for Runtime {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Runtime {}

impl PartialOrd for Runtime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Runtime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.cmp(other.name)
    }
}
