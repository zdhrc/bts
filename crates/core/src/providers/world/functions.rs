//! The functions the world provider contributes to expressions.
//!
//! [`DESCRIPTORS`] is what the language checks a call against. [`FUNCTIONS`] is
//! what runs: an evaluator for data generation and one helper that generated
//! agents and scorers reach every function through. The two lists name the same
//! functions in the same order.

use super::english::{self, RealizeOptions, Sentence, VariationOptions};
use crate::mocking::data::Value;
use crate::mocking::{Evaluate, EvaluateError, Evaluation, Function, Helper, Runtime};
use bts_lang::spec::{FUNC_ARG_TYPES_RULE, FUNC_ARITY_RULE, FuncDesc, Id, RuleDesc};
use bts_lang::{FieldType, FunctionDesc, ParamDesc, ResultShape};
use rand::Rng;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value as InputValue;

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

/// What the language checks a call to each function against.
pub const DESCRIPTORS: &[FunctionDesc] = &[
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.realize"),
            name: "realize",
            syntax: "realize(sentence[, options])",
            summary: "Renders English without random choices. The sentence object requires clause and accepts optional condition. A clause requires subject {text: string, person: first|second|third (default third), number: singular|plural (default singular)} and predicate {verb, complement?: string}. Verb is a packaged lowercase base word or an object containing all five lowercase forms: base, third_person_singular, past, past_participle, present_participle. Clause options are tense: present|past|future (present), aspect: simple|progressive|perfect|perfect_progressive (simple), polarity: positive|negative (positive), form: statement|question (statement), and modal: can|could|may|might|must|should|will|would (absent by default). A modal requires tense present, with temporal relationships expressed through the chosen modal and aspect. A condition requires subject and predicate and accepts tense, aspect, and polarity with the same defaults. It is introduced by if. Rendering options are negation: expanded|contracted (expanded) and condition_position: before|after (before). Authored subject agreement and explicit verb forms remain caller-owned. Unsupported structures and unknown fields fail during evaluation.",
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
                r#"express({ intent: "change_flight", constructions: [|{ id: "change", predicate: { verb: "change", complement: "my flight" } }|], families: [|{ id: "request", kind: "request" }, { id: "desire", kind: "desire" }|] }, { speaker: { wording: { style: "conversational" } } })"#,
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
            syntax: "vary(text_or_sentence[, options])",
            summary: "Varies English prose or a sentence object accepted by realize using scoped seeded randomness. Existing calls using only negation: boolean (true), condition_order: boolean (true), and rendering preserve their original behavior: uniform selection between the original and applicable alternatives, with unsupported prose unchanged. Adding any style control opts into bounded composition. style accepts neutral, professional, conversational, warm, terse, text_message, enthusiastic, or informal_chat. Explicit controls override preset defaults: contractions, shorthand, casual, slang, reductions, fragments, warmth, soften_requests, negation, and condition_order are booleans. New-mode negation only contracts recognized negative statements. All presets disable typos and request softening by default. punctuation accepts preserve, omit_period, exclamation, or expressive and affects the final terminal mark, preserving question marks, abbreviations, and code-like tokens. casing accepts preserve or lowercase and only lowercases reviewed everyday words and pronoun contractions. probability (default 1, range 0..1) gates wording, tone, and presentation once per message. max_changes (default 3, integer 0..16) bounds total edits. typos accepts probability (default 0, per message independently), max_errors (default 1, integer 0..16), and kinds (default all of common, omission, transposition, repetition). Typos apply only to reviewed lowercase words and share max_changes. protected is an array of nonempty case-sensitive literal strings. Quotations, code-like tokens, and authored structured subject references are protected automatically. Arbitrary prose names cannot be inferred reliably. alternatives is an array of {from: nonempty text, to: nonempty array of nonempty text} author-approved phrase families, matched literally at word boundaries. Author-supplied alternatives may authorize meaning changes. Built-in tone rules require warmth or soften_requests and never invent promises, urgency, or apologies. Fragments are limited to supported first-person progressive statements. Reductions require recognized infinitive constructions. Changes run in grammatical, phrase, tone, presentation, and typo stages with nonoverlapping choices within each stage. No draw is consumed for a zero or certain probability gate, a sole candidate, or no eligible edit. rendering applies only to structured input. Invalid options fail during evaluation.",
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
            examples: &[r#"combine([|settings.arrival, settings.availability|], "and")"#],
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
            examples: &[r#"remember({}, [|settings.guest|], [|"arrival_time"|])"#],
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
            examples: &[r#"unmentioned([|{ id: "arrival_time", text: "Arrival is after midnight." }|], settings.context)"#],
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
            examples: &[r#"merge({ tense: "present", modal: null }, [|{ modal: "can" }, { tense: null }|])"#],
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
];

// every function runs in data generation, in agents and in scorers, through one entry point
const fn function(name: &'static str, data: Evaluate, texts: bool) -> Function {
    let helper = Helper {
        name: "_english",
        // generated agents read it as an ordinary library function
        reads_as: Some("english"),
        random: true,
        seeded: false,
        counts: false,
        texts,
        plain: false,
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
    function("realize", realize, true),
    function("express", express, true),
    function("expression", expression, false),
    function("expression_candidates", expression_candidates, false),
    function("vary", vary, true),
    function("combine", combine, true),
    function("refer", refer, true),
    function("remember", remember, false),
    function("unmentioned", unmentioned, false),
    function("format_number", format_number, true),
    function("format_date", format_date, true),
    function("format_time", format_time, true),
    Function {
        name: "merge",
        check: None,
        data: Some(super::walk::merge),
        python: Some(walked("_world_merge", "merge_objects", super::walk::PY_MERGE)),
        typescript: Some(walked("_world_merge", "mergeObjects", super::walk::TS_MERGE)),
    },
    // what a weighted fork in a walk lowers to, which no author calls
    Function {
        name: super::walk::PICK,
        check: None,
        data: Some(super::walk::pick),
        python: Some(seeded("_world_pick", "take_branch")),
        typescript: Some(seeded("_world_pick", "takeBranch")),
    },
    // what the place of a trace among conversations lowers to
    Function {
        name: super::walk::TURN,
        check: None,
        data: Some(super::walk::turn),
        python: Some(seeded("_world_turn", "conversation_turn")),
        typescript: Some(seeded("_world_turn", "conversationTurn")),
    },
    // what the draws of a styled sentence lower to
    Function {
        name: super::walk::DRAWS,
        check: None,
        data: Some(super::walk::drawn),
        python: Some(seeded("_world_draws", "draws_for")),
        typescript: Some(seeded("_world_draws", "drawsFor")),
    },
    // what a sentence said a speaker's way lowers to: styled by the draws it is handed
    Function {
        name: super::walk::SAY,
        check: None,
        data: Some(say),
        python: Some(said()),
        typescript: Some(said()),
    },
    // what the identifier of a conversation lowers to
    Function {
        name: super::walk::ID,
        check: None,
        data: Some(super::walk::id),
        python: Some(seeded("_world_id", "conversation_id")),
        typescript: Some(seeded("_world_id", "conversationId")),
    },
];

// styling goes through the English entry point, as an operation no author calls
const fn said() -> Helper {
    let Some(mut helper) = function("say", say, true).python else {
        panic!("the English entry point has a helper");
    };
    helper.scorer = None;
    helper
}

// the draws a sentence is styled by, played back as a random source
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

// a sentence said a speaker's way, with the draws it was handed
fn say(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [text, wording, Value::Array(draws)] = args else {
        return Err(EvaluateError::Shape);
    };
    let draws = (draws.iter())
        .map(|draw| match draw {
            Value::Number(number) => number.as_f64().filter(|draw| (0.0..1.0).contains(draw)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(EvaluateError::Shape)?;
    let plain = |value: &Value| {
        (evaluation.plain(value)).map_err(|_| EvaluateError::Invalid("English functions do not accept attachments".into()))
    };
    let args = [plain(text)?, plain(wording)?];
    run_english(Operation::Vary, &args, &mut Replay { draws: &draws, at: 0 })
        .map(Value::from)
        .map_err(|error| EvaluateError::Invalid(format!("say: {error}").into()))
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
        text,
        operation: None,
        runtime: None,
        scorer: None,
    }
}

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
    run_english(operation, &args, evaluation.random())
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
