//! The functions the world provider contributes to expressions.
//!
//! [`DESCRIPTORS`] is what the language checks a call against. [`FUNCTIONS`] is
//! what runs: an evaluator for data generation and one helper that generated
//! agents and scorers reach every function through. The two lists name the same
//! functions in the same order.

use super::english::{self, RealizeOptions, Sentence, VariationOptions};
use crate::mocking::data::Value;
use crate::mocking::{Evaluate, EvaluateError, Evaluation, Function, Helper, Known, Runtime};
use bts_lang::spec::{FUNC_ARG_TYPES_RULE, FUNC_ARITY_RULE, FuncDesc, Id, RuleDesc};
use bts_lang::{FieldType, FunctionDesc, ParamDesc, ResultShape};
use rand::Rng;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value as InputValue;
use std::collections::BTreeMap;

const CALL_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE];

// what `expression` and `expression_candidates` return, so reads into them are typed
const TEXT: ResultShape = ResultShape::Scalar(FieldType::String);
const NUMBER: ResultShape = ResultShape::Scalar(FieldType::Number);
const STRINGS: ResultShape = ResultShape::Array(&TEXT);
const MESSAGE: ResultShape = ResultShape::Object(&[
    ("layout", TEXT),
    ("recipe", TEXT),
    ("forms", STRINGS),
    ("organization", TEXT),
    ("elaboration", TEXT),
    ("sentence_shape", TEXT),
    ("reference_style", TEXT),
]);
const SELECTION: ResultShape =
    ResultShape::Object(&[("intent", TEXT), ("family", TEXT), ("pattern", TEXT), ("message", MESSAGE)]);
const CONTEXT: ResultShape = ResultShape::Object(&[
    ("established", STRINGS),
    ("recent", ResultShape::Array(&SELECTION)),
    ("topic", TEXT),
    ("requested", STRINGS),
    ("known", STRINGS),
    ("offered", STRINGS),
    ("allowed_acts", STRINGS),
]);
const EDIT: ResultShape = ResultShape::Object(&[
    ("rule", TEXT),
    ("replacement", TEXT),
    ("stage", NUMBER),
    ("range", ResultShape::Object(&[("start", NUMBER), ("end", NUMBER)])),
]);
const CANDIDATE: ResultShape = ResultShape::Object(&[
    ("selection", SELECTION),
    ("text", TEXT),
    ("used_facts", STRINGS),
    ("establishes", STRINGS),
    ("probability", NUMBER),
]);
const EXPRESSION: ResultShape = ResultShape::Object(&[
    ("text", TEXT),
    ("base_text", TEXT),
    ("selection", SELECTION),
    ("used_facts", STRINGS),
    ("context", CONTEXT),
    ("edits", ResultShape::Array(&EDIT)),
]);
const CANDIDATES: ResultShape = ResultShape::Array(&CANDIDATE);
// what `conversation` returns, so a read into one of its messages is typed
const MESSAGES: ResultShape = ResultShape::Array(&ResultShape::Object(&[("role", TEXT), ("content", TEXT)]));

