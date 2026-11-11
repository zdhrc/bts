//! Deterministic English realization and conservative text variation.
//!
//! Structured sentences carry meaning explicitly. Rendering options control only
//! wording, and variation uses a caller's RNG or an explicit selection. No
//! function stores conversational state or loads linguistic resources at runtime.
//!
//! Grammar and inflection are English-only. Unicode segmentation preserves names
//! and other authored text without implying support for another language.
//!
//! Structured clauses support present, past, and future tense, simple,
//! progressive, perfect, and perfect progressive aspect, optional modality,
//! questions, negation, and conditions. Prose analysis is conservative and
//! preserves unsupported text. Verb forms come from packaged English rules and
//! vocabulary or explicit caller-supplied forms.

pub mod analysis;
pub mod composition;
pub mod discourse;
pub mod formatting;
pub mod inflection;
pub mod model;
mod negation;
pub mod realization;
pub mod text;
pub mod variation;

pub use analysis::{Analysis, AnalyzedText, analyze};
pub use composition::{Relation, combine};
pub use discourse::{
    Detail, DiscourseContext, Entity, Pronouns, ReferenceForm, ReferenceOptions, ReferenceStyle, refer, remember, unmentioned,
};
pub use formatting::{
    DateOptions, DateStyle, EnglishLocale, HourCycle, NumberOptions, TimeOptions, format_date, format_number, format_time,
};
pub use model::*;
pub use realization::realize;
pub use variation::{
    CasingStyle, PhraseAlternatives, PunctuationStyle, Style, StyleEdit, StyleOptions, StyleResult, TypoKind, TypoOptions,
    Variation, VariationOptions, applicable_variations, apply_variation, style_edits, text_alternatives, vary, vary_sentence,
    vary_sentence_styled, vary_styled,
};

pub(crate) fn runtime_data() -> serde_json::Value {
    let verbs: serde_json::Map<String, serde_json::Value> = inflection::known_verbs()
        .iter()
        .map(|verb| (verb.base.clone(), serde_json::to_value(verb).expect("packaged verb forms")))
        .collect();
    serde_json::json!({"verbs": verbs, "negations": negation::FORMS, "styles": variation::style_data()})
}
