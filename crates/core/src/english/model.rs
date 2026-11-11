//! English grammatical features kept separate from rendered wording.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Person {
    First,
    Second,
    #[default]
    Third,
}

/// Grammatical agreement number. Singular `they` takes plural agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Number {
    #[default]
    Singular,
    Plural,
}

/// An authored subject with explicit agreement features.
///
/// Author proper names and identifiers with their intended casing. Only known
/// grammatical words such as `the` and `they` are capitalized at sentence start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub text: String,
    #[serde(default)]
    pub person: Person,
    #[serde(default)]
    pub number: Number,
}

/// Explicit verb forms avoid guessing irregular inflections.
///
/// All forms are lowercase single words. Put particles and objects in the
/// predicate complement, for example `check` with the complement `in`.
/// The verb `be` receives person and number agreement during realization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Verb {
    pub base: String,
    pub third_person_singular: String,
    pub past: String,
    pub past_participle: String,
    pub present_participle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub verb: Verb,
    #[serde(default)]
    pub complement: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modal {
    Can,
    Could,
    May,
    Might,
    Must,
    Should,
    Will,
    Would,
}

impl Modal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Can => "can",
            Self::Could => "could",
            Self::May => "may",
            Self::Might => "might",
            Self::Must => "must",
            Self::Should => "should",
            Self::Will => "will",
            Self::Would => "would",
        }
    }

    /// An unambiguous contraction supported by this module.
    pub fn contraction(self) -> Option<&'static str> {
        super::negation::contract(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    #[default]
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    #[default]
    Statement,
    Question,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tense {
    #[default]
    Present,
    Past,
    Future,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    #[default]
    Simple,
    Progressive,
    Perfect,
    PerfectProgressive,
}

/// A clause with explicit tense, aspect, and optional modality.
///
/// Modal clauses use present tense and select their modal explicitly, such as
/// `Could` or `Would`. Perfect aspect expresses anteriority with a modal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clause {
    pub subject: Subject,
    #[serde(default)]
    pub modal: Option<Modal>,
    #[serde(default)]
    pub tense: Tense,
    #[serde(default)]
    pub aspect: Aspect,
    #[serde(default)]
    pub polarity: Polarity,
    pub predicate: Predicate,
    #[serde(default)]
    pub form: Form,
}

/// An English condition introduced by `if`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub subject: Subject,
    pub predicate: Predicate,
    #[serde(default)]
    pub polarity: Polarity,
    #[serde(default)]
    pub tense: Tense,
    #[serde(default)]
    pub aspect: Aspect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sentence {
    pub clause: Clause,
    #[serde(default)]
    pub condition: Option<Condition>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NegationStyle {
    #[default]
    Expanded,
    Contracted,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionPosition {
    #[default]
    Before,
    After,
}

/// Rendering choices do not change modality, polarity, or question form.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizeOptions {
    #[serde(default)]
    pub negation: NegationStyle,
    #[serde(default)]
    pub condition_position: ConditionPosition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidField { field: &'static str, reason: &'static str },
    UnsupportedContraction(Modal),
    UnsupportedConstruction(&'static str),
    InvalidVariation(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidField { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::UnsupportedContraction(modal) => write!(formatter, "no supported contraction for {} not", modal.as_str()),
            Self::UnsupportedConstruction(reason) => formatter.write_str(reason),
            Self::InvalidVariation(reason) => write!(formatter, "invalid variation: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

impl<'de> Deserialize<'de> for Verb {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Forms {
            base: String,
            third_person_singular: String,
            past: String,
            past_participle: String,
            present_participle: String,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Known(String),
            Explicit(Forms),
        }
        match Input::deserialize(deserializer)? {
            Input::Known(base) => Self::lookup(&base).ok_or_else(|| {
                serde::de::Error::custom(format!("unknown English verb {base:?}, supply its five explicit forms"))
            }),
            Input::Explicit(forms) => Self::from_forms(
                &forms.base,
                &forms.third_person_singular,
                &forms.past,
                &forms.past_participle,
                &forms.present_participle,
            )
            .map_err(serde::de::Error::custom),
        }
    }
}
