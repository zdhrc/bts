//! Conservative wording, explicit tone choices and bounded typing variation.

use std::ops::Range;

use rand::Rng;
use unicode_segmentation::UnicodeSegmentation;

use super::{Analysis, ConditionPosition, Error, Form, NegationStyle, Polarity, RealizeOptions, Sentence, analyze, realize};

/// Enables independent transformations. Disabled controls leave wording alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VariationOptions {
    pub negation: bool,
    pub condition_order: bool,
}

/// A wording change, never a change to modality, polarity, or question form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variation {
    Negation(NegationStyle),
    ConditionPosition(ConditionPosition),
}

/// Lists supported changes in stable order, excluding the current wording.
///
/// Negative questions are excluded from contraction variation because their
/// expanded and contracted forms can imply different speaker expectations.
pub fn applicable_variations(
    sentence: &Sentence,
    rendering: &RealizeOptions,
    options: &VariationOptions,
) -> Result<Vec<Variation>, Error> {
    realize(sentence, rendering)?;
    let mut variations = Vec::with_capacity(2);
    let negative = sentence.clause.polarity == Polarity::Negative
        || sentence
            .condition
            .as_ref()
            .is_some_and(|condition| condition.polarity == Polarity::Negative);
    let negative_question = sentence.clause.form == Form::Question && sentence.clause.polarity == Polarity::Negative;
    if options.negation && negative && !negative_question {
        let negation = match rendering.negation {
            NegationStyle::Expanded => NegationStyle::Contracted,
            NegationStyle::Contracted => NegationStyle::Expanded,
        };
        let candidate = RealizeOptions { negation, ..*rendering };
        match realize(sentence, &candidate) {
            Ok(_) => variations.push(Variation::Negation(negation)),
            Err(Error::UnsupportedContraction(_) | Error::UnsupportedConstruction(_)) => {}
            Err(error) => return Err(error),
        }
    }
    if options.condition_order && sentence.condition.is_some() {
        variations.push(Variation::ConditionPosition(match rendering.condition_position {
            ConditionPosition::Before => ConditionPosition::After,
            ConditionPosition::After => ConditionPosition::Before,
        }));
    }
    Ok(variations)
}

/// Applies an explicit supported selection, returning new rendering options.
///
/// The semantic structure is borrowed and cannot be changed by a variation.
pub fn apply_variation(sentence: &Sentence, rendering: &RealizeOptions, selection: Variation) -> Result<RealizeOptions, Error> {
    let options = VariationOptions {
        negation: true,
        condition_order: true,
    };
    if !applicable_variations(sentence, rendering, &options)?.contains(&selection) {
        return Err(Error::InvalidVariation("selection is not applicable to this sentence"));
    }
    let mut changed = *rendering;
    match selection {
        Variation::Negation(style) => changed.negation = style,
        Variation::ConditionPosition(position) => changed.condition_position = position,
    }
    Ok(changed)
}

/// Chooses uniformly between the original wording and each applicable change.
///
/// At most one change is applied. No randomness is consumed when none applies.
pub fn vary_sentence<R: Rng + ?Sized>(
    sentence: &Sentence,
    rendering: &RealizeOptions,
    options: &VariationOptions,
    rng: &mut R,
) -> Result<String, Error> {
    let variations = applicable_variations(sentence, rendering, options)?;
    let selected = if variations.is_empty() {
        0
    } else {
        rng.random_range(0..=variations.len() as u64) as usize
    };
    let mut changed = *rendering;
    if selected > 0 {
        match variations[selected - 1] {
            Variation::Negation(style) => changed.negation = style,
            Variation::ConditionPosition(position) => changed.condition_position = position,
        }
    }
    realize(sentence, &changed)
}

/// Varies supported prose without reformatting any unchanged source bytes.
///
/// Supports consistent negation register across recognized sentences and ordering
/// of recognized conditions. Unsupported prose and disabled controls preserve
/// the original. The original and each alternative have equal probability.
pub fn vary<R: Rng + ?Sized>(text: &str, options: &VariationOptions, rng: &mut R) -> Result<String, Error> {
    let alternatives = text_alternatives(text, options);
    if alternatives.is_empty() {
        return Ok(text.to_owned());
    }
    let index = rng.random_range(0..=alternatives.len() as u64) as usize;
    Ok(if index == 0 {
        text.to_owned()
    } else {
        alternatives[index - 1].clone()
    })
}

struct Edit {
    range: Range<usize>,
    replacement: &'static str,
    contracts: bool,
}

/// Lists conservative prose alternatives without selecting one.
///
/// Negation changes use a consistent register across all supported clauses.
/// Conditional ordering preserves the authored references. Any unsupported
/// sentence leaves the whole input untouched, represented by an empty list.
pub fn text_alternatives(text: &str, options: &VariationOptions) -> Vec<String> {
    if !options.negation && !options.condition_order {
        return Vec::new();
    }
    let mut edits = Vec::new();
    let mut reordered = None;
    if let Some(conditional) = Conditional::parse(text) {
        if !clause(conditional.condition, '.', conditional.condition_offset, &mut edits)
            || !clause(conditional.main, conditional.punctuation, conditional.main_offset, &mut edits)
        {
            return Vec::new();
        }
        if options.condition_order {
            reordered = Some(conditional.reorder());
        }
    } else {
        let mut offset = 0;
        let mut count = 0;
        for segment in text.split_sentence_bounds() {
            if !segment.trim().is_empty() {
                let Analysis::Supported(analysis) = analyze(segment) else {
                    return Vec::new();
                };
                if let Some((range, replacement, contracts)) = analysis.contraction() {
                    edits.push(Edit {
                        range: offset + range.start..offset + range.end,
                        replacement,
                        contracts,
                    });
                }
                count += 1;
            }
            offset += segment.len();
        }
        if count == 0 {
            return Vec::new();
        }
    }
    edits.sort_by_key(|edit| edit.range.start);
    let mut alternatives = Vec::with_capacity(3);
    if options.negation {
        for contracts in [false, true] {
            let mut changed = text.to_owned();
            for edit in edits.iter().rev().filter(|edit| edit.contracts == contracts) {
                changed.replace_range(edit.range.clone(), edit.replacement);
            }
            if changed != text && !alternatives.contains(&changed) {
                alternatives.push(changed);
            }
        }
    }
    if let Some(reordered) = reordered.filter(|changed| changed != text && !alternatives.contains(changed)) {
        alternatives.push(reordered);
    }
    alternatives
}

