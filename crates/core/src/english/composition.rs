//! Explicit relationships between English propositions.

use serde::{Deserialize, Serialize};

use super::{Error, Form, RealizeOptions, Sentence, realize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    And,
    But,
    Or,
    Because,
}

/// Combines declarative propositions using an explicitly supplied relationship.
///
/// Every proposition retains its subject, tense, and polarity. No cause,
/// contrast, shared subject, or entity reference is inferred from the wording.
/// For `because`, the first proposition is the effect and the second is the
/// cause. The cause is rendered first to keep negation attached to the effect.
/// Conditional propositions require an explicit grouping and are rejected.
pub fn combine(sentences: &[Sentence], relation: Relation, options: &RealizeOptions) -> Result<String, Error> {
    if sentences.len() < 2 || (relation == Relation::Because && sentences.len() != 2) {
        return Err(Error::InvalidField {
            field: "sentences",
            reason: "combine requires at least two propositions and because requires exactly two",
        });
    }
    if sentences.iter().any(|sentence| sentence.clause.form != Form::Statement) {
        return Err(Error::UnsupportedConstruction("combine requires declarative propositions"));
    }
    if sentences.iter().any(|sentence| sentence.condition.is_some()) {
        return Err(Error::UnsupportedConstruction(
            "combine requires unconditional propositions to preserve condition scope",
        ));
    }
    let separator = match relation {
        Relation::And => ", and ",
        Relation::But => ", but ",
        Relation::Or => ", or ",
        Relation::Because => {
            return Ok(format!(
                "Because {}, {}.",
                super::realization::realize_body(&sentences[1], options)?,
                super::realization::realize_body(&sentences[0], options)?,
            ));
        }
    };
    let mut result = realize(&sentences[0], options)?;
    result.pop();
    for sentence in &sentences[1..] {
        result.push_str(separator);
        result.push_str(&super::realization::realize_body(sentence, options)?);
    }
    result.push('.');
    Ok(result)
}

/// An eligible realization and its normalized selection probability.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpressionCandidate {
    pub selection: super::ExpressionSelection,
    pub text: String,
    pub used_facts: Vec<String>,
    pub establishes: Vec<String>,
    #[serde(serialize_with = "expression_probability_json")]
    pub probability: f64,
}

/// A generated utterance, its selection, style edits and next explicit context.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpressionResult {
    pub text: String,
    pub base_text: String,
    pub selection: super::ExpressionSelection,
    pub used_facts: Vec<String>,
    pub context: super::discourse::ExpressionContext,
    pub edits: Vec<super::StyleEdit>,
}

#[derive(Serialize)]
struct ExpressionPattern {
    id: &'static str,
    prefix: &'static str,
    suffix: &'static str,
    gerund: bool,
}

fn patterns(kind: super::ExpressionKind) -> Vec<ExpressionPattern> {
    use super::ExpressionKind::*;
    let rows: &[(&str, &str, &str, bool)] = match kind {
        Request => &[
            ("request_please", "Please ", ".", false),
            ("request_could_you", "Could you ", "?", false),
            ("request_would_you", "Would you ", "?", false),
        ],
        Desire => &[
            ("desire_would_like", "I would like to ", ".", false),
            ("desire_want", "I want to ", ".", false),
            ("desire_looking_to", "I am looking to ", ".", false),
        ],
        Possibility => &[
            ("possibility_can_i", "Can I ", "?", false),
            ("possibility_possible", "Is it possible to ", "?", false),
            ("possibility_able", "Would I be able to ", "?", false),
        ],
        Procedure => &[
            ("procedure_how_do", "How do I ", "?", false),
            ("procedure_how_can", "How can I ", "?", false),
            ("procedure_go_about", "How do I go about ", "?", true),
        ],
        Need => &[("need_need_to", "I need to ", ".", false)],
        ReasonRequest => &[
            ("reason_request_could_you", "Could you ", "?", false),
            ("reason_request_please", "Please ", ".", false),
        ],
        FollowUp | Authored | Structured => &[],
    };
    rows.iter()
        .map(|&(id, prefix, suffix, gerund)| ExpressionPattern {
            id,
            prefix,
            suffix,
            gerund,
        })
        .collect()
}

pub(crate) fn expression_data() -> serde_json::Value {
    use super::ExpressionKind::*;
    let mut kinds = serde_json::Map::new();
    for kind in [
        Request,
        Desire,
        Possibility,
        Procedure,
        Need,
        ReasonRequest,
        FollowUp,
        Authored,
        Structured,
    ] {
        kinds.insert(
            serde_json::to_value(kind).unwrap().as_str().unwrap().into(),
            serde_json::to_value(patterns(kind)).unwrap(),
        );
    }
    serde_json::json!({"patterns":kinds,"messages": message_data(),"options": super::ExpressionOptions::default()})
}

fn expression_probability_json<S: serde::Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if *value == 0.0 || *value == 1.0 {
        serializer.serialize_u8(*value as u8)
    } else {
        serializer.serialize_f64(*value)
    }
}

