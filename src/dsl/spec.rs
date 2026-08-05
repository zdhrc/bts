use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct Spec {
    pub(crate) schema_version: u32,
    pub(crate) language_version: u32,
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) surface: SurfaceDesc,
    pub(crate) expressions: &'static [ExprDesc],
    pub(crate) functions: &'static [FuncDesc],
    pub(crate) blocks: &'static [BlockDesc],
    pub(crate) rules: &'static [RuleDesc],
    pub(crate) examples: &'static [Example],
}

impl Spec {
    pub(crate) fn block(&self, keyword: &str) -> Option<&BlockDesc> {
        self.blocks.iter().find(|block| block.keyword == keyword)
    }

    pub(crate) fn block_by_id(&self, id: Id) -> Option<&BlockDesc> {
        self.blocks.iter().find(|block| block.id == id)
    }

    pub(crate) fn field(&self, block: Id, field: Id) -> Option<&FieldDesc> {
        self.block_by_id(block)?.body.fields.iter().find(|desc| desc.id == field)
    }

    pub(crate) fn rule(&self, id: Id) -> Option<&RuleDesc> {
        self.rules
            .iter()
            .chain(self.blocks.iter().flat_map(|block| block.rules))
            .chain(self.expressions.iter().flat_map(|expr| expr.rules))
            .chain(self.functions.iter().flat_map(|func| func.rules))
            .find(|rule| rule.id == id)
    }

    pub(crate) fn function(&self, name: &str) -> Option<&FuncDesc> {
        self.functions.iter().find(|func| func.name == name)
    }
}

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

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct SurfaceDesc {
    pub(crate) notation: &'static str,
    pub(crate) grammar: &'static str,
    pub(crate) notes: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct ExprDesc {
    pub(crate) id: Id,
    pub(crate) syntax: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) examples: &'static [&'static str],
    pub(crate) rules: &'static [RuleDesc],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct FuncDesc {
    pub(crate) id: Id,
    pub(crate) name: &'static str,
    pub(crate) syntax: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) examples: &'static [&'static str],
    pub(crate) rules: &'static [RuleDesc],
}

// some variants are spec vocab no field descriptor uses yet, kept so field
// constraints can adopt them without touching the type system
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExprType {
    Any,
    String,
    Number,
    Boolean,
    Array { items: &'static ExprType },
    Object { values: &'static ExprType },
}

impl fmt::Display for ExprType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => formatter.write_str("any value"),
            Self::String => formatter.write_str("string"),
            Self::Number => formatter.write_str("number"),
            Self::Boolean => formatter.write_str("boolean"),
            Self::Array { items } => write!(formatter, "array<{items}>"),
            Self::Object { values } if matches!(**values, Self::Any) => formatter.write_str("object"),
            Self::Object { values } => write!(formatter, "object<{values}>"),
        }
    }
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

impl BlockDesc {
    pub(crate) fn allows(&self, place: Place) -> bool {
        self.allowed_in.contains(&place)
    }