fn clause(body: &str, punctuation: char, offset: usize, edits: &mut Vec<Edit>) -> bool {
    let input = format!("{body}{punctuation}");
    let Analysis::Supported(analysis) = analyze(&input) else {
        return false;
    };
    if let Some((range, replacement, contracts)) = analysis.contraction() {
        edits.push(Edit {
            range: offset + range.start..offset + range.end,
            replacement,
            contracts,
        });
    }
    true
}

struct Conditional<'a> {
    source: &'a str,
    condition: &'a str,
    main: &'a str,
    condition_offset: usize,
    main_offset: usize,
    leading: usize,
    trailing: usize,
    punctuation: char,
    before: bool,
}

impl<'a> Conditional<'a> {
    fn parse(source: &'a str) -> Option<Self> {
        let trimmed = source.trim();
        let leading = source.len() - source.trim_start().len();
        let trailing = source.trim_end().len();
        let punctuation = trimmed.chars().last()?;
        if !matches!(punctuation, '.' | '?') || trimmed.unicode_sentences().count() != 1 {
            return None;
        }
        let body = &trimmed[..trimmed.len() - 1];
        if body.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
            return None;
        }
        let (condition, main, condition_offset, main_offset, before) =
            if let Some(rest) = body.strip_prefix("If ").or_else(|| body.strip_prefix("if ")) {
                let (condition, main) = rest.split_once(", ")?;
                if condition.is_empty() || main.is_empty() || main.contains(',') {
                    return None;
                }
                (condition, main, leading + 3, leading + 3 + condition.len() + 2, true)
            } else {
                let (main, condition) = body.split_once(" if ")?;
                if main.is_empty() || condition.is_empty() || main.contains(',') || condition.contains(',') {
                    return None;
                }
                (condition, main, leading + main.len() + 4, leading, false)
            };
        Some(Self {
            source,
            condition,
            main,
            condition_offset,
            main_offset,
            leading,
            trailing,
            punctuation,
            before,
        })
    }

    fn reorder(&self) -> String {
        let main = initial_case(self.main, self.punctuation == '?', self.before);
        let changed = if self.before {
            format!("{main} if {}{}", self.condition, self.punctuation)
        } else {
            format!("If {}, {main}{}", self.condition, self.punctuation)
        };
        format!("{}{changed}{}", &self.source[..self.leading], &self.source[self.trailing..])
    }
}

fn initial_case(text: &str, question: bool, upper: bool) -> String {
    let end = text.find(' ').unwrap_or(text.len());
    let first = &text[..end];
    let lower = first.to_ascii_lowercase();
    let pronoun = matches!(lower.as_str(), "you" | "we" | "they" | "he" | "she" | "it");
    let determiner = matches!(
        lower.as_str(),
        "the" | "a" | "an" | "this" | "that" | "these" | "those" | "my" | "your" | "our" | "their" | "his" | "her" | "its"
    ) && text
        .get(end + 1..)
        .and_then(|tail| tail.chars().next())
        .is_some_and(char::is_lowercase);
    let auxiliary = question
        && matches!(
            lower.as_str(),
            "can"
                | "could"
                | "may"
                | "might"
                | "must"
                | "should"
                | "will"
                | "would"
                | "do"
                | "does"
                | "did"
                | "am"
                | "is"
                | "are"
                | "was"
                | "were"
                | "have"
                | "has"
                | "had"
        );
    if !(pronoun || determiner || auxiliary) || !first.is_ascii() || first.is_empty() {
        return text.to_owned();
    }
    let mut changed = text.to_owned();
    if upper {
        changed[..1].make_ascii_uppercase();
    } else {
        changed[..1].make_ascii_lowercase();
    }
    changed
}

/// Named, inspectable presets. Explicit controls always override a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Neutral,
    Professional,
    Conversational,
    Warm,
    Terse,
    TextMessage,
    Enthusiastic,
    InformalChat,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PunctuationStyle {
    #[default]
    Preserve,
    OmitPeriod,
    Exclamation,
    Expressive,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CasingStyle {
    #[default]
    Preserve,
    Lowercase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypoKind {
    Common,
    Omission,
    Transposition,
    Repetition,
}

/// Probability is per message, independent of the wording probability.
/// Only reviewed everyday words are eligible. Errors share the change budget.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypoOptions {
    pub probability: f64,
    #[serde(deserialize_with = "count_option")]
    pub max_errors: u8,
    pub kinds: Vec<TypoKind>,
}

impl Default for TypoOptions {
    fn default() -> Self {
        Self {
            probability: 0.0,
            max_errors: 1,
            kinds: vec![
                TypoKind::Common,
                TypoKind::Omission,
                TypoKind::Transposition,
                TypoKind::Repetition,
            ],
        }
    }
}

/// Author-approved phrase alternatives can explicitly authorize semantic changes.
/// Matching is literal and case-sensitive, with whole-word boundaries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhraseAlternatives {
    pub from: String,
    pub to: Vec<String>,
}