/// What the language checks a call to each function against.
pub const DESCRIPTORS: &[FunctionDesc] = &[
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.realize"),
            name: "realize",
            syntax: "realize(sentence[, options])",
            summary: "Renders English without random choices. The sentence object requires clause and accepts optional condition. A clause requires subject {text: string, person: first|second|third (default third), number: singular|plural (default singular)} and predicate {verb, complement?: string}. Verb is a packaged lowercase base word or an object containing all five lowercase forms: base, third_person_singular, past, past_participle, present_participle. Clause options are tense: present|past|future (present), aspect: simple|progressive|perfect|perfect_progressive (simple), polarity: positive|negative (positive), form: statement|question (statement), and modal: can|could|may|might|must|should|will|would (absent by default). A modal requires tense present, with temporal relationships expressed through the chosen modal and aspect. A condition requires subject and predicate and accepts tense, aspect, and polarity with the same defaults. It is introduced by if. Rendering options are negation: expanded|contracted (expanded) and condition_position: before|after (before). Authored subject agreement and explicit verb forms remain caller-owned. A sentence starts with a capital when its first word is made only of lowercase letters, so write an ordinary subject in lowercase and it reads correctly in a statement and in a question. A first word with a capital, a digit or other marks is a name or an identifier and stays as written. Unsupported structures and unknown fields fail during evaluation.",
            examples: &[
                r#"realize({ clause: { subject: { text: "the guest" }, predicate: { verb: "arrive", complement: "after midnight" }, tense: "future" } })"#,
            ],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.express"),
            name: "express",
            syntax: "express(source[, options])",
            summary: "Generates English from author-approved expressions of an intent. source contains intent: nonempty identifier, families: nonempty array, constructions: array (default []), facts: [{id,text}] (default []), optional_details: fact IDs (default []), and protected: literal strings (default []). A construction is {id,predicate:{verb,complement?},weight:1,requires:[],excludes:[],mentions:[]}. Verb accepts the same known base string or explicit five forms as realize. Compatible verbs, objects, particles, and complements stay together as one construction. A family is {id,kind,weight:1,constructions:[],patterns:[],requires:[],excludes:[],context_requires:[],establishes:[],mentions:[],reason:null,text:null}. kind is request, desire, possibility, procedure, need, reason_request, follow_up, or authored. Built-in families use a selected action in request, first-person, or procedural constructions. Empty construction and pattern lists allow all compatible choices. requires and mentions need supplied fact IDs, excludes rejects supplied fact IDs, and context_requires needs established context markers. reason_request needs reason: a supplied fact ID. authored and follow_up need complete text and cannot specify constructions, patterns, or reason. follow_up also requires nonempty context_requires. Author-supplied alternatives are trusted to express the intended meaning. No semantic equivalence or hidden circumstances are inferred. Use render, realize, and ordinary bts choices to prepare domain-specific text and circumstances. options contains speaker, context, and repeat_penalty (default 0.25, range 0..1). speaker contains wording: vary style controls (default neutral), family_weights and pattern_weights: identifier-to-weight objects, greetings and closings: [{text,weight:1}], greeting_probability (0.5), closing_probability (0), detail_probability (0.5), and max_details (1, integer 0..16). All probabilities are in 0..1 and weights in 0..1000000. Profiles are ordinary values that can be sampled once and reused across turns. Greetings apply only with empty history. Optional supplied details are sampled without replacement until max_details or the first failed detail gate, excluding already expressed or established facts. context contains established: identifiers and recent: selection records, both default empty. Family and construction repeats within the same intent are multiplied by repeat_penalty, with zero excluding repeats. Selection normalizes weights by family, construction, and pattern, independent of vocabulary size. Evaluation fails if no positive-weight choice is eligible. No random draw is consumed for a sole choice or probability zero or one. Uses scoped seeded randomness and performs no network calls. For composable messages, source also accepts topic: an explicit topic or entity identifier and components: [{id,role,forms,mentions:[],requires:[],context_requires:[],slot:null,option:null}]. Roles are current, outcome, reason, constraint, answer, previous, replacement, acknowledgment, clarification, follow_up_request, and detail. Each form is {id,text,shape:complete,reference:explicit,weight:1,context_requires:[]}. Shapes are complete, fragments, clause, and value. Clauses and values must omit terminal punctuation and are only used in recipes requiring them. Use realize to prepare grammatical component text. Contextual forms require a topic and nonempty component or form context_requires. Family message is {layout,bindings:{role:component_id},relation:null,allow_apology:false,details:[]}. Layouts are request_only, request_reason, reason_request, outcome_request, current_desired, constraint_request, answer, correction, acknowledgment, clarification, and follow_up_request. current_desired and response layouts require kind structured with no text, reason, constructions, or patterns. Other layouts wrap a regular action family or authored request. reason_request family kind cannot also use message. Message plans cannot use source optional_details. details explicitly binds detail components, with no added facts. Cause links require relation cause on request_reason or reason_request layouts and declarative first-person actions. Contrast requires relation contrast on current_desired. Without those relationships only separate sentences are eligible. Apology variants require allow_apology on correction. Every binding must match the component role. Speaker maps organization (request_first, circumstance_first, outcome_first, current_then_desired, response), elaboration (minimal, relevant_context, detailed), sentence_shape (complete, short_sentences, fragments), and reference_style (explicit, contextual) independently weight applicable choices. Omitted weights are one, zero excludes. Organization and elaboration multiply family weights. Form weights normalize within each recipe and shape and reference preferences weight eligible recipes and forms. Message plans govern fragments and disable the separate surface fragment rewrite. Context additionally accepts topic, known fact IDs, requested slot IDs, offered option IDs, and allowed_acts (answer, correction, acknowledgment, clarification, follow_up_request), all empty or absent by default. Responses require matching topic and an explicitly allowed act. Answers and clarifications require a requested slot. Component option requires an offered option ID. Corrections require previous and replacement components with matching slots and nonempty, disjoint mentions, and previous facts must already be established. Components may use supplied facts or explicitly known facts. Context never derives meaning from agent text. Message expansion is limited to 16384 choices per family.",
            examples: &[
                r#"express({ intent: "change_flight", constructions: [{ id: "change", predicate: { verb: "change", complement: "my flight" } }], families: [{ id: "request", kind: "request" }, { id: "desire", kind: "desire" }] }, { speaker: { wording: { style: "conversational" } } })"#,
            ],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.expression"),
            name: "expression",
            syntax: "expression(source[, options])",
            summary: "Uses the same inputs and random choices as express and returns an object with text, base_text before surface styling, selection: {intent,family,construction,pattern}, used_facts, edits, and context. construction is null for authored and follow_up families. edits contain rule, range with UTF-8 byte start/end offsets relative to the text immediately before that edit, replacement, and stage. The returned context adds explicit family establishes and actually expressed fact IDs, then appends the selection to recent, retaining at most 32 entries. Input context is never mutated. Reuse a speaker value and pass the returned context to later turns explicitly. Nothing stores hidden conversation state. Composable selections also contain message: {layout,recipe,forms,organization,elaboration,sentence_shape,reference_style}, with forms recorded as component_id/form_id and each identifier escaping ~ as ~0 and / as ~1. Topic-scoped outputs add expressed facts to known. Answers consume their requested slot and responses consume allowed_acts. Corrections remove previous facts from known while retaining their historical mentions in established. A changed topic clears pending requested slots, offered options, and allowed acts. The caller supplies the next agent turn context explicitly.",
            examples: &[r#"expression(settings.intent, { speaker: settings.speaker, context: settings.previous.context })"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::Object,
        shape: Some(EXPRESSION),
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.expression_candidates"),
            name: "expression_candidates",
            syntax: "expression_candidates(source[, options])",
            summary: "Validates express inputs and lists applicable base realizations without consuming randomness. Each candidate contains selection, text, used_facts, establishes, and normalized probability. An empty array means no eligible positive-weight choice. Optional greetings, details, closings, and surface edits are not expanded. Pattern IDs can restrict families or configure speaker preferences: request_please, request_could_you, request_would_you, desire_would_like, desire_want, desire_looking_to, possibility_can_i, possibility_possible, possibility_able, procedure_how_do, procedure_how_can, procedure_go_about, need_need_to, reason_request_could_you, reason_request_please, authored, follow_up, and structured. Composable message candidates expand eligible component forms and include message selection metadata. The procedure_go_about pattern uses the supplied verb's present participle. Invalid IDs in a family's patterns or constructions fail. Speaker weight entries for unused IDs are allowed so profiles can be reused across scenarios.",
            examples: &[r#"expression_candidates(settings.intent, { speaker: settings.speaker })"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::Array,
        shape: Some(CANDIDATES),
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.vary"),
            name: "vary",
            syntax: "vary(text_or_sentence[, options[, draws]])",
            summary: "Varies English prose or a sentence object accepted by realize using scoped seeded randomness. Existing calls using only negation: boolean (true), condition_order: boolean (true), and rendering preserve their original behavior: uniform selection between the original and applicable alternatives, with unsupported prose unchanged. Adding any style control opts into bounded composition. style accepts neutral, professional, conversational, warm, terse, text_message, enthusiastic, or informal_chat. Explicit controls override preset defaults: contractions, shorthand, casual, slang, reductions, fragments, warmth, soften_requests, negation, and condition_order are booleans. New-mode negation only contracts recognized negative statements. All presets disable typos and request softening by default. punctuation accepts preserve, omit_period, exclamation, or expressive and affects the final terminal mark, preserving question marks, abbreviations, and code-like tokens. casing accepts preserve or lowercase and only lowercases reviewed everyday words and pronoun contractions. probability (default 1, range 0..1) gates wording, tone, and presentation once per message. max_changes (default 3, integer 0..16) bounds total edits. typos accepts probability (default 0, per message independently), max_errors (default 1, integer 0..16), and kinds (default all of common, omission, transposition, repetition). Typos apply only to reviewed lowercase words and share max_changes. protected is an array of nonempty case-sensitive literal strings. Quotations, code-like tokens, and authored structured subject references are protected automatically. Arbitrary prose names cannot be inferred reliably. alternatives is an array of {from: nonempty text, to: nonempty array of nonempty text} author-approved phrase families, matched literally at word boundaries. Author-supplied alternatives may authorize meaning changes. Built-in tone rules require warmth or soften_requests and never invent promises, urgency, or apologies. Fragments are limited to supported first-person progressive statements. Reductions require recognized infinitive constructions. Changes run in grammatical, phrase, tone, presentation, and typo stages with nonoverlapping choices within each stage. No draw is consumed for a zero or certain probability gate, a sole candidate, or no eligible edit. rendering applies only to structured input. Invalid options fail during evaluation. draws is an optional array of numbers from zero up to one. With it, vary takes its random choices from those numbers in order, in place of the scoped random source, so the result depends on them alone. A walk reads such numbers as context.draws, which are the same in generated data and in a generated agent given the seed.",
            examples: &[
                r#"vary("The guest cannot check in.", { negation: true, condition_order: false })"#,
                r#"vary("I am checking the file. Thank you.", { style: "text_message", max_changes: 6 })"#,
                r#"vary("please send the message", { style: "neutral", typos: { probability: 0.1, max_errors: 1 } })"#,
                r#"vary("Send the file.", { style: "professional", soften_requests: true })"#,
            ],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::String, FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "draws",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.combine"),
            name: "combine",
            syntax: "combine(sentences, relation[, options])",
            summary: "Combines an array of at least two unconditional structured statements accepted by realize. Relation is and, but, or, or because. And, but, and or preserve input order and retain each subject. Because requires exactly two inputs ordered [effect, cause] and renders Because cause, effect. It does not infer causality or substitute one relation for another. Conditional sentences and questions are rejected to avoid changing their scope. Rendering options match realize. Invalid structures or options fail during evaluation.",
            examples: &[r#"combine([settings.arrival, settings.availability], "and")"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "sentences",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "relation",
                accepts: &[FieldType::String],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.refer"),
            name: "refer",
            syntax: "refer(entity, context[, options])",
            summary: "Returns an English name or an explicitly supplied pronoun without mutating context. Entity fields are id and label (required nonempty strings), person: first|second|third (third), number: singular|plural (singular), and optional pronouns {subject, object, possessive_determiner, possessive}, all four single-word strings. Context is {entities: entity array, mentioned: entity-id string array, facts: fact-id string array}, with every array defaulting to empty. Entity definitions and ids must agree, mentioned ids must have definitions, and context ids must be unique. Options are form: subject|object|possessive_determiner|possessive (subject) and style: auto|name|pronoun (auto). Auto uses a supplied pronoun only after introduction and when unambiguous among mentioned entities, otherwise the label. Name always uses the label. Pronoun requires introduction and an unambiguous supplied form or fails. No gender or entity identity is inferred.",
            examples: &[r#"refer(settings.guest, settings.context)"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "entity",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "context",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.remember"),
            name: "remember",
            syntax: "remember(context, entities[, facts])",
            summary: "Returns a new context object with entities introduced and facts established. Context fields are entities (array of entities accepted by refer), mentioned (array of entity-id strings), and facts (array of fact-id strings), each defaulting to empty. Entities is an array of entity objects. The optional facts argument is an array of nonempty fact-id strings and defaults to []. Existing definitions are retained, identical entities and repeated facts are deduplicated, and conflicting definitions for one id fail. The input context is unchanged. Carry the returned value explicitly across turns. Starting from {} resets the conversation scope. Unknown fields and invalid context references fail during evaluation.",
            examples: &[r#"remember({}, [settings.guest], ["arrival_time"])"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "context",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "entities",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "facts",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::Object,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.unmentioned"),
            name: "unmentioned",
            syntax: "unmentioned(details, context)",
            summary: "Returns an array of detail objects whose id is absent from context.facts, preserving source order and text. Each detail is exactly {id: string, text: string}, both nonempty and without surrounding whitespace or control characters. Context has the entities, mentioned, and facts arrays documented by refer, each defaulting to empty. Selection is deterministic and never changes context. Identical repeated details are deduplicated. Conflicting text for one id fails even if that fact was already established. Use remember to mark details established only after they have been communicated.",
            examples: &[r#"unmentioned([{ id: "arrival_time", text: "Arrival is after midnight." }], settings.context)"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "details",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "context",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::Array,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.format_number"),
            name: "format_number",
            syntax: "format_number(number_or_decimal_text[, options])",
            summary: "Formats a number or exact decimal string using a packaged English regional profile. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US), grouping: boolean (true), and fraction_digits: integral number from zero through nine, including 1.0 (omitted by default). Explicit precision rounds decimal ties away from zero and pads trailing zeros. Without precision, trailing fractional zeros are trimmed. All profiles use a decimal point and comma grouping. En-IN groups the final three integer digits then pairs, while other profiles use groups of three. Negative zero becomes zero. Decimal strings may contain an exponent and avoid floating-point input ambiguity. Expanded input is bounded to 4096 digits. Invalid values, unknown fields, or unsupported locales fail during evaluation. No OS locale or external data is consulted.",
            examples: &[r#"format_number("1234567.50", { locale: "en-IN" })"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::String, FieldType::Number],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.format_date"),
            name: "format_date",
            syntax: "format_date(date[, options])",
            summary: "Formats a strict YYYY-MM-DD Gregorian date with year 0001 through 9999. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US) and style: short|long (long). Short output uses MM/DD/YYYY for en-US, YYYY-MM-DD for en-CA, and DD/MM/YYYY otherwise. Long output uses English month names, month-first for en-US and en-CA, and day-first otherwise. Invalid calendar dates, unknown fields, or unsupported locales fail during evaluation. No clock or OS locale is consulted.",
            examples: &[r#"format_date("2026-11-10", { locale: "en-GB", style: "long" })"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::String],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.format_time"),
            name: "format_time",
            syntax: "format_time(time[, options])",
            summary: "Formats a strict HH:MM or HH:MM:SS local clock value. Hours range from 00 through 23 and minutes and seconds from 00 through 59. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US), hour_cycle: h12|h24 (omitted), and seconds: boolean (false). Omitted hour_cycle selects h24 for en-GB and h12 otherwise. H12 includes AM or PM. Seconds are displayed only when requested, with missing seconds treated as 00. No current time, OS locale, or timezone conversion is implied. Invalid times, unknown fields, or unsupported locales fail during evaluation.",
            examples: &[r#"format_time("17:30", { locale: "en-US" })"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::String],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "options",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: true,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.merge"),
            name: "merge",
            syntax: "merge(base, objects)",
            summary: "Makes one object from many. It starts from base and applies each object in order. A later key replaces an earlier one, and a null value or a null item changes nothing, so an object sets only what it states. Keys are merged one level deep. A walk uses it to add up what its steps give: merge(defaults, context.steps). Performs no random draws.",
            examples: &[r#"merge({ tense: "present", modal: null }, [{ modal: "can" }, { tense: null }])"#],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "base",
                accepts: &[FieldType::Object],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "objects",
                accepts: &[FieldType::Array],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
        ],
        returns: FieldType::Object,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.conversation"),
            name: "conversation",
            syntax: "conversation({user, turn[, system, assistant, between]})",
            summary: "Makes the chat messages of one turn of a conversation, for the input of an llm step. user is the list of what the user says, one string for each turn, and turn is a whole number that counts them from zero. The result is the user's message at turn, after the messages of every turn before it. assistant is the list of what the assistant replied, and when it is given each earlier user message is followed by the reply of its turn, so it needs a reply for every turn before this one. system is a string that becomes a system message at the start. between is a list with one entry for each turn, the messages that passed between the user's message of that turn and the reply to it, such as the assistant's tool call and the tool's result. When it is given, the entry of each earlier turn is placed after that turn's user message and before its reply, as written, so it needs an entry for every turn before this one. An entry that is null or an empty list adds nothing. A null system, assistant or between is left out. Each message is {role, content}, and a message from between is whatever was written. The lists may be of any length, which a comprehension cannot take. Performs no random draws.",
            examples: &[
                r#"conversation({ system: policies.text, user: customer.said, assistant: helper.said, turn: chat.turn })"#,
                r#"conversation({ user: customer.said, assistant: helper.said, between: lookups.items, turn: chat.turn })"#,
            ],
            rules: CALL_RULES,
        },
        params: &[ParamDesc {
            name: "said",
            accepts: &[FieldType::Object],
            nullable: false,
            optional: false,
            constant: false,
            bounds: None,
        }],
        returns: FieldType::Array,
        shape: Some(MESSAGES),
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.paragraph"),
            name: "paragraph",
            syntax: "paragraph(sentences)",
            summary: "Joins sentences into one message, with a space between them. A sentence that another follows and that does not end in a full stop, a question mark or an exclamation mark is given a full stop, so sentences written without one, as in a text message, do not run together. The last sentence is left as it is. Null and empty strings are skipped. A list uses it for its text. Performs no random draws.",
            examples: &[r#"paragraph(["someone hit my car", "i need to file a claim"])"#],
            rules: CALL_RULES,
        },
        params: &[ParamDesc {
            name: "sentences",
            accepts: &[FieldType::Array],
            nullable: false,
            optional: false,
            constant: false,
            bounds: None,
        }],
        returns: FieldType::String,
        shape: None,
    },
];

// every function runs in data generation, in agents and in scorers, through one entry point
// `draws` says when a call takes its choices from the random stream of the run
const fn function(name: &'static str, data: Evaluate, texts: bool, draws: fn(&[Known]) -> bool) -> Function {
    let helper = Helper {
        name: "_english",
        // generated agents read it as an ordinary library function
        reads_as: Some("english"),
        random: true,
        seeded: false,
        counts: false,
        texts,
        plain: false,
        // every function checks the shape of what it is given, so any call may raise
        sure: None,
        returns: None,
        draws: Some(draws),
        text: "",
        operation: Some(name),
        runtime: Some(Runtime {
            name: "english",
            source: super::runtime::source,
        }),
        scorer: Some("_language_english"),
    };
    Function {
        name,
        check: None,
        data: Some(data),
        python: Some(helper),
        typescript: Some(helper),
    }
}

/// What runs each function, for data generation and for generated code.
///
/// The flag says whether the function returns text, as its descriptor declares.
pub static FUNCTIONS: &[Function] = &[
    function("realize", realize, true, never),
    function("express", express, true, always),
    function("expression", expression, false, always),
    function("expression_candidates", expression_candidates, false, never),
    function("vary", vary, true, unless_handed_draws),
    function("combine", combine, true, never),
    function("refer", refer, true, never),
    function("remember", remember, false, never),
    function("unmentioned", unmentioned, false, never),
    function("format_number", format_number, true, never),
    function("format_date", format_date, true, never),
    function("format_time", format_time, true, never),
    Function {
        name: "merge",
        check: None,
        data: Some(super::walk::merge),
        python: Some(walked("_world_merge", "merge_objects", super::walk::PY_MERGE).known(merges, merged)),
        typescript: Some(walked("_world_merge", "mergeObjects", super::walk::TS_MERGE).known(merges, merged)),
    },
    Function {
        name: "conversation",
        check: None,
        data: Some(conversation),
        python: Some(walked("_world_conversation", "chat_messages", PY_CONVERSATION)),
        typescript: Some(walked("_world_conversation", "chatMessages", TS_CONVERSATION)),
    },
    Function {
        name: "paragraph",
        check: None,
        data: Some(paragraph),
        python: Some(walked("_world_paragraph", "join_sentences", PY_PARAGRAPH).known(joins, joined)),
        typescript: Some(walked("_world_paragraph", "joinSentences", TS_PARAGRAPH).known(joins, joined)),
    },
    // what a weighted fork in a walk lowers to, which no author calls
    Function {
        name: super::walk::PICK,
        check: None,
        data: Some(super::walk::pick),
        python: Some(seeded("_world_pick", "take_branch").known(super::walk::picks, super::walk::picked)),
        typescript: Some(seeded("_world_pick", "takeBranch").known(super::walk::picks, super::walk::picked)),
    },
    // what the place of a trace among series lowers to
    Function {
        name: super::walk::TURN,
        check: None,
        data: Some(super::walk::turn),
        python: Some(seeded("_world_turn", "series_turn").known(super::walk::places, super::walk::placed)),
        typescript: Some(seeded("_world_turn", "seriesTurn").known(super::walk::places, super::walk::placed)),
    },
    // what the draws of a walk lower to
    Function {
        name: super::walk::DRAWS,
        check: None,
        data: Some(super::walk::drawn),
        python: Some(seeded("_world_draws", "draws_for").known(super::walk::draws_fit, super::walk::draws_given)),
        typescript: Some(seeded("_world_draws", "drawsFor").known(super::walk::draws_fit, super::walk::draws_given)),
    },
    // what the identifier of a series lowers to
    Function {
        name: super::walk::ID,
        check: None,
        data: Some(super::walk::id),
        python: Some(seeded("_world_id", "series_id").known(super::walk::names, super::walk::named)),
        typescript: Some(seeded("_world_id", "seriesId").known(super::walk::names, super::walk::named)),
    },
];

// a function that makes no choice
fn never(_: &[Known]) -> bool {
    false
}

// a function that chooses among what it may say
fn always(_: &[Known]) -> bool {
    true
}

// `vary` takes its choices from the draws it is handed, and from the random stream without them
fn unless_handed_draws(args: &[Known]) -> bool {
    args.len() < 3
}

// draws handed to `vary`, played back as its random source
struct Replay<'a> {
    draws: &'a [f64],
    at: usize,
}

impl rand::RngCore for Replay<'_> {
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    // the 53 bits of the draw, where a float is read from
    fn next_u64(&mut self) -> u64 {
        let draw = self.draws.get(self.at).copied().unwrap_or(0.5);
        self.at += 1;
        ((draw * 9_007_199_254_740_992.0) as u64) << 11
    }

    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let next = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&next[..chunk.len()]);
        }
    }
}

// a helper that draws by the seed of the run, defined in the text those helpers share
const fn seeded(name: &'static str, reads_as: &'static str) -> Helper {
    Helper {
        name,
        reads_as: Some(reads_as),
        random: true,
        seeded: true,
        counts: false,
        texts: false,
        plain: true,
        sure: None,
        returns: None,
        // what it draws depends on the seed alone, so a second call draws the same
        draws: Some(never),
        text: "",
        operation: None,
        runtime: Some(Runtime {
            name: "world",
            source: super::walk::runtime,
        }),
        scorer: None,
    }
}

// a helper a walk uses, defined by its own text
const fn walked(name: &'static str, reads_as: &'static str, text: &'static str) -> Helper {
    Helper {
        name,
        reads_as: Some(reads_as),
        random: false,
        seeded: false,
        counts: false,
        texts: false,
        plain: true,
        sure: None,
        returns: None,
        draws: None,
        text,
        operation: None,
        runtime: None,
        scorer: None,
    }
}

// an object, or nothing where an object may be left out
fn object_or_null(item: &Known) -> bool {
    match item {
        Known::Object(_) | Known::Null => true,
        Known::Maybe(item) => matches!(**item, Known::Object(_)),
        Known::Run(item) => object_or_null(item),
        _ => false,
    }
}

// `merge` raises unless it starts from an object and every item is an object or null
fn merges(args: &[Known]) -> bool {
    match args {
        [Known::Object(_), Known::Items(items)] => items.iter().all(object_or_null),
        [Known::Object(_), Known::List(item)] => object_or_null(item),
        _ => false,
    }
}

// the keys `merge` is sure to give, since a later item replaces a key only where it states one
fn merged(args: &[Known]) -> Known {
    let [Known::Object(base), objects] = args else {
        return Known::Plain;
    };
    let mut merged = base.clone();
    // an item that is always there sets what it states, one that may be missing only widens a key
    let mut lay = |item: &Known, always: bool| {
        let (fields, always) = match item {
            Known::Object(fields) => (fields, always),
            Known::Maybe(item) | Known::Run(item) => match &**item {
                Known::Object(fields) => (fields, false),
                Known::Null => return true,
                Known::Maybe(item) => match &**item {
                    Known::Object(fields) => (fields, false),
                    _ => return false,
                },
                _ => return false,
            },
            Known::Null => return true,
            _ => return false,
        };
        for (key, value) in fields {
            let stated = match value {
                Known::Null => continue,
                Known::Maybe(value) => match merged.remove(key) {
                    Some(kept) => kept.join((**value).clone()),
                    None => continue,
                },
                value if always => value.clone(),
                value => match merged.remove(key) {
                    Some(kept) => kept.join(value.clone()),
                    None => continue,
                },
            };
            merged.insert(key.clone(), stated);
        }
        true
    };
    let fits = match objects {
        Known::Items(items) => items.iter().all(|item| lay(item, true)),
        Known::List(item) => lay(item, false),
        _ => false,
    };
    if fits { Known::Object(merged) } else { Known::Plain }
}

// `paragraph` raises unless every sentence is a string or null
fn joins(args: &[Known]) -> bool {
    let sentence = |item: &Known| match item {
        Known::Text(_) | Known::Null => true,
        Known::Maybe(item) | Known::Run(item) => match &**item {
            Known::Text(_) | Known::Null => true,
            Known::Maybe(item) => matches!(**item, Known::Text(_)),
            _ => false,
        },
        _ => false,
    };
    match args {
        [Known::Items(items)] => items.iter().all(sentence),
        [Known::List(item)] => sentence(item),
        _ => false,
    }
}

fn joined(_: &[Known]) -> Known {
    Known::Text(None)
}

const CONVERSATION_KEYS: &str =
    "conversation: takes an object with user and turn, and optionally system, assistant and between";
const CONVERSATION_TURN: &str = "conversation: turn is a whole number that counts the user's messages from zero";
const CONVERSATION_REPLIES: &str = "conversation: assistant has a reply for every turn before this one";
const CONVERSATION_BETWEEN: &str =
    "conversation: between has an entry for every turn before this one, each a list of messages or null";

/// Evaluates `conversation`: the messages of a chat up to the user's message at one turn.
fn conversation(args: &[Value], _: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    fn unfit(what: &'static str) -> EvaluateError {
        EvaluateError::Invalid(what.into())
    }
    fn texts<'a>(value: &'a Value, shape: &'static str) -> Result<Vec<&'a String>, EvaluateError> {
        let Value::Array(items) = value else {
            return Err(unfit(shape));
        };
        (items.iter())
            .map(|item| match item {
                Value::String(text) => Ok(text),
                _ => Err(unfit(shape)),
            })
            .collect()
    }
    fn message(role: &str, content: &str) -> Value {
        Value::Object(BTreeMap::from([
            ("role".to_owned(), Value::String(role.to_owned())),
            ("content".to_owned(), Value::String(content.to_owned())),
        ]))
    }
    let [Value::Object(said)] = args else {
        return Err(unfit(CONVERSATION_KEYS));
    };
    if said
        .keys()
        .any(|key| !["system", "user", "assistant", "between", "turn"].contains(&key.as_str()))
    {
        return Err(unfit(CONVERSATION_KEYS));
    }
    let user = texts(
        said.get("user").ok_or_else(|| unfit(CONVERSATION_KEYS))?,
        "conversation: user is a list of strings",
    )?;
    let assistant = match said.get("assistant") {
        None | Some(Value::Null) => None,
        Some(replies) => Some(texts(replies, "conversation: assistant is a list of strings")?),
    };
    let turn = match said.get("turn") {
        Some(Value::Number(turn)) => turn.as_u64().and_then(|turn| usize::try_from(turn).ok()),
        Some(_) => None,
        None => return Err(unfit(CONVERSATION_KEYS)),
    };
    let turn = turn
        .filter(|turn| *turn < user.len())
        .ok_or_else(|| unfit(CONVERSATION_TURN))?;
    if assistant.as_ref().is_some_and(|replies| replies.len() < turn) {
        return Err(unfit(CONVERSATION_REPLIES));
    }
    // what passed between a user's message and the reply to it, one entry for each turn
    let between = match said.get("between") {
        None | Some(Value::Null) => None,
        Some(Value::Array(entries)) if entries.len() >= turn => {
            let entries = (entries.iter())
                .map(|entry| match entry {
                    Value::Null => Ok(&[][..]),
                    Value::Array(passed) if passed.iter().all(|message| matches!(message, Value::Object(_))) => {
                        Ok(passed.as_slice())
                    }
                    _ => Err(unfit(CONVERSATION_BETWEEN)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Some(entries)
        }
        Some(_) => return Err(unfit(CONVERSATION_BETWEEN)),
    };
    let mut messages = Vec::new();
    match said.get("system") {
        None | Some(Value::Null) => {}
        Some(Value::String(system)) => messages.push(message("system", system)),
        Some(_) => return Err(unfit("conversation: system is a string")),
    }
    for earlier in 0..turn {
        messages.push(message("user", user[earlier]));
        if let Some(entries) = &between {
            messages.extend(entries[earlier].iter().cloned());
        }
        if let Some(replies) = &assistant {
            messages.push(message("assistant", replies[earlier]));
        }
    }
    messages.push(message("user", user[turn]));
    Ok(Value::Array(messages))
}

/// Evaluates `paragraph`: sentences joined so that none runs into the next.
fn paragraph(args: &[Value], _: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::Array(items)] = args else {
        return Err(EvaluateError::Invalid("paragraph: takes a list of strings".into()));
    };
    let mut sentences = Vec::new();
    for item in items {
        match item {
            Value::Null => {}
            Value::String(sentence) if sentence.is_empty() => {}
            Value::String(sentence) => sentences.push(sentence.as_str()),
            _ => return Err(EvaluateError::Invalid("paragraph: takes a list of strings".into())),
        }
    }
    let mut joined = String::new();
    for (index, sentence) in sentences.iter().enumerate() {
        joined.push_str(sentence);
        if index + 1 < sentences.len() {
            if !sentence.trim_end().ends_with(['.', '?', '!']) {
                joined.push('.');
            }
            joined.push(' ');
        }
    }
    Ok(Value::String(joined))
}

// the Python text of `paragraph`.
const PY_PARAGRAPH: &str = r#"
def _world_paragraph(sentences):
    sentences = _value(sentences)
    if not isinstance(sentences, list) or not all(item is None or isinstance(item, str) for item in sentences):
        raise TypeError("paragraph: takes a list of strings")
    sentences = [item for item in sentences if item]
    joined = ""
    for index, sentence in enumerate(sentences):
        joined += sentence
        if index + 1 < len(sentences):
            if not sentence.rstrip().endswith((".", "?", "!")): joined += "."
            joined += " "
    return joined
"#;

// the TypeScript text of `paragraph`.
const TS_PARAGRAPH: &str = r#"
function _world_paragraph(sentences: unknown): string {
    const given = _value(sentences);
    if (!Array.isArray(given) || !given.every((item) => item === null || typeof item === "string")) {
        throw new TypeError("paragraph: takes a list of strings");
    }
    const kept: string[] = given.filter((item) => item !== null && item !== "");
    return kept.map((sentence, index) => index + 1 < kept.length && !/[.?!]$/.test(sentence.trimEnd()) ? sentence + "." : sentence).join(" ");
}
"#;

// the Python text of `conversation`.
const PY_CONVERSATION: &str = r#"
def _world_conversation(said):
    said = _value(said)
    keys = "conversation: takes an object with user and turn, and optionally system, assistant and between"
    if not isinstance(said, dict) or any(key not in ("system", "user", "assistant", "between", "turn") for key in said): raise TypeError(keys)
    if "user" not in said or "turn" not in said: raise TypeError(keys)
    def texts(key):
        items = said[key]
        if not isinstance(items, list) or not all(isinstance(item, str) for item in items):
            raise TypeError(f"conversation: {key} is a list of strings")
        return items
    user = texts("user")
    assistant = texts("assistant") if said.get("assistant") is not None else None
    turn = said["turn"]
    if not isinstance(turn, int) or isinstance(turn, bool) or not 0 <= turn < len(user):
        raise ValueError("conversation: turn is a whole number that counts the user's messages from zero")
    if assistant is not None and len(assistant) < turn:
        raise ValueError("conversation: assistant has a reply for every turn before this one")
    between = said.get("between")
    if between is not None:
        fits = isinstance(between, list) and len(between) >= turn and all(
            entry is None or (isinstance(entry, list) and all(isinstance(message, dict) for message in entry))
            for entry in between
        )
        if not fits:
            raise ValueError("conversation: between has an entry for every turn before this one, each a list of messages or null")
    messages = []
    system = said.get("system")
    if system is not None:
        if not isinstance(system, str): raise TypeError("conversation: system is a string")
        messages.append({"role": "system", "content": system})
    for earlier in range(turn):
        messages.append({"role": "user", "content": user[earlier]})
        if between is not None: messages.extend(between[earlier] or [])
        if assistant is not None: messages.append({"role": "assistant", "content": assistant[earlier]})
    messages.append({"role": "user", "content": user[turn]})
    return messages
"#;

// the TypeScript text of `conversation`.
const TS_CONVERSATION: &str = r#"
function _world_conversation(said: unknown): { role: string; content: unknown; [key: string]: unknown }[] {
    const given = _value(said);
    const keys = "conversation: takes an object with user and turn, and optionally system, assistant and between";
    if (given === null || typeof given !== "object" || Array.isArray(given)) throw new TypeError(keys);
    if (Object.keys(given).some((key) => !["system", "user", "assistant", "between", "turn"].includes(key))) throw new TypeError(keys);
    if (!("user" in given) || !("turn" in given)) throw new TypeError(keys);
    const texts = (key: string): string[] => {
        const items = given[key];
        if (!Array.isArray(items) || !items.every((item) => typeof item === "string")) {
            throw new TypeError(`conversation: ${key} is a list of strings`);
        }
        return items;
    };
    const user = texts("user");
    const assistant = given.assistant === null || given.assistant === undefined ? null : texts("assistant");
    const turn = given.turn;
    if (typeof turn !== "number" || !Number.isInteger(turn) || turn < 0 || turn >= user.length) {
        throw new RangeError("conversation: turn is a whole number that counts the user's messages from zero");
    }
    if (assistant !== null && assistant.length < turn) {
        throw new RangeError("conversation: assistant has a reply for every turn before this one");
    }
    const between = given.between === null || given.between === undefined ? null : given.between;
    const passed = (entry: unknown): entry is { role: string; content: unknown }[] =>
        Array.isArray(entry) && entry.every((message) => message !== null && typeof message === "object" && !Array.isArray(message));
    if (between !== null && !(Array.isArray(between) && between.length >= turn && between.every((entry) => entry === null || passed(entry)))) {
        throw new RangeError("conversation: between has an entry for every turn before this one, each a list of messages or null");
    }
    const messages: { role: string; content: unknown; [key: string]: unknown }[] = [];
    const system = given.system;
    if (system !== null && system !== undefined) {
        if (typeof system !== "string") throw new TypeError("conversation: system is a string");
        messages.push({ role: "system", content: system });
    }
    for (let earlier = 0; earlier < turn; earlier++) {
        messages.push({ role: "user", content: user[earlier] });
        if (between !== null && between[earlier] !== null) messages.push(...between[earlier]);
        if (assistant !== null) messages.push({ role: "assistant", content: assistant[earlier] });
    }
    messages.push({ role: "user", content: user[turn] });
    return messages;
}
"#;

#[derive(Clone, Copy)]
enum Operation {
    Realize,
    Express,
    Expression,
    ExpressionCandidates,
    Vary,
    Combine,
    Refer,
    Remember,
    Unmentioned,
    FormatNumber,
    FormatDate,
    FormatTime,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Realize => "realize",
            Self::Express => "express",
            Self::Expression => "expression",
            Self::ExpressionCandidates => "expression_candidates",
            Self::Vary => "vary",
            Self::Combine => "combine",
            Self::Refer => "refer",
            Self::Remember => "remember",
            Self::Unmentioned => "unmentioned",
            Self::FormatNumber => "format_number",
            Self::FormatDate => "format_date",
            Self::FormatTime => "format_time",
        }
    }
}

// one evaluator for each function, since a function is named by what runs it
macro_rules! evaluators {
    ($($name:ident => $operation:ident),* $(,)?) => {
        $(fn $name(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
            evaluate(Operation::$operation, args, evaluation)
        })*
    };
}

evaluators! {
    realize => Realize,
    express => Express,
    expression => Expression,
    expression_candidates => ExpressionCandidates,
    vary => Vary,
    combine => Combine,
    refer => Refer,
    remember => Remember,
    unmentioned => Unmentioned,
    format_number => FormatNumber,
    format_date => FormatDate,
    format_time => FormatTime,
}

fn evaluate(operation: Operation, args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let args = (args.iter())
        .map(|arg| {
            (evaluation.plain(arg)).map_err(|_| EvaluateError::Invalid("English functions do not accept attachments".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // vary handed its draws takes its choices from them, not from the random source
    let result = match (operation, args.as_slice()) {
        (Operation::Vary, [text, options, InputValue::Array(draws)]) => {
            let draws = (draws.iter())
                .map(|draw| draw.as_f64().filter(|draw| (0.0..1.0).contains(draw)))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| EvaluateError::Invalid("vary: draws are numbers from zero up to one".into()))?;
            run_english(
                operation,
                &[text.clone(), options.clone()],
                &mut Replay { draws: &draws, at: 0 },
            )
        }
        (Operation::Vary, [_, _, _]) => Err("draws are an array of numbers".to_owned()),
        _ => run_english(operation, &args, evaluation.random()),
    };
    result
        .map(Value::from)
        .map_err(|error| EvaluateError::Invalid(format!("{}: {error}", operation.name()).into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnglishVaryOptions {
    #[serde(default = "english_enabled")]
    negation: bool,
    #[serde(default = "english_enabled")]
    condition_order: bool,
    #[serde(default)]
    rendering: RealizeOptions,
}

fn english_enabled() -> bool {
    true
}

impl Default for EnglishVaryOptions {
    fn default() -> Self {
        Self {
            negation: true,
            condition_order: true,
            rendering: RealizeOptions::default(),
        }
    }
}

fn run_english<R: Rng + ?Sized>(operation: Operation, args: &[InputValue], rng: &mut R) -> Result<InputValue, String> {
    use Operation::*;
    let (min, max) = match operation {
        Express | Expression | ExpressionCandidates | Realize | Vary | FormatNumber | FormatDate | FormatTime => (1, 2),
        Combine | Refer | Remember => (2, 3),
        Unmentioned => (2, 2),
    };
    if !(min..=max).contains(&args.len()) {
        return Err(format!("expected {min} to {max} arguments"));
    }
    match operation {
        Express | Expression | ExpressionCandidates => {
            english_expression_shapes(&args[0], args.get(1))?;
            let source = decode_english(&args[0])?;
            let english_options = english_options(args.get(1))?;
            match operation {
                Express => english::express(&source, &english_options, rng)
                    .map(InputValue::String)
                    .map_err(english_error_message),
                Expression => {
                    serde_json::to_value(english::expression(&source, &english_options, rng).map_err(english_error_message)?)
                        .map_err(english_error_message)
                }
                _ => serde_json::to_value(
                    english::expression_candidates(&source, &english_options).map_err(english_error_message)?,
                )
                .map_err(english_error_message),
            }
        }
        Realize => {
            let sentence: Sentence = decode_english(&args[0])?;
            let english_options = english_options(args.get(1))?;
            english::realize(&sentence, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
        Vary => {
            if args.get(1).and_then(InputValue::as_object).is_some_and(|english_options| {
                english_options
                    .keys()
                    .any(|key| !["negation", "condition_order", "rendering"].contains(&key.as_str()))
            }) {
                let english_options: english::StyleOptions = english_options(args.get(1))?;
                let result = if let Some(text) = args[0].as_str() {
                    english::vary_styled(text, &english_options, rng)
                } else {
                    english::vary_sentence_styled(&decode_english(&args[0])?, &english_options, rng)
                };
                return result
                    .map(|result| InputValue::String(result.text))
                    .map_err(english_error_message);
            }
            let english_options: EnglishVaryOptions = english_options(args.get(1))?;
            let variation = VariationOptions {
                negation: english_options.negation,
                condition_order: english_options.condition_order,
            };
            let output = if let Some(text) = args[0].as_str() {
                english::vary(text, &variation, rng)
            } else {
                let sentence: Sentence = decode_english(&args[0])?;
                english::vary_sentence(&sentence, &english_options.rendering, &variation, rng)
            };
            output.map(InputValue::String).map_err(english_error_message)
        }
        Combine => {
            let sentences: Vec<Sentence> = decode_english(&args[0])?;
            let relation = decode_english(&args[1])?;
            let english_options = english_options(args.get(2))?;
            english::composition::combine(&sentences, relation, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
        Refer => {
            let entity = decode_english(&args[0])?;
            let context = decode_english(&args[1])?;
            let english_options = english_options(args.get(2))?;
            english::discourse::refer(&entity, &context, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
        Remember => {
            let context = decode_english(&args[0])?;
            let entities: Vec<english::discourse::Entity> = decode_english(&args[1])?;
            let facts: Vec<String> = english_options(args.get(2))?;
            let context = english::discourse::remember(&context, &entities, &facts).map_err(english_error_message)?;
            serde_json::to_value(context).map_err(english_error_message)
        }
        Unmentioned => {
            let details: Vec<english::discourse::Detail> = decode_english(&args[0])?;
            let context = decode_english(&args[1])?;
            let selected = english::discourse::unmentioned(&details, &context).map_err(english_error_message)?;
            serde_json::to_value(selected).map_err(english_error_message)
        }
        FormatNumber => {
            let number = match &args[0] {
                InputValue::String(value) => value.clone(),
                InputValue::Number(number) => number.to_string(),
                _ => return Err("expected a number or decimal string".into()),
            };
            let english_options = english_options(args.get(1))?;
            english::formatting::format_number(&number, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
        FormatDate => {
            let date: String = decode_english(&args[0])?;
            let english_options = english_options(args.get(1))?;
            english::formatting::format_date(&date, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
        FormatTime => {
            let time: String = decode_english(&args[0])?;
            let english_options = english_options(args.get(1))?;
            english::formatting::format_time(&time, &english_options)
                .map(InputValue::String)
                .map_err(english_error_message)
        }
    }
}

fn decode_english<T: DeserializeOwned>(value: &InputValue) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(english_error_message)
}

fn english_options<T: DeserializeOwned + Default>(value: Option<&InputValue>) -> Result<T, String> {
    value.map(decode_english).unwrap_or_else(|| Ok(T::default()))
}

fn english_error_message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

// Serde also accepts struct sequences. The DSL contract requires named objects.
fn english_expression_shapes(source: &InputValue, english_options: Option<&InputValue>) -> Result<(), String> {
    fn object(value: &InputValue) -> Result<(), String> {
        if value.is_object() {
            Ok(())
        } else {
            Err("expected an expression object".into())
        }
    }
    fn objects(value: Option<&InputValue>) -> Result<(), String> {
        if let Some(InputValue::Array(values)) = value {
            for value in values {
                object(value)?;
            }
        }
        Ok(())
    }
    object(source)?;
    for key in ["families", "facts", "constructions", "components"] {
        objects(source.get(key))?;
    }
    if let Some(InputValue::Array(values)) = source.get("components") {
        for value in values {
            objects(value.get("forms"))?;
        }
    }
    if let Some(InputValue::Array(values)) = source.get("families") {
        for value in values {
            if let Some(message) = value.get("message").filter(|v| !v.is_null()) {
                object(message)?;
                if let Some(bindings) = message.get("bindings") {
                    object(bindings)?;
                }
            }
        }
    }
    if let Some(InputValue::Array(values)) = source.get("constructions") {
        for value in values {
            if let Some(predicate) = value.get("predicate") {
                object(predicate)?;
                if let Some(verb) = predicate.get("verb")
                    && !verb.is_string()
                {
                    object(verb)?;
                }
            }
        }
    }
    if let Some(english_options) = english_options {
        object(english_options)?;
        if let Some(context) = english_options.get("context") {
            object(context)?;
            objects(context.get("recent"))?;
            if let Some(InputValue::Array(recent)) = context.get("recent") {
                for selection in recent {
                    if let Some(message) = selection.get("message").filter(|v| !v.is_null()) {
                        object(message)?;
                    }
                }
            }
        }
        if let Some(speaker) = english_options.get("speaker") {
            object(speaker)?;
            for key in ["greetings", "closings"] {
                objects(speaker.get(key))?;
            }
            if let Some(wording) = speaker.get("wording") {
                object(wording)?;
                for key in ["typos", "rendering"] {
                    if let Some(value) = wording.get(key) {
                        object(value)?;
                    }
                }
                objects(wording.get("alternatives"))?;
            }
        }
    }
    Ok(())
}