fn expression_error(reason: &'static str) -> Error {
    Error::InvalidField {
        field: "expression",
        reason,
    }
}
fn expression_text(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && !text.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}
fn unique_texts(texts: &[String]) -> bool {
    texts.iter().all(|s| expression_text(s)) && texts.iter().collect::<std::collections::BTreeSet<_>>().len() == texts.len()
}
fn expression_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
fn expression_weight_valid(value: f64) -> bool {
    value.is_finite() && (0.0..=1_000_000.0).contains(&value)
}
fn complete_expression(text: &str) -> String {
    if text.ends_with(['.', '?', '!']) {
        text.into()
    } else {
        format!("{text}.")
    }
}
fn append_unique(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        if !target.contains(value) {
            target.push(value.clone());
        }
    }
}

fn validate_expression(source: &super::ExpressionSource, options: &super::ExpressionOptions) -> Result<(), Error> {
    use super::ExpressionKind::*;
    validate_messages(source, options)?;
    let ids: Vec<_> = source.constructions.iter().map(|c| c.id.clone()).collect();
    let families: Vec<_> = source.families.iter().map(|f| f.id.clone()).collect();
    let facts: Vec<_> = source.facts.iter().map(|f| f.id.clone()).collect();
    if !expression_text(&source.intent)
        || source.families.is_empty()
        || !unique_texts(&ids)
        || !unique_texts(&families)
        || !unique_texts(&facts)
        || !unique_texts(&source.optional_details)
        || source.facts.iter().any(|f| !expression_text(&f.text))
        || source.protected.iter().any(|s| s.is_empty())
    {
        return Err(expression_error("expected nonempty text and unique identifiers"));
    }
    if source.optional_details.iter().any(|id| !facts.contains(id)) {
        return Err(expression_error("optional detail references an unknown fact"));
    }
    for construction in &source.constructions {
        super::realization::expression_action(&construction.predicate, false)?;
        if !expression_weight_valid(construction.weight)
            || !unique_texts(&construction.requires)
            || !unique_texts(&construction.excludes)
            || !unique_texts(&construction.mentions)
            || construction.requires.iter().any(|id| construction.excludes.contains(id))
        {
            return Err(expression_error("invalid construction controls"));
        }
    }
    for family in &source.families {
        if !expression_weight_valid(family.weight)
            || [
                &family.requires,
                &family.excludes,
                &family.context_requires,
                &family.establishes,
                &family.mentions,
                &family.constructions,
                &family.patterns,
            ]
            .iter()
            .any(|v| !unique_texts(v))
            || family.requires.iter().any(|id| family.excludes.contains(id))
            || family.constructions.iter().any(|id| !ids.contains(id))
        {
            return Err(expression_error("invalid family controls or construction reference"));
        }
        if family.kind == Structured {
            if family.message.is_none()
                || family.text.is_some()
                || family.reason.is_some()
                || !family.constructions.is_empty()
                || !family.patterns.is_empty()
            {
                return Err(expression_error(
                    "structured families require a message and no action or text",
                ));
            }
        } else if matches!(family.kind, Authored | FollowUp) {
            if family.text.as_deref().is_none_or(|s| !expression_text(s))
                || family.reason.is_some()
                || !family.constructions.is_empty()
                || !family.patterns.is_empty()
                || family.kind == FollowUp && family.context_requires.is_empty()
            {
                return Err(expression_error(
                    "authored families need text and follow-ups need explicit context requirements",
                ));
            }
        } else if family.text.is_some()
            || (family.kind == ReasonRequest) != family.reason.is_some()
            || family.reason.as_deref().is_some_and(|s| !expression_text(s))
            || family
                .patterns
                .iter()
                .any(|id| !patterns(family.kind).iter().any(|p| p.id == id))
        {
            return Err(expression_error("invalid built-in family text, reason or pattern"));
        }
    }
    let speaker = &options.speaker;
    speaker.wording.resolved()?;
    if !expression_probability(options.repeat_penalty)
        || !expression_probability(speaker.greeting_probability)
        || !expression_probability(speaker.closing_probability)
        || !expression_probability(speaker.detail_probability)
        || speaker.max_details > 16
        || !unique_texts(&options.context.established)
        || options.context.recent.len() > 32
    {
        return Err(expression_error("invalid probabilities, detail budget or context"));
    }
    for selection in &options.context.recent {
        if let Some(m) = &selection.message
            && ([
                &m.layout,
                &m.recipe,
                &m.organization,
                &m.elaboration,
                &m.sentence_shape,
                &m.reference_style,
            ]
            .iter()
            .any(|v| !expression_text(v))
                || !unique_texts(&m.forms))
        {
            return Err(expression_error("invalid message history"));
        }
        if [&selection.intent, &selection.family, &selection.pattern]
            .iter()
            .any(|s| !expression_text(s))
            || selection.construction.as_deref().is_some_and(|s| !expression_text(s))
        {
            return Err(expression_error("invalid selection history"));
        }
    }
    if speaker
        .family_weights
        .iter()
        .chain(&speaker.pattern_weights)
        .any(|(id, w)| !expression_text(id) || !expression_weight_valid(*w))
        || speaker
            .greetings
            .iter()
            .chain(&speaker.closings)
            .any(|p| !expression_text(&p.text) || !expression_weight_valid(p.weight))
    {
        return Err(expression_error("invalid speaker weights or phrases"));
    }
    Ok(())
}