/// Opt-in variation controls. These do not change the legacy VariationOptions API.
///
/// Probability gates wording, tone and presentation once per message. After that
/// gate, eligible nonoverlapping edits are selected up to max_changes. A preset
/// supplies defaults, then explicit fields override them. Typos are always off
/// unless enabled separately. Unknown names are not inferred from arbitrary prose.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StyleOptions {
    pub style: Option<Style>,
    pub negation: Option<bool>,
    pub condition_order: Option<bool>,
    pub contractions: Option<bool>,
    pub shorthand: Option<bool>,
    pub casual: Option<bool>,
    pub slang: Option<bool>,
    pub reductions: Option<bool>,
    pub fragments: Option<bool>,
    pub warmth: Option<bool>,
    pub soften_requests: Option<bool>,
    pub punctuation: Option<PunctuationStyle>,
    pub casing: Option<CasingStyle>,
    pub probability: f64,
    #[serde(deserialize_with = "count_option")]
    pub max_changes: u8,
    pub typos: TypoOptions,
    pub protected: Vec<String>,
    pub alternatives: Vec<PhraseAlternatives>,
    pub rendering: RealizeOptions,
}

impl Default for StyleOptions {
    fn default() -> Self {
        Self {
            style: None,
            negation: None,
            condition_order: None,
            contractions: None,
            shorthand: None,
            casual: None,
            slang: None,
            reductions: None,
            fragments: None,
            warmth: None,
            soften_requests: None,
            punctuation: None,
            casing: None,
            probability: 1.0,
            max_changes: 3,
            typos: TypoOptions::default(),
            protected: vec![],
            alternatives: vec![],
            rendering: RealizeOptions::default(),
        }
    }
}

fn count_option<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    if !(0.0..=16.0).contains(&value) || value.fract() != 0.0 {
        return Err(serde::de::Error::custom("expected an integer from 0 through 16"));
    }
    Ok(value as u8)
}

impl StyleOptions {
    /// Resolves the preset and explicit overrides into a reusable configuration.
    pub fn resolved(&self) -> Result<Self, Error> {
        let probability = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
        if !probability(self.probability)
            || !probability(self.typos.probability)
            || self.max_changes > 16
            || self.typos.max_errors > 16
        {
            return Err(Error::InvalidVariation(
                "probabilities must be in 0..=1 and budgets in 0..=16",
            ));
        }
        if self.protected.iter().any(|s| s.is_empty())
            || self
                .alternatives
                .iter()
                .any(|a| a.from.trim().is_empty() || a.to.is_empty() || a.to.iter().any(|s| s.trim().is_empty()))
        {
            return Err(Error::InvalidVariation(
                "protected terms and phrase alternatives must not be empty",
            ));
        }
        let mut result = self.clone();
        let conversational = matches!(
            self.style,
            Some(
                Style::Conversational
                    | Style::Warm
                    | Style::Terse
                    | Style::TextMessage
                    | Style::Enthusiastic
                    | Style::InformalChat
            )
        );
        result.negation = Some(self.negation.unwrap_or(self.style.is_none() || conversational));
        result.condition_order = Some(self.condition_order.unwrap_or(self.style.is_none()));
        result.contractions = Some(self.contractions.unwrap_or(conversational));
        result.casual = Some(self.casual.unwrap_or(conversational));
        result.shorthand = Some(self.shorthand.unwrap_or(self.style == Some(Style::TextMessage)));
        result.slang = Some(self.slang.unwrap_or(self.style == Some(Style::InformalChat)));
        result.reductions = Some(
            self.reductions
                .unwrap_or(matches!(self.style, Some(Style::TextMessage | Style::InformalChat))),
        );
        result.fragments = Some(self.fragments.unwrap_or(matches!(
            self.style,
            Some(Style::Terse | Style::TextMessage | Style::InformalChat)
        )));
        result.warmth = Some(
            self.warmth
                .unwrap_or(matches!(self.style, Some(Style::Warm | Style::Enthusiastic))),
        );
        result.soften_requests = Some(self.soften_requests.unwrap_or(false));
        result.punctuation = Some(self.punctuation.unwrap_or(match self.style {
            Some(Style::TextMessage | Style::Terse) => PunctuationStyle::OmitPeriod,
            Some(Style::Enthusiastic) => PunctuationStyle::Exclamation,
            _ => PunctuationStyle::Preserve,
        }));
        result.casing = Some(self.casing.unwrap_or(if self.style == Some(Style::TextMessage) {
            CasingStyle::Lowercase
        } else {
            CasingStyle::Preserve
        }));
        Ok(result)
    }

    fn enabled(&self, category: &str) -> bool {
        (match category {
            "contractions" => self.contractions,
            "shorthand" => self.shorthand,
            "casual" => self.casual,
            "slang" => self.slang,
            "reductions" => self.reductions,
            "fragments" => self.fragments,
            "warmth" => self.warmth,
            "soften_requests" => self.soften_requests,
            _ => Some(false),
        }) == Some(true)
    }
}

/// A candidate edit. Ranges are UTF-8 byte offsets into the inspected input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StyleEdit {
    pub rule: String,
    pub range: Range<usize>,
    pub replacement: String,
    pub stage: u8,
}

/// Applied edit ranges refer to the text immediately before that edit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StyleResult {
    pub text: String,
    pub edits: Vec<StyleEdit>,
}

#[derive(serde::Serialize)]
struct StyleRule {
    id: String,
    category: &'static str,
    from: String,
    to: String,
    guard: String,
    stage: u8,
}