    pub(crate) fn field(&self, keyword: &str) -> Option<&FieldDesc> {
        self.body.fields.iter().find(|field| field.keyword == keyword)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct BodyDesc {
    pub(crate) fields: &'static [FieldDesc],
    pub(crate) open: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub(crate) struct FieldDesc {
    pub(crate) id: Id,
    pub(crate) keyword: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) value: &'static ExprType,
    pub(crate) cardinality: Cardinality,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cardinality {
    Optional,
    Required,
    Repeated,
}

// some variants are spec vocab no block descriptor uses yet, the modeler already
// enforces all three
#[allow(dead_code)]
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

    pub(crate) const VARS: Id = Id::new("block.vars");
    pub(crate) const TRACE: Id = Id::new("block.trace");
    pub(crate) const TASK: Id = Id::new("block.task");
    pub(crate) const LLM: Id = Id::new("block.llm");
    pub(crate) const TOOL: Id = Id::new("block.tool");
    pub(crate) const FUNCTION: Id = Id::new("block.function");

    pub(crate) const INPUT: Id = Id::new("field.input");
    pub(crate) const OUTPUT: Id = Id::new("field.output");
    pub(crate) const METADATA: Id = Id::new("field.metadata");
    pub(crate) const METRICS: Id = Id::new("field.metrics");
    pub(crate) const TAGS: Id = Id::new("field.tags");

    pub(crate) const STRING: Id = Id::new("expr.string");
    pub(crate) const TEMPLATE: Id = Id::new("expr.template");
    pub(crate) const VAR_REF: Id = Id::new("expr.variable-reference");
    pub(crate) const NUMBER: Id = Id::new("expr.number");
    pub(crate) const BOOLEAN: Id = Id::new("expr.boolean");
    pub(crate) const NULL: Id = Id::new("expr.null");
    pub(crate) const ARRAY: Id = Id::new("expr.array");
    pub(crate) const OBJECT: Id = Id::new("expr.object");
    pub(crate) const FUNC: Id = Id::new("expr.func");
    pub(crate) const INDEX: Id = Id::new("expr.index");
    pub(crate) const SLICE: Id = Id::new("expr.slice");
    pub(crate) const SPREAD: Id = Id::new("expr.spread");
    pub(crate) const FOR: Id = Id::new("expr.for");
    pub(crate) const GROUPING: Id = Id::new("expr.grouping");
    pub(crate) const UNARY: Id = Id::new("expr.unary");
    pub(crate) const ARITHMETIC: Id = Id::new("expr.arithmetic");
    pub(crate) const COMPARISON: Id = Id::new("expr.comparison");
    pub(crate) const LOGICAL: Id = Id::new("expr.logical");
    pub(crate) const CONDITIONAL: Id = Id::new("expr.conditional");

    pub(crate) const FUNC_CHOICE: Id = Id::new("func.choice");
    pub(crate) const FUNC_RANGE: Id = Id::new("func.range");

    pub(crate) const KNOWN_REFERENCES: Id = Id::new("rule.known-references");
    pub(crate) const UNIQUE_VARS: Id = Id::new("rule.unique-vars");
    pub(crate) const STATIC_VARS: Id = Id::new("rule.static-vars");
    pub(crate) const DEFINED_VARS: Id = Id::new("rule.defined-vars");
    pub(crate) const SCALAR_INTERPOLATION: Id = Id::new("rule.scalar-interpolation");
    pub(crate) const UNIQUE_OBJECT_KEYS: Id = Id::new("rule.unique-object-keys");
    pub(crate) const FINITE_NUMBERS: Id = Id::new("rule.finite-numbers");
    pub(crate) const NONEMPTY_SHAPE: Id = Id::new("rule.nonempty-shape");
    pub(crate) const RESERVED_METRICS: Id = Id::new("rule.reserved-metrics");
    pub(crate) const KNOWN_FUNCTIONS: Id = Id::new("rule.known-functions");
    pub(crate) const FUNC_POSITIONS: Id = Id::new("rule.function-positions");
    pub(crate) const CHOICE_ALTERNATIVES: Id = Id::new("rule.choice-alternatives");
    pub(crate) const RANGE_BOUNDS: Id = Id::new("rule.range-bounds");
    pub(crate) const OPERAND_TYPES: Id = Id::new("rule.operand-types");
    pub(crate) const INDEXABLE_TARGETS: Id = Id::new("rule.indexable-targets");
    pub(crate) const INDEX_BOUNDS: Id = Id::new("rule.index-bounds");
    pub(crate) const SLICEABLE_TARGETS: Id = Id::new("rule.sliceable-targets");
    pub(crate) const SLICE_BOUNDS: Id = Id::new("rule.slice-bounds");
    pub(crate) const SPREAD_OPERANDS: Id = Id::new("rule.spread-operands");
    pub(crate) const FOR_COLLECTIONS: Id = Id::new("rule.for-collections");
    pub(crate) const STATIC_FOR: Id = Id::new("rule.static-for");
    pub(crate) const BOOLEAN_CONDITIONS: Id = Id::new("rule.boolean-conditions");
    pub(crate) const NONZERO_DIVISORS: Id = Id::new("rule.nonzero-divisors");

    pub(crate) const COMPLETE_TRACE: Id = Id::new("example.complete-trace");
}

const ANY: ExprType = ExprType::Any;
const STRING: ExprType = ExprType::String;
const OBJECT: ExprType = ExprType::Object { values: &ANY };
const STRING_ARRAY: ExprType = ExprType::Array { items: &STRING };

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
    Place::Block { id: ids::TOOL },
    Place::Block { id: ids::FUNCTION },
];

const NO_RULES: &[RuleDesc] = &[];
const FINITE_NUMBERS_RULE: RuleDesc = RuleDesc {
    id: ids::FINITE_NUMBERS,
    summary: "Every number must be representable as a finite integer or floating-point value.",
};
const FINITE_NUMBER_RULE: &[RuleDesc] = &[FINITE_NUMBERS_RULE];
const UNIQUE_OBJECT_KEYS_RULE: &[RuleDesc] = &[RuleDesc {
    id: ids::UNIQUE_OBJECT_KEYS,
    summary: "An object key may appear at most once in an object.",
}];
const KNOWN_REFERENCES_RULE: &[RuleDesc] = &[RuleDesc {
    id: ids::KNOWN_REFERENCES,
    summary: "An interpolation may only use documented references; currently `trace.index`, the 0-based index of the generated trace, and `var.<name>` for a defined variable.",
}];
const VARS_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::UNIQUE_VARS,
        summary: "A variable name may be defined at most once across all vars blocks.",
    },
    RuleDesc {
        id: ids::STATIC_VARS,
        summary: "A variable value may not reference other variables.",
    },
];
const FUNC_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::KNOWN_FUNCTIONS,
        summary: "A function call may only use documented functions; currently `choice` and `range`.",
    },
    RuleDesc {
        id: ids::FUNC_POSITIONS,
        summary: "A non-constant expression (a function call, or an operator expression containing one) may only appear where any value is accepted; fields with a specific type and interpolations must use values known before generation.",
    },
];
const OPERAND_TYPES_RULE: RuleDesc = RuleDesc {
    id: ids::OPERAND_TYPES,
    summary: "Operator operands must have a known type: `+ - * / %` and `< <= > >=` take numbers, `== !=` compare two strings, two numbers, or two booleans, and `&& || !` take booleans.",
};
const NONZERO_DIVISORS_RULE: RuleDesc = RuleDesc {
    id: ids::NONZERO_DIVISORS,
    summary: "Division and remainder require a nonzero divisor; a constant zero divisor is rejected during validation, and a divisor that evaluates to zero fails generation.",
};
const BOOLEAN_CONDITIONS_RULE: RuleDesc = RuleDesc {
    id: ids::BOOLEAN_CONDITIONS,
    summary: "The condition of `?:` must be a boolean.",
};
const UNARY_RULES: &[RuleDesc] = &[OPERAND_TYPES_RULE];
const ARITHMETIC_RULES: &[RuleDesc] = &[OPERAND_TYPES_RULE, NONZERO_DIVISORS_RULE, FINITE_NUMBERS_RULE];
const COMPARISON_RULES: &[RuleDesc] = &[OPERAND_TYPES_RULE];
const LOGICAL_RULES: &[RuleDesc] = &[OPERAND_TYPES_RULE];
const CONDITIONAL_RULES: &[RuleDesc] = &[BOOLEAN_CONDITIONS_RULE];
const CHOICE_RULES: &[RuleDesc] = &[RuleDesc {
    id: ids::CHOICE_ALTERNATIVES,
    summary: "`choice` takes at least one alternative.",
}];
const RANGE_RULES: &[RuleDesc] = &[RuleDesc {
    id: ids::RANGE_BOUNDS,
    summary: "`range` takes exactly two finite numbers with min <= max.",
}];
const INDEX_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::INDEXABLE_TARGETS,
        summary: "Indexing requires an array or object target: arrays take an integer index, objects take a string key.",
    },
    RuleDesc {
        id: ids::INDEX_BOUNDS,
        summary: "An array index must be within bounds and an object key must be present; constant violations are rejected during validation, dynamic ones fail the run during generation.",
    },
];
const SLICE_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::SLICEABLE_TARGETS,
        summary: "Slicing requires an array target; slice bounds are numbers.",
    },
    RuleDesc {
        id: ids::SLICE_BOUNDS,
        summary: "Slice bounds are non-negative integers, clamped to the array length; a start at or past the end produces an empty array. Constant violations are rejected during validation, dynamic ones fail the run during generation.",
    },
];
const SPREAD_RULES: &[RuleDesc] = &[RuleDesc {
    id: ids::SPREAD_OPERANDS,
    summary: "A spread operand must be an array (inside arrays) or an object (inside objects) whose shape is known before generation. Later object entries override keys introduced by a spread, but two explicit keys may still not collide.",
}];
const FOR_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::FOR_COLLECTIONS,
        summary: "A for expression iterates an array or object whose shape is known before generation; element values may still be dynamic.",
    },
    RuleDesc {
        id: ids::STATIC_FOR,
        summary: "For expressions unroll during validation: a filter condition must resolve to a constant boolean and an object key to a constant string.",
    },
];
const VAR_REF_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::DEFINED_VARS,
        summary: "A variable reference must name a variable defined in a vars block.",
    },
    RuleDesc {
        id: ids::SCALAR_INTERPOLATION,
        summary: "An interpolated variable must be a string, number, or boolean.",
    },
];