/// Lists applicable expression choices without sampling or inferring facts.
/// Family, construction and pattern weights are normalized at their respective
/// levels so adding vocabulary does not implicitly increase a family's frequency.
pub fn expression_candidates(
    source: &super::ExpressionSource,
    options: &super::ExpressionOptions,
) -> Result<Vec<ExpressionCandidate>, Error> {
    use super::ExpressionKind::*;
    validate_expression(source, options)?;
    let facts: std::collections::BTreeMap<_, _> = source.facts.iter().map(|f| (f.id.as_str(), f.text.as_str())).collect();
    let applies = |requires: &[String], excludes: &[String], mentions: &[String]| {
        requires.iter().chain(mentions).all(|id| facts.contains_key(id.as_str()))
            && excludes.iter().all(|id| !facts.contains_key(id.as_str()))
    };
    let recent: Vec<_> = options.context.recent.iter().filter(|s| s.intent == source.intent).collect();
    let mut result = Vec::new();
    let mut family_total = 0.0;
    for family in &source.families {
        if !applies(&family.requires, &family.excludes, &family.mentions)
            || !family
                .context_requires
                .iter()
                .all(|id| options.context.established.contains(id))
            || family.reason.as_deref().is_some_and(|id| !facts.contains_key(id))
        {
            continue;
        }
        let mut family_weight = family.weight
            * options.speaker.family_weights.get(&family.id).copied().unwrap_or(1.0)
            * if recent.iter().any(|s| s.family == family.id) {
                options.repeat_penalty
            } else {
                1.0
            };
        if let Some(plan) = &family.message {
            let data = message_data();
            let layout = &data["layouts"][&plan.layout];
            let elaboration = if !plan.details.is_empty() {
                "detailed"
            } else if layout["act"].is_string() || plan.layout == "request_only" {
                "minimal"
            } else {
                "relevant_context"
            };
            family_weight *= options
                .speaker
                .organization
                .get(layout["organization"].as_str().unwrap())
                .copied()
                .unwrap_or(1.0)
                * options.speaker.elaboration.get(elaboration).copied().unwrap_or(1.0);
        }
        if family_weight == 0.0 {
            continue;
        }
        let mut group = Vec::new();
        let mut construction_total = 0.0;
        let mut facts_used = family.mentions.clone();
        if let Some(reason) = &family.reason {
            append_unique(&mut facts_used, std::slice::from_ref(reason));
        }
        if matches!(family.kind, Authored | FollowUp | Structured) {
            let pattern = match family.kind {
                Authored => "authored",
                FollowUp => "follow_up",
                _ => "structured",
            };
            let weight = options.speaker.pattern_weights.get(pattern).copied().unwrap_or(1.0);
            if weight > 0.0 {
                construction_total = 1.0;
                group.push(ExpressionCandidate {
                    selection: super::ExpressionSelection {
                        intent: source.intent.clone(),
                        family: family.id.clone(),
                        construction: None,
                        pattern: pattern.into(),
                        message: None,
                    },
                    text: family.text.clone().unwrap_or_default(),
                    used_facts: facts_used.clone(),
                    establishes: family.establishes.clone(),
                    probability: 1.0,
                });
            }
        } else {
            let patterns: Vec<_> = patterns(family.kind)
                .into_iter()
                .filter(|p| family.patterns.is_empty() || family.patterns.iter().any(|id| id == p.id))
                .filter_map(|p| {
                    let weight = options.speaker.pattern_weights.get(p.id).copied().unwrap_or(1.0);
                    (weight > 0.0).then_some((p, weight))
                })
                .collect();
            let pattern_total: f64 = patterns.iter().map(|(_, w)| w).sum();
            if pattern_total == 0.0 {
                continue;
            }
            for construction in &source.constructions {
                if !family.constructions.is_empty() && !family.constructions.contains(&construction.id)
                    || !applies(&construction.requires, &construction.excludes, &construction.mentions)
                {
                    continue;
                }
                let weight = construction.weight
                    * if recent.iter().any(|s| s.construction.as_deref() == Some(&construction.id)) {
                        options.repeat_penalty
                    } else {
                        1.0
                    };
                if weight == 0.0 {
                    continue;
                }
                construction_total += weight;
                let mut used = construction.mentions.clone();
                append_unique(&mut used, &facts_used);
                for (pattern, pattern_weight) in &patterns {
                    let action = super::realization::expression_action(&construction.predicate, pattern.gerund)?;
                    let mut text = format!("{}{}{}", pattern.prefix, action, pattern.suffix);
                    if let Some(reason) = &family.reason {
                        text = format!("{} {text}", complete_expression(facts[reason.as_str()]));
                    }
                    group.push(ExpressionCandidate {
                        selection: super::ExpressionSelection {
                            intent: source.intent.clone(),
                            family: family.id.clone(),
                            construction: Some(construction.id.clone()),
                            pattern: pattern.id.into(),
                            message: None,
                        },
                        text,
                        used_facts: used.clone(),
                        establishes: family.establishes.clone(),
                        probability: weight * pattern_weight / pattern_total,
                    });
                }
            }
        }
        if family.message.is_some() {
            group = expand_messages(source, family, options, group)?;
            construction_total = group.iter().map(|c| c.probability).sum();
        }
        if construction_total > 0.0 {
            family_total += family_weight;
            for candidate in &mut group {
                candidate.probability = candidate.probability / construction_total * family_weight;
            }
            result.extend(group);
        }
    }
    for candidate in &mut result {
        candidate.probability /= family_total;
    }
    Ok(result)
}

