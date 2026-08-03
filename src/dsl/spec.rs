//! Declarative description of the BTS language.
//!
//! This module describes the language's public shape: its surface grammar,
//! expression forms, constructs, placement rules, and documented validation
//! rules. It deliberately does not implement lexing or parsing. Those layers
//! decide how source becomes syntax; this specification describes which parsed
//! declarations are meaningful BTS programs.
//!
//! The descriptions use static data so they can be shared by validation,
//! documentation, editor tooling, and agent setup without allocation or a
//! second source of truth.

use serde::Serialize;

/// The complete public description of a language version.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct Spec {
    /// Version of the serialized description shape.
    pub(crate) schema_version: u32,
    /// Version of the language described by this value.
    pub(crate) language_version: u32,
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) surface: SurfaceDesc,
    pub(crate) expressions: &'static [ExprForm],
    pub(crate) blocks: &'static [BlockDesc],
    /// Rules applying to the language as a whole rather than one block.
    pub(crate) rules: &'static [RuleDesc],
    pub(crate) examples: &'static [Example],
}

/// A stable identifier used to connect descriptions to implementation code.
///
/// IDs are intended for matching, diagnostics, and serialized output. Display
/// names and source keywords may change without forcing an ID change.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct Id(&'static str);

impl Id {
    pub(crate) const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Human- and machine-readable information about the concrete syntax.
///
/// The grammar is descriptive for now. If BTS later adopts a parser generator,
/// this field can be generated from its grammar without changing consumers of
/// the rest of the specification.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct SurfaceDesc {
    pub(crate) notation: &'static str,
    pub(crate) grammar: &'static str,
    pub(crate) notes: &'static [&'static str],
}

/// A concrete expression form recognized by the surface grammar.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct ExprForm {
    pub(crate) id: Id,
    pub(crate) syntax: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) examples: &'static [&'static str],
    pub(crate) rules: &'static [RuleDesc],
}