const EXPR_TYPES: &[ExprDesc] = &[
    ExprDesc {
        id: ids::STRING,
        syntax: "\"text\"",
        summary: "A double-quoted string. `$${` escapes a literal `${`; other escape sequences are not currently supported.",
        examples: &["\"hello\"", "\"gpt-4o-mini\""],
        rules: NO_RULES,
    },
    ExprDesc {
        id: ids::TEMPLATE,
        syntax: "\"text ${reference} text\"",
        summary: "A string containing `${...}` interpolations resolved for each generated trace. Valid anywhere a string is, except block names.",
        examples: &["\"question #${trace.index}\""],
        rules: KNOWN_REFERENCES_RULE,
    },
    ExprDesc {
        id: ids::VAR_REF,
        syntax: "var.<name>",
        summary: "A reference to a variable defined in a vars block, resolved before generation. Usable as any value, or interpolated with `${var.<name>}`.",
        examples: &["var.temperature", "\"model: ${var.model}\""],
        rules: VAR_REF_RULES,
    },
    ExprDesc {
        id: ids::NUMBER,
        syntax: "digits[.digits]",
        summary: "An integer or finite decimal number. A leading `-` is unary negation, not part of the literal.",
        examples: &["4", "0.2", "-1.5"],
        rules: FINITE_NUMBER_RULE,
    },
    ExprDesc {
        id: ids::BOOLEAN,
        syntax: "true | false",
        summary: "A boolean literal.",
        examples: &["true", "false"],
        rules: NO_RULES,
    },
    ExprDesc {
        id: ids::NULL,
        syntax: "null",
        summary: "A null literal representing an explicitly absent value.",
        examples: &["null"],
        rules: NO_RULES,
    },
    ExprDesc {
        id: ids::ARRAY,
        syntax: "[value, ...]",
        summary: "A comma-separated sequence of expressions with an optional trailing comma.",
        examples: &["[]", "[\"chat\", \"prod\"]"],
        rules: NO_RULES,
    },
    ExprDesc {
        id: ids::OBJECT,
        syntax: "{ key = value ... }",
        summary: "An object with unique identifier keys and expression values.",
        examples: &["{}", "{ tokens = 4 cached = false }"],
        rules: UNIQUE_OBJECT_KEYS_RULE,
    },
    ExprDesc {
        id: ids::FUNC,
        syntax: "name(arg, ...)",
        summary: "A call to a documented function, evaluated once per generated trace.",
        examples: &["choice(\"clear\", \"vague\")", "range(80, 400)"],
        rules: FUNC_RULES,
    },
    ExprDesc {
        id: ids::INDEX,
        syntax: "value[index] | value.field",
        summary: "Selects an array element by 0-based integer index or an object value by string key. `value.field` is shorthand for `value[\"field\"]`; the bracketed index may be any expression, evaluated per generated trace.",
        examples: &[
            "var.models[0]",
            "var.user[\"name\"]",
            "var.user.name",
            "var.models[choice(0, 1, 2)]",
        ],
        rules: INDEX_RULES,
    },
    ExprDesc {
        id: ids::SLICE,
        syntax: "value[start:end]",
        summary: "Selects a sub-range of an array: 0-based, start inclusive, end exclusive. Either bound may be omitted to default to that end of the array, and bounds may be any number expressions, evaluated per generated trace.",
        examples: &["var.xs[1:3]", "var.xs[:2]", "var.xs[range(0, 2):]"],
        rules: SLICE_RULES,
    },
    ExprDesc {
        id: ids::SPREAD,
        syntax: "[...array] | { ...object }",
        summary: "Splices the elements of an array or the entries of an object into a surrounding literal, resolved during validation. Later object entries override keys a spread introduced.",
        examples: &["[1, ...var.xs]", "{ ...var.meta temperature = 0.9 }"],
        rules: SPREAD_RULES,
    },
    ExprDesc {
        id: ids::FOR,
        syntax: "[for x in collection : body if cond] | { for k, v in collection : key => value if cond }",
        summary: "Maps and filters a collection into a new array or object, unrolled during validation. One binding names the element (arrays) or key (objects); two name index/element or key/value. A binding referenced twice re-evaluates a dynamic element each time, like variables.",
        examples: &[
            "[for x in var.xs : x * 2]",
            "[for i, x in var.xs : \"${i}-${x}\" if i > 0]",
            "{ for k, v in var.meta : k => v if k != \"secret\" }",
        ],
        rules: FOR_RULES,
    },
    ExprDesc {
        id: ids::GROUPING,
        syntax: "(expression)",
        summary: "Parentheses group a sub-expression to override operator precedence.",
        examples: &["(1 + 2) * 3"],
        rules: NO_RULES,
    },
    ExprDesc {
        id: ids::UNARY,
        syntax: "-value | !value",
        summary: "Negates a number or inverts a boolean.",
        examples: &["-4", "!var.cached"],
        rules: UNARY_RULES,
    },
    ExprDesc {
        id: ids::ARITHMETIC,
        syntax: "a + b | a - b | a * b | a / b | a % b",
        summary: "Arithmetic over numbers. Two integer operands produce an integer (`/` truncates toward zero); any float operand produces a float. Constant expressions are evaluated during validation; a divisor that evaluates to zero during generation fails the run.",
        examples: &["7 / 2", "var.total * 0.5", "range(1, 5) * 100"],
        rules: ARITHMETIC_RULES,
    },
    ExprDesc {
        id: ids::COMPARISON,
        syntax: "a == b | a != b | a < b | a <= b | a > b | a >= b",
        summary: "Equality over two strings, two numbers, or two booleans; ordering over numbers. Produces a boolean.",
        examples: &["var.model == \"gpt-4o\"", "range(1, 10) > 5"],
        rules: COMPARISON_RULES,
    },
    ExprDesc {
        id: ids::LOGICAL,
        syntax: "a && b | a || b",
        summary: "Boolean and/or. Short-circuits: the right operand is only evaluated when the left doesn't decide the result.",
        examples: &["var.prod && !var.cached"],
        rules: LOGICAL_RULES,
    },
    ExprDesc {
        id: ids::CONDITIONAL,
        syntax: "cond ? then : else",
        summary: "Picks `then` when the condition is true, `else` otherwise. Right-associative; only the taken branch is evaluated for each generated trace.",
        examples: &["var.tier == \"pro\" ? range(20, 80) : range(200, 800)"],
        rules: CONDITIONAL_RULES,
    },
];