fn choose_weighted<R: rand::Rng + ?Sized>(weights: &[f64], rng: &mut R) -> usize {
    if weights.len() == 1 {
        return 0;
    }
    let mut draw = rng.random::<f64>() * weights.iter().sum::<f64>();
    for (index, weight) in weights.iter().enumerate() {
        if draw < *weight {
            return index;
        }
        draw -= weight;
    }
    weights.len() - 1
}
fn expression_chance<R: rand::Rng + ?Sized>(probability: f64, rng: &mut R) -> bool {
    probability == 1.0 || probability > 0.0 && rng.random::<f64>() < probability
}
fn choose_phrase<R: rand::Rng + ?Sized>(phrases: &[super::WeightedPhrase], probability: f64, rng: &mut R) -> Option<String> {
    let phrases: Vec<_> = phrases.iter().filter(|p| p.weight > 0.0).collect();
    if phrases.is_empty() || !expression_chance(probability, rng) {
        return None;
    }
    Some(
        phrases[choose_weighted(&phrases.iter().map(|p| p.weight).collect::<Vec<_>>(), rng)]
            .text
            .clone(),
    )
}

/// Generates from author-approved intent expressions and returns explicit history.
pub fn expression<R: rand::Rng + ?Sized>(
    source: &super::ExpressionSource,
    options: &super::ExpressionOptions,
    rng: &mut R,
) -> Result<ExpressionResult, Error> {
    let candidates = expression_candidates(source, options)?;
    if candidates.is_empty() {
        return Err(expression_error("no eligible expression with positive weight"));
    }
    let chosen = &candidates[choose_weighted(&candidates.iter().map(|c| c.probability).collect::<Vec<_>>(), rng)];
    let speaker = &options.speaker;
    let mut parts = Vec::new();
    if options.context.recent.is_empty()
        && let Some(greeting) = choose_phrase(&speaker.greetings, speaker.greeting_probability, rng)
    {
        parts.push(greeting);
    }
    parts.push(chosen.text.clone());
    let mut used_facts = chosen.used_facts.clone();
    let mut details: Vec<_> = source
        .optional_details
        .iter()
        .filter(|id| !used_facts.contains(id) && !options.context.established.contains(id))
        .collect();
    for _ in 0..speaker.max_details {
        if details.is_empty() || !expression_chance(speaker.detail_probability, rng) {
            break;
        }
        let index = choose_weighted(&vec![1.0; details.len()], rng);
        let id = details.remove(index);
        let text = &source.facts.iter().find(|f| f.id == *id).unwrap().text;
        parts.push(complete_expression(text));
        used_facts.push(id.clone());
    }
    if let Some(closing) = choose_phrase(&speaker.closings, speaker.closing_probability, rng) {
        parts.push(closing);
    }
    let base_text = parts.join(" ");
    let mut wording = speaker.wording.clone();
    wording.protected.extend(source.protected.iter().cloned());
    if chosen.selection.message.is_some() {
        wording.fragments = Some(false);
    }
    let styled = super::vary_styled(&base_text, &wording, rng)?;
    let mut established = chosen.establishes.clone();
    append_unique(&mut established, &used_facts);
    let context = message_context(
        source,
        chosen,
        &used_facts,
        options.context.after(chosen.selection.clone(), &established),
    );
    Ok(ExpressionResult {
        text: styled.text,
        base_text,
        selection: chosen.selection.clone(),
        used_facts,
        context,
        edits: styled.edits,
    })
}

/// Generates the same utterance as expression and returns only its text.
pub fn express<R: rand::Rng + ?Sized>(
    source: &super::ExpressionSource,
    options: &super::ExpressionOptions,
    rng: &mut R,
) -> Result<String, Error> {
    expression(source, options, rng).map(|result| result.text)
}

