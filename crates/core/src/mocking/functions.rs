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
use std::collections::BTreeMap;

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
    /// Whether the result is plain data, which typed code can use as it is.
    pub plain: bool,
    /// Whether a call is sure not to raise, from what is known of its arguments.
    ///
    /// The arguments are the ones the function was called with, before the operation
    /// and the random source a helper may take. A helper without this may raise on any
    /// call, so a consumer computes its result only where the result is read.
    pub sure: Option<fn(&[Known]) -> bool>,
    /// What a call returns, from what is known of its arguments.
    ///
    /// It says more than `counts`, `texts` and `plain` do, and a consumer prefers it.
    pub returns: Option<fn(&[Known]) -> Known>,
    /// Whether a call draws from the random stream of the run, from what is known of its arguments.
    ///
    /// A call that does gives another result each time it runs, so a consumer computes it once.
    /// A helper without this draws whenever it is passed the random source.
    pub draws: Option<fn(&[Known]) -> bool>,
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

/// What is known of a value before it is computed.
///
/// A helper reads its arguments this way to say whether a call can raise and what
/// it returns. A consumer reads the answer to compute a value early and to type it.
#[derive(Clone, Debug, PartialEq)]
pub enum Known {
    /// Nothing is known.
    Unknown,
    /// Plain data of a shape that is not known.
    Plain,
    /// Null.
    Null,
    /// A boolean.
    Bool,
    /// A string, with what it says when it is written out.
    Text(Option<String>),
    /// A finite number between two bounds, and whether it is whole.
    Number { low: f64, high: f64, whole: bool },
    /// A list written item by item. An item that is a [`Known::Run`] stands for any number of items.
    Items(Vec<Known>),
    /// Any number of items alike, in a list written item by item.
    Run(Box<Known>),
    /// A list of any length whose items are alike.
    List(Box<Known>),
    /// An object that has at least these keys.
    Object(BTreeMap<String, Known>),
    /// A value that may be null.
    Maybe(Box<Known>),
}

impl Known {
    /// A number that may be any finite one.
    pub const NUMBER: Known = Known::Number {
        low: f64::NEG_INFINITY,
        high: f64::INFINITY,
        whole: false,
    };

    /// A whole number between two bounds.
    pub fn whole(low: f64, high: f64) -> Known {
        Known::Number { low, high, whole: true }
    }

    /// Whether the value is plain data all the way down.
    pub fn plain(&self) -> bool {
        match self {
            Known::Unknown => false,
            Known::Items(items) => items.iter().all(Known::plain),
            Known::List(item) | Known::Maybe(item) | Known::Run(item) => item.plain(),
            Known::Object(fields) => fields.values().all(Known::plain),
            _ => true,
        }
    }

    /// Whether the value is a string, or a whole number a float holds exactly.
    ///
    /// These are what a walk may be drawn per.
    pub fn key(&self) -> bool {
        match self {
            Known::Text(_) => true,
            Known::Number { low, high, whole } => *whole && *low >= -SAFE && *high <= SAFE,
            _ => false,
        }
    }

    /// What is known of a value that is one of two.
    pub fn join(self, other: Known) -> Known {
        match (self, other) {
            (Known::Null, Known::Null) => Known::Null,
            (Known::Null, Known::Maybe(inner)) | (Known::Maybe(inner), Known::Null) => Known::Maybe(inner),
            (Known::Null, other) | (other, Known::Null) => Known::Maybe(Box::new(other)),
            (Known::Maybe(a), Known::Maybe(b)) => Known::Maybe(Box::new(a.join(*b))),
            (Known::Maybe(a), b) | (b, Known::Maybe(a)) => Known::Maybe(Box::new(a.join(b))),
            (Known::Bool, Known::Bool) => Known::Bool,
            (Known::Text(a), Known::Text(b)) => Known::Text(if a == b { a } else { None }),
            (
                Known::Number { low, high, whole },
                Known::Number {
                    low: other_low,
                    high: other_high,
                    whole: other_whole,
                },
            ) => Known::Number {
                low: low.min(other_low),
                high: high.max(other_high),
                whole: whole && other_whole,
            },
            (Known::Items(a), Known::Items(b))
                if a.len() == b.len() && !a.iter().chain(&b).any(|item| matches!(item, Known::Run(_))) =>
            {
                Known::Items(a.into_iter().zip(b).map(|(a, b)| a.join(b)).collect())
            }
            (a @ (Known::Items(_) | Known::List(_)), b @ (Known::Items(_) | Known::List(_))) => {
                let one = |item: Known| match item {
                    Known::Run(item) => *item,
                    item => item,
                };
                let item = |list: Known| match list {
                    Known::Items(items) => items.into_iter().map(one).reduce(Known::join),
                    Known::List(item) => Some(*item),
                    _ => None,
                };
                match (item(a), item(b)) {
                    (Some(a), Some(b)) => Known::List(Box::new(a.join(b))),
                    (Some(item), None) | (None, Some(item)) => Known::List(Box::new(item)),
                    (None, None) => Known::Items(vec![]),
                }
            }
            (Known::Object(a), Known::Object(mut b)) => Known::Object(
                a.into_iter()
                    .filter_map(|(key, value)| b.remove(&key).map(|other| (key, value.join(other))))
                    .collect(),
            ),
            (a, b) if a.plain() && b.plain() => Known::Plain,
            _ => Known::Unknown,
        }
    }
}

// the largest whole number a float holds exactly
const SAFE: f64 = 9_007_199_254_740_991.0;

impl Helper {
    /// The helper with what it says of its calls: when one cannot raise, and what one returns.
    pub const fn known(mut self, sure: fn(&[Known]) -> bool, returns: fn(&[Known]) -> Known) -> Self {
        self.sure = Some(sure);
        self.returns = Some(returns);
        self
    }

    /// The helper with when a call draws from the random stream of the run.
    pub const fn drawing(mut self, draws: fn(&[Known]) -> bool) -> Self {
        self.draws = Some(draws);
        self
    }
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