const FUNCS: &[FuncDesc] = &[
    FuncDesc {
        id: ids::FUNC_CHOICE,
        name: "choice",
        syntax: "choice(value, ...)",
        summary: "Picks one of its alternatives uniformly at random for each generated trace. Alternatives may be any value, including nested functions.",
        examples: &["choice(\"gpt-4o\", \"gpt-4o-mini\")", "choice(1, 2, range(5, 9))"],
        rules: CHOICE_RULES,
    },
    FuncDesc {
        id: ids::FUNC_RANGE,
        name: "range",
        syntax: "range(min, max)",
        summary: "Samples a number uniformly between min and max (inclusive) for each generated trace. Two integer bounds sample an integer; otherwise a float.",
        examples: &["range(80, 400)", "range(0.0, 1.0)"],
        rules: RANGE_RULES,
    },
];

const BLOCKS: &[BlockDesc] = &[
    BlockDesc {
        id: ids::VARS,
        keyword: "vars",
        summary: "A block of named values shared across the shape via `var.<name>` references.",
        syntax: "vars { <name> = <value> ... }",
        name: NameDesc::Forbidden,
        allowed_in: ROOT_ONLY,
        body: BodyDesc { fields: &[], open: true },
        rules: VARS_RULES,
    },
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
    BlockDesc {
        id: ids::TOOL,
        keyword: "tool",
        summary: "A named tool span containing fields and nested spans.",
        syntax: "tool \"<name>\" { ... }",
        name: NameDesc::Required,
        allowed_in: IN_TRACE_OR_SPAN,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
    },
    BlockDesc {
        id: ids::FUNCTION,
        keyword: "function",
        summary: "A named function span containing fields and nested spans.",
        syntax: "function \"<name>\" { ... }",
        name: NameDesc::Required,
        allowed_in: IN_TRACE_OR_SPAN,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
    },
];