// Recipes are shared with emitted runtimes. Role forms carry their own grammar.
fn message_data() -> &'static serde_json::Value {
    static DATA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    DATA.get_or_init(|| serde_json::json!({
        "preferences": {
            "organization": ["request_first", "circumstance_first", "outcome_first", "current_then_desired", "response"],
            "elaboration": ["minimal", "relevant_context", "detailed"],
            "sentence_shape": ["complete", "short_sentences", "fragments"],
            "reference_style": ["explicit", "contextual"]
        },
        "roles": ["current", "outcome", "reason", "constraint", "answer", "previous", "replacement", "acknowledgment", "clarification", "follow_up_request", "detail"],
        "layouts": {
            "request_only": {"roles":[], "organization":"request_first", "standalone":false, "act":null},
            "request_reason": {"roles":["reason"], "organization":"request_first", "standalone":false, "act":null},
            "reason_request": {"roles":["reason"], "organization":"circumstance_first", "standalone":false, "act":null},
            "outcome_request": {"roles":["outcome"], "organization":"outcome_first", "standalone":false, "act":null},
            "current_desired": {"roles":["current","outcome"], "organization":"current_then_desired", "standalone":true, "act":null},
            "constraint_request": {"roles":["constraint"], "organization":"circumstance_first", "standalone":false, "act":null},
            "answer": {"roles":["answer"], "organization":"response", "standalone":true, "act":"answer"},
            "correction": {"roles":["previous","replacement"], "organization":"response", "standalone":true, "act":"correction"},
            "acknowledgment": {"roles":["acknowledgment"], "organization":"response", "standalone":true, "act":"acknowledgment"},
            "clarification": {"roles":["clarification"], "organization":"response", "standalone":true, "act":"clarification"},
            "follow_up_request": {"roles":["follow_up_request"], "organization":"response", "standalone":true, "act":"follow_up_request"}
        },
        "recipes": [
            {"id":"request", "layout":"request_only", "text":"{request}", "forms":{}, "shape":"complete"},
            {"id":"request_then_reason", "layout":"request_reason", "text":"{request} {reason}", "forms":{"reason":"complete"}, "shape":"short_sentences"},
            {"id":"request_because", "layout":"request_reason", "text":"{request_clause} because {reason}.", "forms":{"reason":"clause"}, "shape":"complete", "relation":"cause", "declarative":true},
            {"id":"reason_then_request", "layout":"reason_request", "text":"{reason} {request}", "forms":{"reason":"complete"}, "shape":"short_sentences"},
            {"id":"because_reason", "layout":"reason_request", "text":"Because {reason}, {request}", "forms":{"reason":"clause"}, "shape":"complete", "relation":"cause", "declarative":true},
            {"id":"outcome_then_request", "layout":"outcome_request", "text":"{outcome} {request}", "forms":{"outcome":"complete"}, "shape":"short_sentences"},
            {"id":"outcome_fragment", "layout":"outcome_request", "text":"{outcome}. {request}", "forms":{"outcome":"fragments"}, "shape":"fragments"},
            {"id":"current_then_outcome", "layout":"current_desired", "text":"{current} {outcome}", "forms":{"current":"complete","outcome":"complete"}, "shape":"short_sentences"},
            {"id":"current_but_outcome", "layout":"current_desired", "text":"{current_clause}, but {outcome}.", "forms":{"current":"complete","outcome":"clause"}, "shape":"complete", "relation":"contrast"},
            {"id":"constraint_then_request", "layout":"constraint_request", "text":"{constraint} {request}", "forms":{"constraint":"complete"}, "shape":"short_sentences"},
            {"id":"answer_complete", "layout":"answer", "text":"{answer}", "forms":{"answer":"complete"}, "shape":"complete"},
            {"id":"answer_fragment", "layout":"answer", "text":"{answer}", "forms":{"answer":"fragments"}, "shape":"fragments"},
            {"id":"correct_explicit", "layout":"correction", "text":"{replacement} I previously said {previous}.", "forms":{"previous":"value","replacement":"complete"}, "shape":"short_sentences"},
            {"id":"correct_contextual", "layout":"correction", "text":"I meant {replacement}, not {previous}.", "forms":{"previous":"value","replacement":"value"}, "shape":"complete", "reference":"contextual"},
            {"id":"correct_fragment", "layout":"correction", "text":"{replacement}, not {previous}", "forms":{"previous":"value","replacement":"value"}, "shape":"fragments", "reference":"contextual"},
            {"id":"correct_apology", "layout":"correction", "text":"Sorry, I meant {replacement}, not {previous}.", "forms":{"previous":"value","replacement":"value"}, "shape":"complete", "reference":"contextual", "apology":true},
            {"id":"acknowledge", "layout":"acknowledgment", "text":"{acknowledgment}", "forms":{"acknowledgment":"complete"}, "shape":"complete"},
            {"id":"clarify", "layout":"clarification", "text":"{clarification}", "forms":{"clarification":"complete"}, "shape":"complete"},
            {"id":"follow_up", "layout":"follow_up_request", "text":"{follow_up_request}", "forms":{"follow_up_request":"complete"}, "shape":"complete"}
        ]
    }))
}