fn style_rules() -> &'static [StyleRule] {
    static RULES: std::sync::OnceLock<Vec<StyleRule>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = Vec::new();
        let mut add = |category, from: &str, to: &str, guard: &str, stage| {
            rules.push(StyleRule {
                id: format!("{category}:{}", from.to_ascii_lowercase().replace(' ', "_")),
                category,
                from: from.into(),
                to: to.into(),
                guard: guard.into(),
                stage,
            });
        };
        for (subject, auxiliary, ending) in [
            ("I", "am", "'m"),
            ("you", "are", "'re"),
            ("we", "are", "'re"),
            ("they", "are", "'re"),
            ("he", "is", "'s"),
            ("she", "is", "'s"),
            ("it", "is", "'s"),
        ] {
            add(
                "contractions",
                &format!("{subject} {auxiliary}"),
                &format!("{subject}{ending}"),
                &format!("aux:{auxiliary}"),
                0,
            );
        }
        for subject in ["I", "you", "we", "they", "he", "she", "it"] {
            for (auxiliary, ending) in [
                ("will", "'ll"),
                ("would", "'d"),
                ("had", "'d"),
                (
                    if matches!(subject, "he" | "she" | "it") {
                        "has"
                    } else {
                        "have"
                    },
                    if matches!(subject, "he" | "she" | "it") { "'s" } else { "'ve" },
                ),
            ] {
                add(
                    "contractions",
                    &format!("{subject} {auxiliary}"),
                    &format!("{subject}{ending}"),
                    &format!("aux:{auxiliary}"),
                    0,
                );
            }
        }
        for (subject, be) in [
            ("I", "am"),
            ("you", "are"),
            ("we", "are"),
            ("they", "are"),
            ("he", "is"),
            ("she", "is"),
            ("it", "is"),
        ] {
            add(
                "reductions",
                &format!("{subject} {be} going to"),
                &format!("{subject} {be} gonna"),
                "base",
                0,
            );
            if matches!(subject, "I" | "you" | "we" | "they") {
                add(
                    "reductions",
                    &format!("{subject} want to"),
                    &format!("{subject} wanna"),
                    "base",
                    0,
                );
            }
        }
        add("fragments", "I am ", "", "progressive", 0);
        add("fragments", "I'm ", "", "progressive", 0);
        for (from, to) in [
            ("by the way", "btw"),
            ("as soon as possible", "ASAP"),
            ("for your information", "FYI"),
            ("in my opinion", "IMO"),
            ("to be honest", "tbh"),
            ("let me know", "lmk"),
            ("talk to you later", "ttyl"),
            ("be right back", "brb"),
        ] {
            add(
                "shorthand",
                from,
                to,
                if from == "as soon as possible" { "phrase" } else { "initial" },
                1,
            );
        }
        for (from, to, guard) in [
            ("thank you", "thanks", "initial"),
            ("hello", "hi", "opening"),
            ("goodbye", "bye", "full"),
            ("yes", "yeah", "full"),
            ("no", "nope", "full"),
            ("understood", "got it", "full"),
            ("certainly", "sure", "full"),
            ("approximately", "about", "phrase"),
            ("a little bit", "a bit", "phrase"),
        ] {
            add("casual", from, to, guard, 1);
        }
        for (from, to) in [
            ("yes", "yep"),
            ("okay", "ok"),
            ("see you", "see ya"),
            ("that is cool", "that's cool"),
            ("see you later", "catch you later"),
        ] {
            add("slang", from, to, "full", 1);
        }
        for (from, to) in [
            ("thank you", "thank you so much"),
            ("thanks", "thanks so much"),
            ("hello", "hello there"),
            ("hi", "hi there"),
        ] {
            add("warmth", from, to, "full", 2);
        }
        for verb in ["send", "check", "share", "confirm", "provide", "review", "update"] {
            add("soften_requests", verb, &format!("could you {verb}"), "request", 2);
            add(
                "soften_requests",
                &format!("please {verb}"),
                &format!("could you please {verb}"),
                "request",
                2,
            );
        }
        rules
    })
}

// This small vocabulary bounds casing and typing changes without guessing names.
const STYLE_WORDS: &[&str] = &[
    "i",
    "you",
    "we",
    "they",
    "he",
    "she",
    "it",
    "the",
    "this",
    "that",
    "a",
    "an",
    "hello",
    "hi",
    "thanks",
    "thank",
    "please",
    "sorry",
    "checking",
    "waiting",
    "sending",
    "looking",
    "received",
    "message",
    "tomorrow",
    "because",
    "really",
    "about",
    "available",
    "definitely",
    "separate",
    "necessary",
    "receive",
    "address",
    "appointment",
    "information",
    "understood",
    "yes",
    "no",
    "okay",
    "sure",
    "by",
    "as",
    "for",
    "in",
    "to",
    "let",
    "talk",
    "be",
    "got",
    "could",
    "send",
    "check",
    "share",
    "confirm",
    "provide",
    "review",
    "update",
];
const STYLE_CASE_WORDS: &[&str] = &[
    "how", "what", "would", "can", "is", "do", "am", "are", "have", "want", "need", "my",
];
const STYLE_CONTRACTIONS: &[&str] = &[
    "I'm", "I'll", "I've", "I'd", "You're", "You've", "You'll", "You'd", "We're", "We've", "We'll", "We'd", "They're",
    "They've", "They'll", "They'd", "He's", "He'll", "He'd", "She's", "She'll", "She'd", "It's", "It'll", "It'd",
];
const STYLE_ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "etc", "approx", "vs", "inc", "ltd", "jr", "sr", "st", "mt", "ave", "rd", "dept", "fig",
];
const COMMON_TYPOS: &[(&str, &str)] = &[
    ("definitely", "definately"),
    ("separate", "seperate"),
    ("necessary", "neccessary"),
    ("receive", "recieve"),
    ("address", "adress"),
    ("tomorrow", "tommorow"),
    ("because", "becuase"),
    ("thanks", "thnaks"),
    ("please", "plese"),
];