pub(crate) const RESERVED_METRIC_KEYS: &[&str] = &["start", "end"];

const RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::NONEMPTY_SHAPE,
        summary: "A shape must declare at least one trace block.",
    },
    RuleDesc {
        id: ids::RESERVED_METRICS,
        summary: "Metric keys `start` and `end` are reserved for generated timestamps.",
    },
];

const EXAMPLES: &[Example] = &[Example {
    id: ids::COMPLETE_TRACE,
    summary: "A multi-turn trace containing task and LLM spans.",
    source: include_str!("../../tests/fixtures/simple.bt"),
    valid: true,
}];

pub(crate) static SPEC: Spec = Spec {
    schema_version: 1,
    language_version: 1,
    name: "bts",
    summary: "A declarative language for describing synthetic traces and spans.",
    surface: SurfaceDesc {
        notation: "EBNF",
        grammar: r#"
document       = { declaration } ;
declaration    = block | attribute ;
block          = identifier, [ string ], "{", { declaration }, "}" ;
attribute      = identifier, "=", expression ;
expression     = conditional ;
conditional    = logical_or, [ "?", expression, ":", conditional ] ;
logical_or     = logical_and, { "||", logical_and } ;
logical_and    = equality, { "&&", equality } ;
equality       = comparison, { ( "==" | "!=" ), comparison } ;
comparison     = additive, { ( "<" | "<=" | ">" | ">=" ), additive } ;
additive       = multiplicative, { ( "+" | "-" ), multiplicative } ;
multiplicative = unary, { ( "*" | "/" | "%" ), unary } ;
unary          = ( "-" | "!" ), unary | postfix ;
postfix        = primary, { "[", expression, "]" | "[", [ expression ], ":", [ expression ], "]" | ".", identifier } ;
primary        = string | number | boolean | null | array | object | variable | binding | function
               | "(", expression, ")" ;
boolean        = "true" | "false" ;
null           = "null" ;
variable       = "var", ".", identifier ;
binding        = identifier (* a loop binding introduced by an enclosing for expression *) ;
function       = identifier, "(", [ expression, { ",", expression }, [ "," ] ], ")" ;
array          = "[", [ array_items | for_array ], "]" ;
array_items    = array_item, { ",", array_item }, [ "," ] ;
array_item     = expression | "...", expression ;
object         = "{", ( { object_item } | for_object ), "}" ;
object_item    = attribute | "...", expression ;
for_array      = "for", bindings, "in", expression, ":", expression, [ "if", expression ] ;
for_object     = "for", bindings, "in", expression, ":", expression, "=>", expression, [ "if", expression ] ;
bindings       = identifier, [ ",", identifier ] ;
identifier     = ASCII_ALPHA, { ASCII_ALPHA | ASCII_DIGIT | "_" } ;
number         = ASCII_DIGIT, { ASCII_DIGIT }, [ ".", ASCII_DIGIT, { ASCII_DIGIT } ] ;
string         = '"', { ANY_EXCEPT_DOUBLE_QUOTE_OR_INTERPOLATION | escape | interpolation }, '"' ;
escape         = "$${" ;
interpolation  = "${", reference, "}" ;
reference      = identifier, { ".", identifier } ;
"#,
        notes: &[
            "Whitespace separates tokens and is otherwise insignificant.",
            "The surface grammar accepts declarations generically; block placement and field types are semantic rules.",
            "Comments are not currently supported.",
            "Inside strings, `$${` escapes a literal `${`; any other `$` is literal.",
            "Block names are plain strings; interpolation is not allowed in them.",
            "Binary operators are left-associative; `?:` is right-associative.",
            "Interpolations accept references only, not operators or function calls.",
            "A missing comma between numeric items parses as subtraction: `[1 -2]` is the one-element array `[-1]`.",
            "A missing comma before an item starting with `[` parses as an index: `[var.a [0]]` is the one-element array `[var.a[0]]`.",
            "`for` is a keyword only directly after `[` or `{`; `{ for = 1 }` is still an attribute. `in` and `if` are keywords only inside a for expression, and none of the three can name a loop binding.",
            "A full ternary inside brackets stays an index: `xs[a ? 0 : 1]` selects one element. Parenthesize a ternary bound to slice from it.",
        ],
    },
    expressions: EXPR_TYPES,
    functions: FUNCS,
    blocks: BLOCKS,
    rules: RULES,
    examples: EXAMPLES,
};
