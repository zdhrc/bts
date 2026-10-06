mod python;
mod typescript;

use crate::dsl::{Number, ScorerArg, ScorerLang, Value, When};
use std::fmt;

// expression-level emitters know nothing about scorers; only the scaffold
// functions (python::scorer, typescript::scorer) are component-specific. if a
// future component kind needs statements beyond guard/return, introduce a
// small statement ir here between dsl::Value and the emitters; expressions
// should keep flowing through Value.

// language-neutral runtime helpers a generated scorer may need; collected
// during emission so only used snippets appear, in declaration order (normal
// first, its dependents after)
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub(super) enum Helper {
    Normal,
    Gamma,
    Lognormal,
    Exponential,
    Pareto,
    Beta,
    Poisson,
    Choice,
    Weighted,
    RandInt,
    Hex,
    Alphanum,
    Round,
}

// one code scorer's render inputs; several items pack into one module
pub(crate) struct Item<'m> {
    pub(crate) name: &'m str,
    pub(crate) slug: &'m str,
    pub(crate) score: &'m Value,
    pub(crate) whens: &'m [When],
}

// Render named scorer functions with their shared imports and helpers.
pub(crate) fn module(lang: ScorerLang, items: &[Item]) -> Result<String, Error> {
    match lang {
        ScorerLang::Python => python::module(items),
        ScorerLang::Typescript => typescript::module(items),
    }
}

// json-style escaping; both languages read the result inside double quotes
pub(super) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for part in text.chars() {
        match part {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            part => escaped.push(part),
        }
    }
    escaped
}

// {:?} keeps a trailing .0 on whole floats, valid in both languages
pub(super) fn num(number: &Number) -> String {
    match number {
        Number::Int(value) => value.to_string(),
        Number::Float(value) => format!("{value:?}"),
    }
}

pub(super) fn float(value: f64) -> String {
    format!("{value:?}")
}

pub(super) fn arg_name(arg: ScorerArg) -> &'static str {
    match arg {
        ScorerArg::Input => "input",
        ScorerArg::Output => "output",
        ScorerArg::Expected => "expected",
        ScorerArg::Metadata => "metadata",
    }
}

// Slugs contain only lowercase ASCII letters, digits, and hyphens. This
// mapping stays unique while producing an identifier safe in both languages.
pub(super) fn fn_name(slug: &str) -> String {
    format!("scorer_{}", slug.replace('-', "_"))
}

#[derive(Debug)]
pub(crate) enum Error {
    // the modeler rejects everything untranspilable, so this only fires on a
    // contract break between the dsl and scg
    Unsupported { what: &'static str },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { what } => {
                write!(
                    formatter,
                    "scorer expression contains {what}, which cannot emit as scorer code"
                )
            }
        }
    }
}

impl std::error::Error for Error {}