pub(crate) fn style_data() -> serde_json::Value {
    let mut presets = serde_json::Map::new();
    for style in [
        Style::Neutral,
        Style::Professional,
        Style::Conversational,
        Style::Warm,
        Style::Terse,
        Style::TextMessage,
        Style::Enthusiastic,
        Style::InformalChat,
    ] {
        let name = serde_json::to_value(style).unwrap().as_str().unwrap().to_owned();
        presets.insert(
            name,
            serde_json::to_value(
                StyleOptions {
                    style: Some(style),
                    ..StyleOptions::default()
                }
                .resolved()
                .unwrap(),
            )
            .unwrap(),
        );
    }
    serde_json::json!({"rules":style_rules(), "words":STYLE_WORDS, "case_words":STYLE_CASE_WORDS, "typos":COMMON_TYPOS, "contractions":STYLE_CONTRACTIONS, "abbreviations":STYLE_ABBREVIATIONS, "presets":presets, "defaults": StyleOptions::default().resolved().unwrap()})
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn style_case(source: &str, replacement: &str) -> String {
    if source.starts_with(char::is_uppercase) && !replacement.is_empty() {
        let mut text = replacement.to_owned();
        if let Some(first) = text.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        text
    } else {
        replacement.into()
    }
}

fn phrase_boundary(text: &str, range: &Range<usize>) -> bool {
    let word = |c: char| !c.is_ascii() || c.is_alphanumeric() || matches!(c, '_' | '\'' | '’' | '-');
    !text[..range.start].chars().next_back().is_some_and(word) && !text[range.end..].chars().next().is_some_and(word)
}

fn sentence_start(text: &str, start: usize) -> bool {
    text[..start]
        .trim_end()
        .chars()
        .next_back()
        .is_none_or(|c| matches!(c, '.' | '!' | '?' | '\n'))
}

fn style_candidates(text: &str, options: &StyleOptions) -> Vec<StyleEdit> {
    let protected = super::text::protected_ranges(text, &options.protected);
    let mut edits = Vec::new();
    let mut push = |rule: String, range: Range<usize>, replacement: String, stage| {
        if text[range.clone()] != replacement && !protected.iter().any(|p| overlaps(p, &range)) {
            edits.push(StyleEdit {
                rule,
                range,
                replacement,
                stage,
            });
        }
    };
    for rule in style_rules().iter().filter(|rule| options.enabled(rule.category)) {
        // ASCII case folding preserves offsets, including surrounding Unicode.
        let lower = text.to_ascii_lowercase();
        for (start, _) in lower.match_indices(&rule.from.to_ascii_lowercase()) {
            let mut range = start..start + rule.from.len();
            let source = &text[range.clone()];
            if source != rule.from && source != style_case("A", &rule.from) && source != rule.from.to_ascii_lowercase() {
                continue;
            }
            if !rule.from.ends_with(' ') && !phrase_boundary(text, &range) {
                continue;
            }
            if rule.from.ends_with(' ') && !sentence_start(text, start) {
                continue;
            }
            let tail = &text[range.end..];
            let end = tail.find(['.', '?', '!', '\n']).unwrap_or(tail.len());
            let body = &tail[..end];
            let words: Vec<_> = body.split_whitespace().collect();
            let full = tail.trim().is_empty() || tail.starts_with(['.', '!', '?']);
            let initial = sentence_start(text, start);
            let guard = match rule.guard.as_str() {
                "phrase" => true,
                "initial" => initial,
                "opening" => initial && (full || tail.starts_with(',')),
                "full" => initial && full,
                "base" => {
                    initial
                        && super::analysis::style_predicate("will", body)
                        && !words.iter().any(|w| ["if", "whether"].contains(w))
                        && (!rule.from.ends_with("going to")
                            || !words.first().is_some_and(|w| {
                                ["work", "school", "college", "church", "bed", "sleep", "shop", "practice"].contains(w)
                            }))
                }
                "progressive" => {
                    initial
                        && super::analysis::style_predicate("am", body)
                        && tail.as_bytes().get(end) != Some(&b'?')
                        && tail.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                        && super::inflection::known_verbs()
                            .iter()
                            .any(|v| words.first() == Some(&v.present_participle.as_str()))
                        && !words.iter().any(|w| ["not", "if", "and", "but", "or", "because"].contains(w))
                }
                "request" => {
                    initial
                        && !words.is_empty()
                        && tail.as_bytes().get(end) == Some(&b'.')
                        && !body.contains([',', ';', ':'])
                        && !words.iter().any(|w| ["if", "and", "or", "not"].contains(w))
                }
                aux if aux.starts_with("aux:") => initial && super::analysis::style_predicate(&aux[4..], body),
                _ => false,
            };
            if !guard {
                continue;
            }
            let mut replacement = style_case(source, &rule.to);
            if rule.guard == "progressive" {
                // Include the first letter so capitalization is part of the same edit.
                range.end += 1;
                replacement = text[range.end - 1..range.end].to_ascii_uppercase();
            }
            if rule.guard == "request" {
                replacement.push_str(body);
                replacement.push('?');
                range.end += end + 1;
            }
            push(rule.id.clone(), range, replacement, rule.stage);
        }
    }
    if options.negation == Some(true) {
        let mut start = 0;
        for sentence in text.split_inclusive(['.', '?', '!', '\n']) {
            if let Analysis::Supported(analysis) = analyze(sentence)
                && let Some((range, replacement, true)) = analysis.contraction()
            {
                push(
                    "negation:contract".into(),
                    start + range.start..start + range.end,
                    replacement.into(),
                    0,
                );
            }
            start += sentence.len();
        }
    }
    if options.condition_order == Some(true) {
        for alternative in text_alternatives(
            text,
            &VariationOptions {
                negation: false,
                condition_order: true,
            },
        ) {
            push("condition:order".into(), 0..text.len(), alternative, 0);
        }
    }
    for (index, family) in options.alternatives.iter().enumerate() {
        for (start, _) in text.match_indices(&family.from) {
            let range = start..start + family.from.len();
            if phrase_boundary(text, &range) {
                for (choice, replacement) in family.to.iter().enumerate() {
                    push(format!("authored:{index}:{choice}"), range.clone(), replacement.clone(), 1);
                }
            }
        }
    }
    let end = text.trim_end().len();
    if end > 0 && text.as_bytes()[end - 1].is_ascii() {
        let mark = &text[end - 1..end];
        let before = &text[..end - 1];
        let last = before.split_whitespace().next_back().unwrap_or("");
        // Do not reinterpret ellipses, abbreviations, decimals or identifiers.
        let terminal = mark == "."
            && !last.is_empty()
            && !last.contains('.')
            && !STYLE_ABBREVIATIONS.contains(&last.to_ascii_lowercase().as_str());
        match options.punctuation.unwrap_or_default() {
            PunctuationStyle::OmitPeriod if terminal => push("punctuation:omit_period".into(), end - 1..end, "".into(), 3),
            PunctuationStyle::Exclamation if terminal => push("punctuation:exclamation".into(), end - 1..end, "!".into(), 3),
            PunctuationStyle::Expressive if terminal || mark == "!" && !before.ends_with('!') => {
                push("punctuation:expressive".into(), end - 1..end, "!!".into(), 3)
            }
            _ => {}
        }
    }
    if options.casing == Some(CasingStyle::Lowercase) {
        for &pronoun in STYLE_CONTRACTIONS {
            for (start, _) in text.match_indices(pronoun) {
                let range = start..start + pronoun.len();
                if phrase_boundary(text, &range) {
                    push("casing:lowercase".into(), range, pronoun.to_ascii_lowercase(), 3);
                }
            }
        }
    }
    for (start, word) in text.split_word_bound_indices() {
        let lower = word.to_ascii_lowercase();
        if !(STYLE_WORDS.contains(&lower.as_str()) || STYLE_CASE_WORDS.contains(&lower.as_str()))
            || !word.is_ascii()
            || !phrase_boundary(text, &(start..start + word.len()))
        {
            continue;
        }
        if options.casing == Some(CasingStyle::Lowercase) {
            push("casing:lowercase".into(), start..start + word.len(), lower.clone(), 3);
        }
        if !STYLE_WORDS.contains(&lower.as_str())
            || word == "could"
            || word.len() < 5
            || word != lower
            || options.typos.probability == 0.0
        {
            continue;
        }
        for kind in &options.typos.kinds {
            let replacement = match kind {
                TypoKind::Common => COMMON_TYPOS.iter().find(|(w, _)| *w == word).map(|(_, to)| to.to_string()),
                TypoKind::Omission => Some(format!("{}{}", &word[..2], &word[3..])),
                TypoKind::Transposition if word.as_bytes()[1] != word.as_bytes()[2] => {
                    Some(format!("{}{}{}{}", &word[..1], &word[2..3], &word[1..2], &word[3..]))
                }
                TypoKind::Repetition => Some(format!("{}{}{}", &word[..2], &word[1..2], &word[2..])),
                _ => None,
            };
            if let Some(replacement) = replacement {
                // A typo must not silently become another packaged English word.
                if !STYLE_WORDS.contains(&replacement.as_str())
                    && !super::inflection::known_verbs().iter().any(|v| {
                        [
                            &v.base,
                            &v.past,
                            &v.third_person_singular,
                            &v.past_participle,
                            &v.present_participle,
                        ]
                        .contains(&&replacement)
                    })
                {
                    push(
                        format!("typo:{}", serde_json::to_value(kind).unwrap().as_str().unwrap()),
                        start..start + word.len(),
                        replacement,
                        4,
                    );
                }
            }
        }
    }
    edits.sort_by(|a, b| {
        (a.stage, a.range.start, &a.rule, &a.replacement).cmp(&(b.stage, b.range.start, &b.rule, &b.replacement))
    });
    edits.dedup();
    edits
}

/// Inspects currently eligible edits without consuming randomness or expanding combinations.
pub fn style_edits(text: &str, options: &StyleOptions) -> Result<Vec<StyleEdit>, Error> {
    Ok(style_candidates(text, &options.resolved()?))
}

/// Applies bounded style edits and returns an explanation of every applied change.
/// No draw is consumed for disabled gates, certain gates or a sole candidate.
pub fn vary_styled<R: Rng + ?Sized>(text: &str, options: &StyleOptions, rng: &mut R) -> Result<StyleResult, Error> {
    vary_styled_with(text, options, || rng.random::<f64>())
}

fn vary_styled_with(text: &str, options: &StyleOptions, mut random: impl FnMut() -> f64) -> Result<StyleResult, Error> {
    let options = options.resolved()?;
    let mut result = StyleResult {
        text: text.into(),
        edits: vec![],
    };
    let mut budget = options.max_changes as usize;
    for stage in 0..=4 {
        if budget == 0 {
            break;
        }
        let mut candidates: Vec<_> = style_candidates(&result.text, &options)
            .into_iter()
            .filter(|e| e.stage == stage)
            .collect();
        if candidates.is_empty() {
            continue;
        }
        if stage < 4 && options.probability == 0.0 {
            continue;
        }
        // Gate the whole message once, at its first eligible wording stage.
        if stage < 4 && result.edits.is_empty() && options.probability < 1.0 && random() >= options.probability {
            return apply_typos(result, &options, budget, &mut random);
        }
        if stage == 4 {
            return apply_typos(result, &options, budget, &mut random);
        }
        while budget > 0 && !candidates.is_empty() {
            let index = if candidates.len() == 1 {
                0
            } else {
                (random() * candidates.len() as f64).floor() as usize
            };
            let edit = candidates.remove(index.min(candidates.len() - 1));
            apply_style_edit(&mut result, &mut candidates, edit);
            budget -= 1;
        }
    }
    Ok(result)
}

fn apply_typos(
    mut result: StyleResult,
    options: &StyleOptions,
    budget: usize,
    random: &mut impl FnMut() -> f64,
) -> Result<StyleResult, Error> {
    let mut candidates: Vec<_> = style_candidates(&result.text, options)
        .into_iter()
        .filter(|e| e.stage == 4)
        .collect();
    if budget == 0 || options.typos.max_errors == 0 || candidates.is_empty() || options.typos.probability == 0.0 {
        return Ok(result);
    }
    if options.typos.probability < 1.0 && random() >= options.typos.probability {
        return Ok(result);
    }
    for _ in 0..budget.min(options.typos.max_errors as usize) {
        if candidates.is_empty() {
            break;
        }
        let index = if candidates.len() == 1 {
            0
        } else {
            (random() * candidates.len() as f64).floor() as usize
        };
        let edit = candidates.remove(index.min(candidates.len() - 1));
        apply_style_edit(&mut result, &mut candidates, edit);
    }
    Ok(result)
}

fn apply_style_edit(result: &mut StyleResult, candidates: &mut Vec<StyleEdit>, edit: StyleEdit) {
    candidates.retain(|other| !overlaps(&other.range, &edit.range));
    let difference = edit.replacement.len() as isize - edit.range.len() as isize;
    for other in candidates.iter_mut().filter(|other| other.range.start >= edit.range.end) {
        other.range =
            other.range.start.checked_add_signed(difference).unwrap()..other.range.end.checked_add_signed(difference).unwrap();
    }
    result.text.replace_range(edit.range.clone(), &edit.replacement);
    result.edits.push(edit);
}

/// Realizes structured input and protects authored subject references before styling.
pub fn vary_sentence_styled<R: Rng + ?Sized>(
    sentence: &Sentence,
    options: &StyleOptions,
    rng: &mut R,
) -> Result<StyleResult, Error> {
    let text = realize(sentence, &options.rendering)?;
    let mut options = options.clone();
    for subject in std::iter::once(&sentence.clause.subject).chain(sentence.condition.iter().map(|c| &c.subject)) {
        if !["i", "you", "he", "she", "it", "we", "they"].contains(&subject.text.to_ascii_lowercase().as_str()) {
            options.protected.push(subject.text.clone());
            options.protected.push(style_case("A", &subject.text));
        }
    }
    vary_styled(&text, &options, rng)
}

/// An authored greeting or closing and its relative selection weight.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightedPhrase {
    pub text: String,
    #[serde(default = "super::model::expression_weight")]
    pub weight: f64,
}