fn validate_messages(source: &super::ExpressionSource, options: &super::ExpressionOptions) -> Result<(), Error> {
    let data = message_data();
    let valid = |values: &serde_json::Value, value: &str| values.as_array().unwrap().iter().any(|v| v == value);
    if source.topic.as_deref().is_some_and(|v| !expression_text(v))
        || options.context.topic.as_deref().is_some_and(|v| !expression_text(v))
        || [
            &options.context.requested,
            &options.context.known,
            &options.context.offered,
            &options.context.allowed_acts,
        ]
        .iter()
        .any(|v| !unique_texts(v))
        || options
            .context
            .allowed_acts
            .iter()
            .any(|v| !["answer", "correction", "acknowledgment", "clarification", "follow_up_request"].contains(&v.as_str()))
        || !unique_texts(&source.components.iter().map(|c| c.id.clone()).collect::<Vec<_>>())
    {
        return Err(expression_error("invalid message identifiers or context"));
    }
    for (name, weights) in [
        ("organization", &options.speaker.organization),
        ("elaboration", &options.speaker.elaboration),
        ("sentence_shape", &options.speaker.sentence_shape),
        ("reference_style", &options.speaker.reference_style),
    ] {
        if weights
            .iter()
            .any(|(key, weight)| !valid(&data["preferences"][name], key) || !expression_weight_valid(*weight))
        {
            return Err(expression_error("invalid message preference"));
        }
    }
    for component in &source.components {
        if !valid(&data["roles"], &component.role)
            || component.forms.is_empty()
            || [&component.mentions, &component.requires, &component.context_requires]
                .iter()
                .any(|v| !unique_texts(v))
            || [component.slot.as_deref(), component.option.as_deref()]
                .iter()
                .flatten()
                .any(|v| !expression_text(v))
            || !unique_texts(&component.forms.iter().map(|f| f.id.clone()).collect::<Vec<_>>())
        {
            return Err(expression_error("invalid message component"));
        }
        for form in &component.forms {
            if !expression_text(&form.text)
                || !expression_weight_valid(form.weight)
                || !unique_texts(&form.context_requires)
                || !["complete", "fragments", "clause", "value"].contains(&form.shape.as_str())
                || !["explicit", "contextual"].contains(&form.reference.as_str())
                || form.reference == "contextual"
                    && (source.topic.is_none() || component.context_requires.is_empty() && form.context_requires.is_empty())
                || form.shape != "complete" && form.text.ends_with(['.', '?', '!'])
            {
                return Err(expression_error("invalid component form or contextual reference guard"));
            }
        }
    }
    for family in &source.families {
        let Some(plan) = &family.message else { continue };
        let Some(layout) = data["layouts"].get(&plan.layout) else {
            return Err(expression_error("unknown message layout"));
        };
        let roles = layout["roles"].as_array().unwrap();
        if plan.bindings.len() != roles.len()
            || roles.iter().any(|role| !plan.bindings.contains_key(role.as_str().unwrap()))
            || (family.kind == super::ExpressionKind::Structured) != layout["standalone"].as_bool().unwrap()
            || family.kind == super::ExpressionKind::ReasonRequest
            || !source.optional_details.is_empty()
            || !unique_texts(&plan.details)
            || plan.allow_apology && plan.layout != "correction"
            || plan.relation.as_deref().is_some_and(|r| {
                !matches!(
                    (plan.layout.as_str(), r),
                    ("request_reason" | "reason_request", "cause") | ("current_desired", "contrast")
                )
            })
        {
            return Err(expression_error(
                "incompatible message layout, relationship or optional details",
            ));
        }
        for (role, id) in &plan.bindings {
            if !source.components.iter().any(|c| c.id == *id && c.role == *role) {
                return Err(expression_error(
                    "message binding references an unknown component or wrong role",
                ));
            }
        }
        for id in &plan.details {
            if !source.components.iter().any(|c| c.id == *id && c.role == "detail") {
                return Err(expression_error("message detail requires a detail component"));
            }
        }
        if layout["act"].is_string() && source.topic.is_none() {
            return Err(expression_error("responses require an explicit topic"));
        }
        if matches!(plan.layout.as_str(), "answer" | "clarification" | "correction") {
            let bound: Vec<_> = plan
                .bindings
                .values()
                .map(|id| source.components.iter().find(|c| c.id == *id).unwrap())
                .collect();
            if bound.iter().any(|c| c.slot.is_none()) || bound.windows(2).any(|pair| pair[0].slot != pair[1].slot) {
                return Err(expression_error("answers and corrections require matching explicit slots"));
            }
            if plan.layout == "correction"
                && (bound.iter().any(|c| c.mentions.is_empty())
                    || bound[0].mentions.iter().any(|id| bound[1].mentions.contains(id)))
            {
                return Err(expression_error(
                    "corrections require distinct previous and replacement fact identifiers",
                ));
            }
        }
    }
    Ok(())
}