/// A constraint on the value accepted by a field.
///
/// `Named` leaves room for reusable domain types without embedding those types
/// in every field. `OneOf` supports unions, while `Array` and `Object` describe
/// the recursive containers supported by the current expression grammar.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExprDesc {
    Any,
    String,
    Number,
    Boolean,
    Array { items: &'static ExprDesc },
    Object { values: &'static ExprDesc },
    OneOf { variants: &'static [ExprDesc] },
    Named { id: Id },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct BlockDesc {
    pub(crate) id: Id,
    pub(crate) keyword: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) syntax: &'static str,
    pub(crate) name: NameDesc,
    pub(crate) allowed_in: &'static [Place],
    pub(crate) body: BodyDesc,
    pub(crate) rules: &'static [RuleDesc],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct BodyDesc {
    pub(crate) fields: &'static [FieldDesc],
    /// Whether unlisted fields are accepted.
    ///
    /// Nested blocks are determined by each block's `allowed_in` placements, so
    /// the parent-child relationship has only one source of truth.
    pub(crate) open: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct FieldDesc {
    pub(crate) id: Id,
    pub(crate) keyword: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) value: &'static ExprDesc,
    pub(crate) cardinality: Cardinality,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cardinality {
    /// Zero or one occurrence.
    Optional,
    /// Exactly one occurrence.
    Required,
    /// Zero or more occurrences.
    Repeated,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NameDesc {
    Forbidden,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Place {
    Root,
    Block { id: Id },
}

/// A named validation rule that cannot, or should not, be inferred from shape.
///
/// Implementations can associate a validator with the stable ID while renderers
/// expose the same summary to people and tools.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct RuleDesc {
    pub(crate) id: Id,
    pub(crate) summary: &'static str,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct Example {
    pub(crate) id: Id,
    pub(crate) summary: &'static str,
    pub(crate) source: &'static str,
    pub(crate) valid: bool,
}

pub(crate) mod ids {
    use super::Id;

    pub(crate) const TRACE: Id = Id::new("block.trace");
    pub(crate) const TASK: Id = Id::new("block.task");
    pub(crate) const LLM: Id = Id::new("block.llm");

    pub(crate) const INPUT: Id = Id::new("field.input");
    pub(crate) const OUTPUT: Id = Id::new("field.output");
    pub(crate) const METADATA: Id = Id::new("field.metadata");
    pub(crate) const METRICS: Id = Id::new("field.metrics");
    pub(crate) const TAGS: Id = Id::new("field.tags");

    pub(crate) const STRING: Id = Id::new("expr.string");
    pub(crate) const NUMBER: Id = Id::new("expr.number");
    pub(crate) const BOOLEAN: Id = Id::new("expr.boolean");
    pub(crate) const ARRAY: Id = Id::new("expr.array");
    pub(crate) const OBJECT: Id = Id::new("expr.object");

    pub(crate) const UNIQUE_OBJECT_KEYS: Id = Id::new("rule.unique-object-keys");
    pub(crate) const FINITE_NUMBERS: Id = Id::new("rule.finite-numbers");

    pub(crate) const COMPLETE_TRACE: Id = Id::new("example.complete-trace");
}

const ANY: ExprDesc = ExprDesc::Any;
const STRING: ExprDesc = ExprDesc::String;
const OBJECT: ExprDesc = ExprDesc::Object { values: &ANY };
const STRING_ARRAY: ExprDesc = ExprDesc::Array { items: &STRING };

const SPAN_FIELDS: &[FieldDesc] = &[
    FieldDesc {
        id: ids::INPUT,
        keyword: "input",
        summary: "Input associated with the trace or span.",
        value: &ANY,
        cardinality: Cardinality::Optional,
    },
    FieldDesc {
        id: ids::OUTPUT,
        keyword: "output",
        summary: "Output associated with the trace or span.",
        value: &ANY,
        cardinality: Cardinality::Optional,
    },
    FieldDesc {
        id: ids::METADATA,
        keyword: "metadata",
        summary: "Arbitrary metadata attached to the trace or span.",
        value: &OBJECT,
        cardinality: Cardinality::Optional,
    },
    FieldDesc {
        id: ids::METRICS,
        keyword: "metrics",
        summary: "Arbitrary metric values attached to the trace or span.",
        value: &OBJECT,
        cardinality: Cardinality::Optional,
    },
    FieldDesc {
        id: ids::TAGS,
        keyword: "tags",
        summary: "String labels attached to the trace or span.",
        value: &STRING_ARRAY,
        cardinality: Cardinality::Optional,
    },
];

const ROOT_ONLY: &[Place] = &[Place::Root];
const IN_TRACE_OR_SPAN: &[Place] = &[
    Place::Block { id: ids::TRACE },
    Place::Block { id: ids::TASK },
    Place::Block { id: ids::LLM },
];

const NO_RULES: &[RuleDesc] = &[];
const FINITE_NUMBER_RULE: &[RuleDesc] = &[RuleDesc {
    id: ids::FINITE_NUMBERS,
    summary: "Every number must be representable as a finite integer or floating-point value.",
}];
const UNIQUE_OBJECT_KEYS_RULE: &[RuleDesc] = &[RuleDesc {
    id: ids::UNIQUE_OBJECT_KEYS,
    summary: "An object key may appear at most once in an object.",
}];

const EXPR_FORMS: &[ExprForm] = &[
    ExprForm {
        id: ids::STRING,
        syntax: "\"text\"",
        summary: "A double-quoted string. Escape sequences are not currently supported.",
        examples: &["\"hello\"", "\"gpt-4o-mini\""],
        rules: NO_RULES,
    },
    ExprForm {
        id: ids::NUMBER,
        syntax: "digits[.digits]",
        summary: "A non-negative integer or finite decimal number.",
        examples: &["4", "0.2"],
        rules: FINITE_NUMBER_RULE,
    },
    ExprForm {
        id: ids::BOOLEAN,
        syntax: "true | false",
        summary: "A boolean literal.",
        examples: &["true", "false"],
        rules: NO_RULES,
    },
    ExprForm {
        id: ids::ARRAY,
        syntax: "[value, ...]",
        summary: "A comma-separated sequence of expressions with an optional trailing comma.",
        examples: &["[]", "[\"chat\", \"prod\"]"],
        rules: NO_RULES,
    },
    ExprForm {
        id: ids::OBJECT,
        syntax: "{ key = value ... }",
        summary: "An object with unique identifier keys and expression values.",
        examples: &["{}", "{ tokens = 4 cached = false }"],
        rules: UNIQUE_OBJECT_KEYS_RULE,
    },
];

const BLOCKS: &[BlockDesc] = &[
    BlockDesc {
        id: ids::TRACE,
        keyword: "trace",
        summary: "A named root trace containing fields and nested spans.",
        syntax: "trace \"<name>\" { ... }",
        name: NameDesc::Required,
        allowed_in: ROOT_ONLY,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
    },
    BlockDesc {
        id: ids::TASK,
        keyword: "task",
        summary: "A named task span containing fields and nested spans.",
        syntax: "task \"<name>\" { ... }",
        name: NameDesc::Required,
        allowed_in: IN_TRACE_OR_SPAN,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
    },
    BlockDesc {
        id: ids::LLM,
        keyword: "llm",
        summary: "A named LLM span containing fields and nested spans.",
        syntax: "llm \"<name>\" { ... }",
        name: NameDesc::Required,
        allowed_in: IN_TRACE_OR_SPAN,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
    },
];

const RULES: &[RuleDesc] = &[];

const EXAMPLES: &[Example] = &[Example {
    id: ids::COMPLETE_TRACE,
    summary: "A multi-turn trace containing task and LLM spans.",
    source: include_str!("../../tests/fixtures/simple.bt"),
    valid: true,
}];

pub(crate) const SPEC: Spec = Spec {
    schema_version: 1,
    language_version: 1,
    name: "BTS",
    summary: "A declarative language for describing synthetic traces and spans.",
    surface: SurfaceDesc {
        notation: "EBNF",
        grammar: r#"
document    = { declaration } ;
declaration = block | attribute ;
block       = identifier, [ string ], "{", { declaration }, "}" ;
attribute   = identifier, "=", expression ;
expression  = string | number | boolean | array | object ;
boolean     = "true" | "false" ;
array       = "[", [ expression, { ",", expression }, [ "," ] ], "]" ;
object      = "{", { attribute }, "}" ;
identifier  = ASCII_ALPHA, { ASCII_ALPHA | ASCII_DIGIT | "_" } ;
number      = ASCII_DIGIT, { ASCII_DIGIT }, [ ".", ASCII_DIGIT, { ASCII_DIGIT } ] ;
string      = '"', { ANY_EXCEPT_DOUBLE_QUOTE }, '"' ;
"#,
        notes: &[
            "Whitespace separates tokens and is otherwise insignificant.",
            "The surface grammar accepts declarations generically; block placement and field types are semantic rules.",
            "Comments and string escape sequences are not currently supported.",
        ],
    },
    expressions: EXPR_FORMS,
    blocks: BLOCKS,
    rules: RULES,
    examples: EXAMPLES,
};