/// Reuse the same value for a speaker across turns. Directness is expressed by
/// family weights, verbosity by optional-detail controls, and register by wording.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeakerProfile {
    pub wording: StyleOptions,
    pub family_weights: std::collections::BTreeMap<String, f64>,
    pub pattern_weights: std::collections::BTreeMap<String, f64>,
    pub organization: std::collections::BTreeMap<String, f64>,
    pub elaboration: std::collections::BTreeMap<String, f64>,
    pub sentence_shape: std::collections::BTreeMap<String, f64>,
    pub reference_style: std::collections::BTreeMap<String, f64>,
    pub greetings: Vec<WeightedPhrase>,
    pub greeting_probability: f64,
    pub closings: Vec<WeightedPhrase>,
    pub closing_probability: f64,
    pub detail_probability: f64,
    #[serde(deserialize_with = "count_option")]
    pub max_details: u8,
}

impl Default for SpeakerProfile {
    fn default() -> Self {
        Self {
            wording: StyleOptions {
                style: Some(Style::Neutral),
                ..StyleOptions::default()
            },
            family_weights: Default::default(),
            pattern_weights: Default::default(),
            organization: Default::default(),
            elaboration: Default::default(),
            sentence_shape: Default::default(),
            reference_style: Default::default(),
            greetings: vec![],
            greeting_probability: 0.5,
            closings: vec![],
            closing_probability: 0.0,
            detail_probability: 0.5,
            max_details: 1,
        }
    }
}