fn expand_messages(
    source: &super::ExpressionSource,
    family: &super::ExpressionFamily,
    options: &super::ExpressionOptions,
    bases: Vec<ExpressionCandidate>,
) -> Result<Vec<ExpressionCandidate>, Error> {
    let data = message_data();
    let plan = family.message.as_ref().unwrap();
    let layout = &data["layouts"][&plan.layout];
    let context = &options.context;
    let topic_matches = source.topic.is_some() && source.topic == context.topic;
    if let Some(act) = layout["act"].as_str()
        && (!topic_matches || !context.allowed_acts.iter().any(|a| a == act))
    {
        return Ok(vec![]);
    }
    let component = |id: &str| source.components.iter().find(|c| c.id == id).unwrap();
    for (role, id) in &plan.bindings {
        let c = component(id);
        if matches!(plan.layout.as_str(), "answer" | "clarification") && !context.requested.contains(c.slot.as_ref().unwrap())
            || role == "previous" && !c.mentions.iter().all(|id| context.established.contains(id))
        {
            return Ok(vec![]);
        }
    }
    let applies = |c: &super::MessageComponent| {
        c.mentions
            .iter()
            .chain(&c.requires)
            .all(|id| source.facts.iter().any(|f| f.id == *id) || context.known.contains(id))
            && c.context_requires.iter().all(|id| context.established.contains(id))
            && c.option
                .as_ref()
                .is_none_or(|id| topic_matches && context.offered.contains(id))
    };
    let weight = |map: &std::collections::BTreeMap<String, f64>, key: &str| map.get(key).copied().unwrap_or(1.0);
    let organization = layout["organization"].as_str().unwrap();
    let elaboration = if !plan.details.is_empty() {
        "detailed"
    } else if layout["act"].is_string() || plan.layout == "request_only" {
        "minimal"
    } else {
        "relevant_context"
    };
    let preference = weight(&options.speaker.organization, organization) * weight(&options.speaker.elaboration, elaboration);
    if preference == 0.0 {
        return Ok(vec![]);
    }
    let mut result = Vec::new();
    for base in bases {
        let mut expanded = Vec::new();
        for recipe in data["recipes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["layout"] == plan.layout)
        {
            if recipe.get("relation").is_some_and(|r| r.as_str() != plan.relation.as_deref())
                || recipe["apology"] == true && !plan.allow_apology
                || recipe["declarative"] == true
                    && !["desire_would_like", "desire_want", "desire_looking_to", "need_need_to"]
                        .contains(&base.selection.pattern.as_str())
            {
                continue;
            }
            let mut choices = vec![(base.used_facts.clone(), Vec::new(), recipe["reference"] == "contextual", 1.0)];
            let roles = layout["roles"].as_array().unwrap();
            let slots: Vec<_> = roles
                .iter()
                .map(|r| {
                    (
                        r.as_str().unwrap(),
                        component(&plan.bindings[r.as_str().unwrap()]),
                        recipe["forms"][r.as_str().unwrap()].as_str().unwrap(),
                    )
                })
                .chain(plan.details.iter().map(|id| ("detail", component(id), "complete")))
                .collect();
            for (role, c, shape) in slots {
                if !applies(c) {
                    choices.clear();
                    break;
                }
                let forms: Vec<_> = c
                    .forms
                    .iter()
                    .filter(|f| {
                        f.shape == shape
                            && f.weight > 0.0
                            && !(role == "current" && recipe["relation"] == "contrast" && f.text.ends_with(['?', '!']))
                            && f.context_requires.iter().all(|id| context.established.contains(id))
                            && (f.reference != "contextual" || topic_matches)
                    })
                    .collect();
                let total: f64 = forms.iter().map(|f| f.weight).sum();
                if choices.len().saturating_mul(forms.len()) > 16384 {
                    return Err(expression_error("message expansion exceeds 16384 choices"));
                }
                let mut next = Vec::new();
                for (used, selected, contextual, w) in choices {
                    for f in &forms {
                        let mut mentions = used.clone();
                        append_unique(&mut mentions, &c.mentions);
                        let mut forms = selected.clone();
                        forms.push(message_form_id(&c.id, &f.id));
                        next.push((
                            mentions,
                            forms,
                            contextual || f.reference == "contextual",
                            w * f.weight / total,
                        ));
                    }
                }
                choices = next;
            }
            let shape = if !plan.details.is_empty() && recipe["shape"] == "complete" {
                "short_sentences"
            } else {
                recipe["shape"].as_str().unwrap()
            };
            for (used, forms, contextual, w) in choices {
                let text = render_message(recipe, plan, source, &base.text, &forms);
                if contextual && !topic_matches {
                    continue;
                }
                let reference = if contextual { "contextual" } else { "explicit" };
                let probability =
                    w * weight(&options.speaker.sentence_shape, shape) * weight(&options.speaker.reference_style, reference);
                if probability == 0.0 {
                    continue;
                }
                let mut candidate = base.clone();
                candidate.text = text;
                candidate.used_facts = used;
                candidate.probability = probability;
                candidate.selection.message = Some(super::MessageSelection {
                    layout: plan.layout.clone(),
                    recipe: recipe["id"].as_str().unwrap().into(),
                    forms,
                    organization: organization.into(),
                    elaboration: elaboration.into(),
                    sentence_shape: shape.into(),
                    reference_style: reference.into(),
                });
                expanded.push(candidate);
            }
        }
        let total: f64 = expanded.iter().map(|c| c.probability).sum();
        for c in &mut expanded {
            c.probability = c.probability / total * base.probability * preference;
        }
        result.extend(expanded);
        if result.len() > 16384 {
            return Err(expression_error("message expansion exceeds 16384 choices"));
        }
    }
    Ok(result)
}

fn message_context(
    source: &super::ExpressionSource,
    chosen: &ExpressionCandidate,
    used: &[String],
    mut context: super::ExpressionContext,
) -> super::ExpressionContext {
    if let Some(topic) = &source.topic {
        if context.topic.as_ref() != Some(topic) {
            context.requested.clear();
            context.offered.clear();
            context.allowed_acts.clear();
        }
        context.topic = Some(topic.clone());
        append_unique(&mut context.known, used);
    }
    if let Some(selection) = &chosen.selection.message {
        let family = source.families.iter().find(|f| f.id == chosen.selection.family).unwrap();
        let plan = family.message.as_ref().unwrap();
        let component = |role: &str| source.components.iter().find(|c| c.id == plan.bindings[role]).unwrap();
        if selection.layout == "answer" {
            context.requested.retain(|id| Some(id) != component("answer").slot.as_ref());
        }
        if selection.layout == "correction" {
            context.known.retain(|id| !component("previous").mentions.contains(id));
        }
        if message_data()["layouts"][&selection.layout]["act"].is_string() {
            context.allowed_acts.clear();
        }
    }
    context
}

fn message_form_id(component: &str, form: &str) -> String {
    let escape = |value: &str| value.replace('~', "~0").replace('/', "~1");
    format!("{}/{}", escape(component), escape(form))
}

fn render_message(
    recipe: &serde_json::Value,
    plan: &super::MessagePlan,
    source: &super::ExpressionSource,
    request: &str,
    forms: &[String],
) -> String {
    let mut values = std::collections::BTreeMap::from([
        ("request".to_owned(), request.to_owned()),
        ("request_clause".to_owned(), request.trim_end_matches('.').to_owned()),
    ]);
    let data = message_data();
    let roles = data["layouts"][&plan.layout]["roles"].as_array().unwrap();
    let bindings: Vec<_> = roles
        .iter()
        .map(|r| (r.as_str().unwrap(), plan.bindings[r.as_str().unwrap()].as_str()))
        .chain(plan.details.iter().map(|id| ("detail", id.as_str())))
        .collect();
    let mut details = Vec::new();
    for ((role, id), selected) in bindings.into_iter().zip(forms) {
        let component = source.components.iter().find(|c| c.id == id).unwrap();
        let form = component
            .forms
            .iter()
            .find(|f| message_form_id(id, &f.id) == *selected)
            .unwrap();
        let value = if form.shape == "complete" {
            complete_expression(&form.text)
        } else {
            form.text.clone()
        };
        if role == "detail" {
            details.push(value);
        } else {
            values.insert(format!("{role}_clause"), value.trim_end_matches('.').to_owned());
            values.insert(role.into(), value);
        }
    }
    let mut remaining = recipe["text"].as_str().unwrap();
    let mut rendered = String::new();
    while let Some((prefix, rest)) = remaining.split_once('{') {
        let (key, after) = rest.split_once('}').unwrap();
        rendered.push_str(prefix);
        rendered.push_str(&values[key]);
        remaining = after;
    }
    rendered.push_str(remaining);
    for detail in details {
        rendered.push(' ');
        rendered.push_str(&detail);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::english::{Aspect, Clause, Condition, Number, Person, Polarity, Predicate, Subject, Tense, Verb};

    fn sentence(subject: &str, verb: &str) -> Sentence {
        Sentence {
            clause: Clause {
                subject: Subject {
                    text: subject.into(),
                    person: Person::Third,
                    number: Number::Singular,
                },
                predicate: Predicate {
                    verb: Verb::lookup(verb).unwrap(),
                    complement: None,
                },
                tense: Tense::Present,
                aspect: Aspect::Simple,
                modal: None,
                polarity: Polarity::Positive,
                form: Form::Statement,
            },
            condition: None,
        }
    }

    #[test]
    fn explicit_relations_preserve_propositions_and_names() {
        let first = sentence("the guest", "arrive");
        let mut second = sentence("Maya", "leave");
        second.clause.tense = Tense::Past;
        second.clause.polarity = Polarity::Negative;
        let sentences = [first, second];
        let original = sentences.clone();
        for (relation, expected) in [
            (Relation::And, "The guest arrives, and Maya did not leave."),
            (Relation::But, "The guest arrives, but Maya did not leave."),
            (Relation::Or, "The guest arrives, or Maya did not leave."),
            (Relation::Because, "Because Maya did not leave, the guest arrives."),
        ] {
            assert_eq!(combine(&sentences, relation, &RealizeOptions::default()).unwrap(), expected);
        }
        assert_eq!(sentences, original);
        let ids = [sentence("api_v2", "work"), sentence("The Hague", "wait")];
        assert_eq!(
            combine(&ids, Relation::And, &RealizeOptions::default()).unwrap(),
            "api_v2 works, and The Hague waits."
        );
        let mut effect = sentence("the guest", "leave");
        effect.clause.polarity = Polarity::Negative;
        effect.clause.tense = Tense::Past;
        let mut cause = sentence("Maya", "arrive");
        cause.clause.tense = Tense::Past;
        assert_eq!(
            combine(&[effect, cause], Relation::Because, &RealizeOptions::default()).unwrap(),
            "Because Maya arrived, the guest did not leave."
        );
    }

    #[test]
    fn invalid_combination_does_not_silently_change_questions_or_cause() {
        let mut clause = sentence("the guest", "arrive");
        assert!(combine(&[], Relation::And, &RealizeOptions::default()).is_err());
        assert!(combine(&[clause.clone()], Relation::And, &RealizeOptions::default()).is_err());
        assert!(
            combine(
                &[clause.clone(), clause.clone(), clause.clone()],
                Relation::Because,
                &RealizeOptions::default()
            )
            .is_err()
        );
        let mut conditional = clause.clone();
        conditional.condition = Some(Condition {
            subject: clause.clause.subject.clone(),
            predicate: clause.clause.predicate.clone(),
            polarity: Polarity::Positive,
            tense: Tense::Present,
            aspect: Aspect::Simple,
        });
        assert!(combine(&[conditional, clause.clone()], Relation::And, &RealizeOptions::default()).is_err());
        clause.clause.form = Form::Question;
        assert!(combine(&[clause.clone(), clause], Relation::And, &RealizeOptions::default()).is_err());
    }
}