/// Repetition penalties apply to previously used families and constructions of
/// the same intent. Zero excludes repeats, one leaves their weights unchanged.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpressionOptions {
    pub speaker: SpeakerProfile,
    pub context: super::discourse::ExpressionContext,
    pub repeat_penalty: f64,
}

impl Default for ExpressionOptions {
    fn default() -> Self {
        Self {
            speaker: SpeakerProfile::default(),
            context: Default::default(),
            repeat_penalty: 0.25,
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::{RngCore, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    use super::*;
    use crate::english::{Aspect, Clause, Condition, Modal, Number, Person, Predicate, Subject, Tense, Verb};

    fn sentence() -> Sentence {
        let subject = Subject {
            text: "the guest".into(),
            person: Person::Third,
            number: Number::Singular,
        };
        Sentence {
            clause: Clause {
                subject: subject.clone(),
                modal: Some(Modal::Can),
                tense: Tense::Present,
                aspect: Aspect::Simple,
                polarity: Polarity::Negative,
                predicate: Predicate {
                    verb: Verb::lookup("check").unwrap(),
                    complement: Some("in with booking ZX-42".into()),
                },
                form: Form::Statement,
            },
            condition: Some(Condition {
                subject,
                predicate: Predicate {
                    verb: Verb::lookup("arrive").unwrap(),
                    complement: Some("late".into()),
                },
                polarity: Polarity::Positive,
                tense: Tense::Present,
                aspect: Aspect::Simple,
            }),
        }
    }

    #[test]
    fn selections_are_explicit_and_do_not_change_semantic_features() {
        let sentence = sentence();
        let original = sentence.clone();
        let rendering = RealizeOptions::default();
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        assert_eq!(
            applicable_variations(&sentence, &rendering, &options).unwrap(),
            [
                Variation::Negation(NegationStyle::Contracted),
                Variation::ConditionPosition(ConditionPosition::After),
            ]
        );
        let changed = apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Contracted)).unwrap();
        assert_eq!(
            realize(&sentence, &changed).unwrap(),
            "If the guest arrives late, the guest can't check in with booking ZX-42."
        );
        assert_eq!(sentence, original);
        assert!(apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Expanded)).is_err());
    }

    #[test]
    fn seeded_choices_are_repeatable_and_only_produce_supported_alternatives() {
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        let sequence = |seed| {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
            (0..64)
                .map(|_| vary_sentence(&sentence(), &RealizeOptions::default(), &options, &mut rng).unwrap())
                .collect::<Vec<_>>()
        };
        let outputs = sequence(42);
        assert_eq!(outputs, sequence(42));
        assert_ne!(outputs, sequence(43));
        let expected = [
            "If the guest arrives late, the guest cannot check in with booking ZX-42.",
            "If the guest arrives late, the guest can't check in with booking ZX-42.",
            "The guest cannot check in with booking ZX-42 if the guest arrives late.",
        ];
        assert!(outputs.iter().all(|output| expected.contains(&output.as_str())));
        assert!(
            expected
                .iter()
                .all(|expected| outputs.iter().any(|output| output == expected))
        );
    }

    #[test]
    fn unsupported_text_and_disabled_variations_do_not_consume_randomness() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut untouched = rng.clone();
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for source in [
            "Don't have to cancel.",
            "They can not cancel.",
            "The guest can check in.",
            "",
            "🙂",
        ] {
            assert_eq!(vary(source, &options, &mut rng).unwrap(), source);
        }
        assert_eq!(
            vary("The guest cannot check in.", &VariationOptions::default(), &mut rng).unwrap(),
            "The guest cannot check in."
        );
        assert_eq!(rng.next_u64(), untouched.next_u64());
    }

    #[test]
    fn text_variation_preserves_identifiers_and_is_reproducible() {
        let source = "  They cannot update résumé_ID-42.\n";
        let options = VariationOptions {
            negation: true,
            condition_order: false,
        };
        let mut first = Xoshiro256PlusPlus::seed_from_u64(19);
        let mut second = first.clone();
        let outputs = (0..32)
            .map(|_| {
                let output = vary(source, &options, &mut first).unwrap();
                assert_eq!(output, vary(source, &options, &mut second).unwrap());
                assert!([source, "  They can't update résumé_ID-42.\n"].contains(&output.as_str()));
                output
            })
            .collect::<Vec<_>>();
        assert!(outputs.iter().any(|output| output == source));
        assert!(outputs.iter().any(|output| output != source));
    }

    #[test]
    fn unsupported_styles_and_negative_questions_are_not_random_alternatives() {
        let mut sentence = sentence();
        sentence.condition = None;
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for modal in [Modal::May, Modal::Might] {
            sentence.clause.modal = Some(modal);
            assert!(
                applicable_variations(&sentence, &RealizeOptions::default(), &options)
                    .unwrap()
                    .is_empty()
            );
        }
        sentence.clause.modal = Some(Modal::Can);
        sentence.clause.form = Form::Question;
        assert!(
            applicable_variations(&sentence, &RealizeOptions::default(), &options)
                .unwrap()
                .is_empty()
        );
        sentence.clause.subject.text.clear();
        assert!(applicable_variations(&sentence, &RealizeOptions::default(), &options).is_err());
    }

    #[test]
    fn conditional_reordering_keeps_references_and_identifiers() {
        let options = VariationOptions {
            negation: false,
            condition_order: true,
        };
        for (before, after) in [
            (
                "If the guest arrives late, can they check in?",
                "Can they check in if the guest arrives late?",
            ),
            (
                "  If Maya has finished, api_v2 will update résumé_ID-42.\n",
                "  api_v2 will update résumé_ID-42 if Maya has finished.\n",
            ),
        ] {
            assert_eq!(text_alternatives(before, &options), [after]);
            assert_eq!(text_alternatives(after, &options), [before]);
        }
    }

    #[test]
    fn contractions_use_consistent_register_and_preserve_other_bytes() {
        let options = VariationOptions {
            negation: true,
            condition_order: false,
        };
        assert_eq!(
            text_alternatives("I won't leave. They cannot stay.", &options),
            ["I will not leave. They cannot stay.", "I won't leave. They can't stay.",]
        );
        assert_eq!(
            text_alternatives("If they do not arrive, Maya cannot leave.", &options),
            ["If they don't arrive, Maya can't leave."]
        );
        assert_eq!(
            text_alternatives("Maya cannot leave if they do not arrive.", &options),
            ["Maya can't leave if they don't arrive."]
        );
    }

    #[test]
    fn unsupported_clauses_prevent_partial_rewrites() {
        let options = VariationOptions {
            negation: true,
            condition_order: true,
        };
        for input in [
            "If the guest arrives, can't they check in?",
            "The guest will leave when they arrive.",
            "If they arrive if she leaves, Maya cannot stay.",
            "I cannot leave. She's already gone.",
            "If they arrive, Maya cannot leave, or stay.",
        ] {
            assert!(text_alternatives(input, &options).is_empty(), "{input}");
        }
    }
}
