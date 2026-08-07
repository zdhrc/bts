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
    pub(crate) name: NamePolicy,
    pub(crate) allowed_in: &'static [Place],
    pub(crate) body: BodyDesc,
    pub(crate) rules: &'static [RuleDesc],
    pub(crate) conventions: &'static [&'static str],
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
pub(crate) enum NamePolicy {
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
    pub(crate) note: &'static str,
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
    pub(crate) const REPEAT: Id = Id::new("block.repeat");
    pub(crate) const CHOICE: Id = Id::new("block.choice");
    pub(crate) const MAYBE: Id = Id::new("block.maybe");

    pub(crate) const INPUT: Id = Id::new("field.input");
    pub(crate) const OUTPUT: Id = Id::new("field.output");
    pub(crate) const EXPECTED: Id = Id::new("field.expected");
    pub(crate) const ERROR: Id = Id::new("field.error");
    pub(crate) const METADATA: Id = Id::new("field.metadata");
    pub(crate) const METRICS: Id = Id::new("field.metrics");
    pub(crate) const TAGS: Id = Id::new("field.tags");
    pub(crate) const COUNT: Id = Id::new("field.count");
    pub(crate) const CHANCE: Id = Id::new("field.chance");

    pub(crate) const STRING: Id = Id::new("expr.string");
    pub(crate) const TEMPLATE: Id = Id::new("expr.template");
    pub(crate) const MULTILINE: Id = Id::new("expr.multiline-string");
    pub(crate) const HEREDOC: Id = Id::new("expr.heredoc");
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
    pub(crate) const FUNC_WEIGHTED: Id = Id::new("func.weighted");
    pub(crate) const FUNC_NORMAL: Id = Id::new("func.normal");
    pub(crate) const FUNC_LOGNORMAL: Id = Id::new("func.lognormal");
    pub(crate) const FUNC_EXPONENTIAL: Id = Id::new("func.exponential");
    pub(crate) const FUNC_PARETO: Id = Id::new("func.pareto");
    pub(crate) const FUNC_BETA: Id = Id::new("func.beta");
    pub(crate) const FUNC_POISSON: Id = Id::new("func.poisson");
    pub(crate) const FUNC_CHANCE: Id = Id::new("func.chance");
    pub(crate) const FUNC_UPPER: Id = Id::new("func.upper");
    pub(crate) const FUNC_LOWER: Id = Id::new("func.lower");
    pub(crate) const FUNC_TRIM: Id = Id::new("func.trim");
    pub(crate) const FUNC_REPLACE: Id = Id::new("func.replace");
    pub(crate) const FUNC_SPLIT: Id = Id::new("func.split");
    pub(crate) const FUNC_JOIN: Id = Id::new("func.join");
    pub(crate) const FUNC_CONTAINS: Id = Id::new("func.contains");
    pub(crate) const FUNC_STARTS_WITH: Id = Id::new("func.starts_with");
    pub(crate) const FUNC_ENDS_WITH: Id = Id::new("func.ends_with");
    pub(crate) const FUNC_LEN: Id = Id::new("func.len");
    pub(crate) const FUNC_FORMAT: Id = Id::new("func.format");
    pub(crate) const FUNC_CLAMP: Id = Id::new("func.clamp");
    pub(crate) const FUNC_ROUND: Id = Id::new("func.round");
    pub(crate) const FUNC_FLOOR: Id = Id::new("func.floor");
    pub(crate) const FUNC_CEIL: Id = Id::new("func.ceil");
    pub(crate) const FUNC_ABS: Id = Id::new("func.abs");
    pub(crate) const FUNC_MIN: Id = Id::new("func.min");
    pub(crate) const FUNC_MAX: Id = Id::new("func.max");
    pub(crate) const FUNC_UUID: Id = Id::new("func.uuid");
    pub(crate) const FUNC_HEX: Id = Id::new("func.hex");
    pub(crate) const FUNC_ALPHANUM: Id = Id::new("func.alphanum");

    pub(crate) const MULTILINE_DELIMITERS: Id = Id::new("rule.multiline-delimiters");
    pub(crate) const MULTILINE_INDENT: Id = Id::new("rule.multiline-indentation");
    pub(crate) const HEREDOC_DELIMITERS: Id = Id::new("rule.heredoc-delimiters");
    pub(crate) const HEREDOC_INDENT: Id = Id::new("rule.heredoc-indentation");
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
    pub(crate) const FUNC_ARITY: Id = Id::new("rule.function-arity");
    pub(crate) const FUNC_ARG_TYPES: Id = Id::new("rule.function-argument-types");
    pub(crate) const WEIGHTED_OPTIONS: Id = Id::new("rule.weighted-options");
    pub(crate) const DIST_PARAMS: Id = Id::new("rule.distribution-params");
    pub(crate) const FORMAT_TEMPLATE: Id = Id::new("rule.format-template");
    pub(crate) const SPLIT_SEPARATOR: Id = Id::new("rule.split-separator");
    pub(crate) const JOIN_ELEMENTS: Id = Id::new("rule.join-elements");
    pub(crate) const CLAMP_BOUNDS: Id = Id::new("rule.clamp-bounds");
    pub(crate) const INTEGER_RESULTS: Id = Id::new("rule.integer-results");
    pub(crate) const RANDOM_LENGTH: Id = Id::new("rule.random-string-length");
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
    pub(crate) const REPEAT_COUNT: Id = Id::new("rule.repeat-count");
    pub(crate) const REPEAT_INDEX: Id = Id::new("rule.repeat-index");
    pub(crate) const MAYBE_CHANCE: Id = Id::new("rule.maybe-chance");
    pub(crate) const DYNAMIC_CHILDREN: Id = Id::new("rule.dynamic-children");

    pub(crate) const MULTI_TURN_CONVERSATION: Id = Id::new("example.multi-turn-conversation");
    pub(crate) const AGENT_TOOL_LOOP: Id = Id::new("example.agent-tool-loop");
    pub(crate) const SUPERVISOR_AND_SUBAGENTS: Id = Id::new("example.supervisor-and-subagents");
    pub(crate) const FANOUT_PARALLEL_WORKERS: Id = Id::new("example.fanout-parallel-workers");
    pub(crate) const WINDOWED_SESSION: Id = Id::new("example.windowed-session");
    pub(crate) const RAG_PIPELINE: Id = Id::new("example.rag-pipeline");
    pub(crate) const ERROR_AND_ESCALATION: Id = Id::new("example.error-and-escalation");
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
        id: ids::EXPECTED,
        keyword: "expected",
        summary: "Expected output associated with the trace or span.",
        value: &ANY,
        cardinality: Cardinality::Optional,
    },
    FieldDesc {
        id: ids::ERROR,
        keyword: "error",
        summary: "Error associated with the trace or span.",
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
const IN_TRACE_SPAN_OR_DYNAMIC: &[Place] = &[
    Place::Block { id: ids::TRACE },
    Place::Block { id: ids::TASK },
    Place::Block { id: ids::LLM },
    Place::Block { id: ids::TOOL },
    Place::Block { id: ids::FUNCTION },
    Place::Block { id: ids::REPEAT },
    Place::Block { id: ids::CHOICE },
    Place::Block { id: ids::MAYBE },
];

const REPEAT_FIELDS: &[FieldDesc] = &[FieldDesc {
    id: ids::COUNT,
    keyword: "count",
    summary: "Number of times the child blocks are stamped out, evaluated per generated trace.",
    value: &ANY,
    cardinality: Cardinality::Required,
}];

const MAYBE_FIELDS: &[FieldDesc] = &[FieldDesc {
    id: ids::CHANCE,
    keyword: "chance",
    summary: "Probability between 0 and 1 that the child blocks are included, evaluated per generated trace; defaults to 0.5.",
    value: &ANY,
    cardinality: Cardinality::Optional,
}];

const NO_RULES: &[RuleDesc] = &[];
const NO_CONVENTIONS: &[&str] = &[];

// conventions mirror what braintrust's own sdk wrappers emit, so generated
// traces render in the ui like traces from real instrumentation
const TRACE_CONVENTIONS: &[&str] = &[
    "Name the root span after the application, session, or entrypoint (e.g. `support-sessions`), the way a top-level traced function would be named; never a prose description.",
    "The root usually carries the user-facing exchange: the user message(s) as `input`, the final answer as `output`.",
    "`tags` belong on the trace root, not on child spans.",
];
const TASK_CONVENTIONS: &[&str] = &[
    "Name task spans like the function or pipeline step real instrumentation would trace (`turn_0`, `agent_node`, `retrieve_context`).",
    "Input and output are free-form: typically the text or values the step consumed and produced.",
    "Task spans usually carry no metrics; timestamps are generated.",
];
const LLM_CONVENTIONS: &[&str] = &[
    "Name LLM spans the way SDK wrappers do: `Chat Completion` for OpenAI-style chat calls, `anthropic.messages.create` for Anthropic. Real instrumentation never uses the model name or an action description as the span name.",
    "`input` is an OpenAI-format message array, system message included: `[{ role = \"system\", content = \"...\" }, { role = \"user\", content = \"...\" }]`. This is what the Braintrust UI renders as a chat transcript and what enables the Try prompt button.",
    "`output` is an assistant message object: `{ role = \"assistant\", content = \"...\" }`.",
    "`metadata` holds the request parameters at the top level: `model`, `provider` (e.g. `openai`, `anthropic`), and settings like `temperature`, `max_tokens`, or `tool_choice`. Use a real registered model id so Braintrust can compute cost from token counts.",
    "`metrics` uses Braintrust's exact field names: `prompt_tokens`, `completion_tokens`, and `tokens` (their sum). These power the token and cost columns in the UI.",
    "Optional metrics, same exact names: `prompt_cached_tokens` (cache reads) and `prompt_cache_creation_tokens` (cache writes), both already included in `prompt_tokens`; `time_to_first_token` in seconds, smaller than the span duration; `estimated_cost` to override the computed cost.",
    "Metric values cannot reference sibling fields, so a sampled `prompt_tokens` cannot feed an exact `tokens` sum. Write token metrics as constants that sum correctly; to vary them per trace, put alternative `llm` blocks differing only in metrics inside a `choice` block.",
];
const TOOL_CONVENTIONS: &[&str] = &[
    "Name tool spans exactly the tool's function name (`get_stock_performance`), never a description of what it does.",
    "`input` is the arguments object (`{ ticker = \"NVDA\" }`); `output` is the result object.",
    "Tool spans typically carry no metadata or metrics.",
];
const FUNCTION_CONVENTIONS: &[&str] = &[
    "Name function spans after the invoked function, like tool spans.",
    "`input` is the arguments object; `output` is the return value.",
];
const REPEAT_CONVENTIONS: &[&str] = &[
    "Sample `count` (`weighted`, `poisson`, `range`) so structure differs per trace; a constant count belongs only in a grouping repeat.",
    "Every iteration stamps identical content except `${repeat.index}`, so repeat suits structurally similar steps (tool rounds, workers) — not conversation turns, which must carry history coherently.",
    "`repeat` with `count = 1` stamps its children once and adds no span, which groups several blocks into a single `choice` alternative.",
];
const CHOICE_CONVENTIONS: &[&str] = &[
    "The primary tool for varying content coherently: wrap whole alternative subtrees (a full conversation, a full tool round) so every field of the picked branch agrees.",
    "A handful of fully authored alternatives beats one parameterized template; add alternatives to widen the pool.",
];
const MAYBE_CONVENTIONS: &[&str] = &[
    "Model rare paths — errors, escalations, retries — at realistic single-digit chances.",
    "Children are included together or not at all, so one maybe block holds a whole correlated failure; nest maybe inside choice branches to give paths different failure rates.",
];
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
    summary: "An interpolation may only use documented references; currently `trace.index`, the 0-based index of the generated trace, `repeat.index`, the 0-based iteration of the innermost enclosing repeat block, and `var.<name>` for a defined variable.",
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
        summary: "A function call may only use the documented functions listed in this spec.",
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
const FUNC_ARITY_RULE: RuleDesc = RuleDesc {
    id: ids::FUNC_ARITY,
    summary: "A function takes the arguments its syntax documents; `choice`, `weighted`, `min`, `max`, and `format` accept a variable number.",
};
const FUNC_ARG_TYPES_RULE: RuleDesc = RuleDesc {
    id: ids::FUNC_ARG_TYPES,
    summary: "A function argument must have the documented type, known before generation; an argument whose type is only known during generation (e.g. a `choice` with mixed alternatives) is rejected.",
};
const DIST_PARAMS_RULE: RuleDesc = RuleDesc {
    id: ids::DIST_PARAMS,
    summary: "Distribution parameters are constant finite numbers: `normal` takes stddev >= 0, `lognormal` takes median > 0 and sigma >= 0, `exponential` and `poisson` take mean > 0, `pareto` takes min > 0 and shape > 0, `beta` takes alpha > 0 and beta > 0, and `chance` takes a probability between 0 and 1.",
};
const SAMPLING_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, DIST_PARAMS_RULE, FINITE_NUMBERS_RULE];
const WEIGHTED_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    RuleDesc {
        id: ids::WEIGHTED_OPTIONS,
        summary: "`weighted` takes at least one `[value, weight]` pair; weights are constant non-negative finite numbers, at least one of them positive.",
    },
];
const TEXT_FUNC_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE];
const SPLIT_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        id: ids::SPLIT_SEPARATOR,
        summary: "`split` requires a non-empty separator; a constant empty separator is rejected during validation, and a dynamic one fails the run during generation.",
    },
];
const JOIN_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        id: ids::JOIN_ELEMENTS,
        summary: "`join` stringifies string, number, and boolean elements; any other element fails the run during generation.",
    },
];
const FORMAT_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        id: ids::FORMAT_TEMPLATE,
        summary: "`format` takes a constant string template with exactly one `{}` placeholder per remaining argument, replaced in order.",
    },
];
const NUMERIC_FUNC_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE, FINITE_NUMBERS_RULE];
const CLAMP_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        id: ids::CLAMP_BOUNDS,
        summary: "`clamp` bounds must satisfy min <= max; a constant violation is rejected during validation, and a dynamic one fails the run during generation.",
    },
];
const ROUNDING_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        id: ids::INTEGER_RESULTS,
        summary: "`round`, `floor`, and `ceil` produce integers; a result outside the 64-bit integer range fails the run during generation.",
    },
];
const RANDOM_STRING_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    RuleDesc {
        id: ids::RANDOM_LENGTH,
        summary: "`hex` and `alphanum` take a constant non-negative integer length.",
    },
];
const UUID_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE];
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
const MULTILINE_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::MULTILINE_DELIMITERS,
        summary: "Multi-line content starts on the line after the opening `\"\"\"` and ends on the line before the closing `\"\"\"`, which sits alone on its own line; the final newline is not part of the value, so leave a blank line to keep one.",
    },
    RuleDesc {
        id: ids::MULTILINE_INDENT,
        summary: "The closing delimiter's leading whitespace is stripped from every content line; blank lines are exempt, and no other line may be indented less.",
    },
];
const HEREDOC_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::HEREDOC_DELIMITERS,
        summary: "Heredoc content starts on the line after the introducer and ends at the first line holding only the delimiter word (surrounding whitespace allowed); unlike `\"\"\"` strings, the newline ending the last content line is part of the value.",
    },
    RuleDesc {
        id: ids::HEREDOC_INDENT,
        summary: "`<<` keeps every content line verbatim; `<<-` strips the longest whitespace prefix shared by the non-blank content lines, and blank lines keep only their newline.",
    },
];
const REPEAT_RULES: &[RuleDesc] = &[
    RuleDesc {
        id: ids::REPEAT_COUNT,
        summary: "`count` must evaluate to a non-negative integer; a constant violation is rejected during validation, and a dynamic one fails the run during generation.",
    },
    RuleDesc {
        id: ids::REPEAT_INDEX,
        summary: "`repeat.index` interpolates the 0-based iteration of the innermost enclosing repeat block and is only valid inside one.",
    },
];
const MAYBE_RULES: &[RuleDesc] = &[RuleDesc {
    id: ids::MAYBE_CHANCE,
    summary: "`chance` must evaluate to a number between 0 and 1 inclusive; a constant violation is rejected during validation, and a dynamic one fails the run during generation.",
}];
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
        id: ids::MULTILINE,
        syntax: "\"\"\"\ntext\n\"\"\"",
        summary: "A triple-quoted string spanning multiple lines. Quotes are literal inside it, and `$${` and `${...}` interpolation work as in single-line strings. Valid anywhere a string is.",
        examples: &["\"\"\"\n    You are ${var.assistant}.\n    Answer briefly.\n    \"\"\""],
        rules: MULTILINE_RULES,
    },
    ExprDesc {
        id: ids::HEREDOC,
        syntax: "<<DELIM\ntext\nDELIM | <<-DELIM\ntext\nDELIM",
        summary: "A heredoc string closed by a line holding only its delimiter word. Quotes are literal inside it, and `$${` and `${...}` interpolation work as in single-line strings. Valid anywhere a string is.",
        examples: &["<<-PROMPT\n    You are ${var.assistant}.\n    Answer briefly.\nPROMPT"],
        rules: HEREDOC_RULES,
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
        syntax: "{ key = value, ... }",
        summary: "An object with unique identifier keys and expression values. Items on the same line are separated by commas, with an optional trailing comma; a line break also separates items.",
        examples: &["{}", "{ tokens = 4, cached = false }"],
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
        examples: &["[1, ...var.xs]", "{ ...var.meta, temperature = 0.9 }"],
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
    FuncDesc {
        id: ids::FUNC_WEIGHTED,
        name: "weighted",
        syntax: "weighted([value, weight], ...)",
        summary: "Picks one of its `[value, weight]` pairs at random for each generated trace, with probability proportional to its weight. Values may be any value, including nested functions; weights are constant non-negative numbers that need not sum to 1.",
        examples: &[
            "weighted([\"gpt-4o\", 8], [\"gpt-4o-mini\", 2])",
            "weighted([range(1, 3), 0.9], [10, 0.1])",
        ],
        rules: WEIGHTED_RULES,
    },
    FuncDesc {
        id: ids::FUNC_NORMAL,
        name: "normal",
        syntax: "normal(mean, stddev)",
        summary: "Samples a float from a normal (Gaussian) distribution with the given mean and standard deviation for each generated trace.",
        examples: &["normal(0.7, 0.1)", "clamp(normal(100, 15), 0, 200)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_LOGNORMAL,
        name: "lognormal",
        syntax: "lognormal(median, sigma)",
        summary: "Samples a positive float from a log-normal distribution for each generated trace, parameterized by its median and the sigma of the underlying normal; `lognormal(300, 0.5)` centers near 300 with a long right tail.",
        examples: &["lognormal(300, 0.5)", "clamp(lognormal(120, 0.8), 10, 30000)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_EXPONENTIAL,
        name: "exponential",
        syntax: "exponential(mean)",
        summary: "Samples a positive float from an exponential distribution with the given mean for each generated trace.",
        examples: &["exponential(250)", "round(exponential(30))"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_PARETO,
        name: "pareto",
        syntax: "pareto(min, shape)",
        summary: "Samples a heavy-tailed float at least min from a Pareto distribution for each generated trace; smaller shapes produce heavier tails.",
        examples: &["pareto(100, 1.5)", "round(pareto(50, 2))"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_BETA,
        name: "beta",
        syntax: "beta(alpha, beta)",
        summary: "Samples a float between 0 and 1 from a beta distribution with the given shape parameters for each generated trace.",
        examples: &["beta(2, 5)", "round(beta(8, 2) * 100)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_POISSON,
        name: "poisson",
        syntax: "poisson(mean)",
        summary: "Samples a non-negative integer count from a Poisson distribution with the given mean for each generated trace.",
        examples: &["poisson(3)", "poisson(0.4)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_CHANCE,
        name: "chance",
        syntax: "chance(probability)",
        summary: "Produces true with the given probability for each generated trace, false otherwise.",
        examples: &["chance(0.1)", "chance(0.9) ? \"hit\" : \"miss\""],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_UPPER,
        name: "upper",
        syntax: "upper(text)",
        summary: "Uppercases a string for each generated trace.",
        examples: &["upper(\"info\")", "upper(choice(\"get\", \"post\"))"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_LOWER,
        name: "lower",
        syntax: "lower(text)",
        summary: "Lowercases a string for each generated trace.",
        examples: &["lower(var.model)", "lower(\"WARN\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_TRIM,
        name: "trim",
        syntax: "trim(text)",
        summary: "Removes leading and trailing whitespace from a string for each generated trace.",
        examples: &["trim(\"  padded  \")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_REPLACE,
        name: "replace",
        syntax: "replace(text, from, to)",
        summary: "Replaces every occurrence of from with to in a string for each generated trace; an empty from leaves the string unchanged.",
        examples: &["replace(var.model, \"gpt-\", \"\")", "replace(\"a b c\", \" \", \"-\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_SPLIT,
        name: "split",
        syntax: "split(text, separator)",
        summary: "Splits a string around each occurrence of the non-empty separator for each generated trace, producing an array of strings.",
        examples: &["split(\"a,b,c\", \",\")", "split(var.path, \"/\")[0]"],
        rules: SPLIT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_JOIN,
        name: "join",
        syntax: "join(array, separator)",
        summary: "Joins the elements of an array into one string with the separator between them for each generated trace; string, number, and boolean elements are stringified.",
        examples: &["join(var.tags, \", \")", "join([1, 2, 3], \"-\")"],
        rules: JOIN_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_CONTAINS,
        name: "contains",
        syntax: "contains(target, needle)",
        summary: "True when a string target contains needle as a substring, or an array target contains needle as an element, for each generated trace.",
        examples: &["contains(var.model, \"mini\")", "contains(var.tags, \"prod\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_STARTS_WITH,
        name: "starts_with",
        syntax: "starts_with(text, prefix)",
        summary: "True when a string starts with the given prefix, for each generated trace.",
        examples: &["starts_with(var.model, \"gpt\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_ENDS_WITH,
        name: "ends_with",
        syntax: "ends_with(text, suffix)",
        summary: "True when a string ends with the given suffix, for each generated trace.",
        examples: &["ends_with(var.model, \"-mini\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_LEN,
        name: "len",
        syntax: "len(value)",
        summary: "The number of characters in a string or elements in an array, as an integer, for each generated trace.",
        examples: &["len(var.models)", "len(\"hello\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_FORMAT,
        name: "format",
        syntax: "format(template, value, ...)",
        summary: "Replaces each `{}` placeholder in a constant string template with the corresponding argument, stringified, in order, for each generated trace. Unlike `${...}` interpolation, arguments may be any expression, including functions.",
        examples: &["format(\"model={} tokens={}\", var.model, range(80, 400))"],
        rules: FORMAT_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_CLAMP,
        name: "clamp",
        syntax: "clamp(value, min, max)",
        summary: "Limits a number to the inclusive range [min, max] for each generated trace. Three integers produce an integer; otherwise a float.",
        examples: &["clamp(lognormal(300, 0.7), 20, 30000)", "clamp(var.temperature, 0.0, 1.0)"],
        rules: CLAMP_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_ROUND,
        name: "round",
        syntax: "round(value)",
        summary: "Rounds a number to the nearest integer for each generated trace; integers pass through unchanged.",
        examples: &["round(lognormal(120, 0.5))", "round(var.score * 100)"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_FLOOR,
        name: "floor",
        syntax: "floor(value)",
        summary: "Rounds a number down to the nearest integer for each generated trace; integers pass through unchanged.",
        examples: &["floor(exponential(4))"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_CEIL,
        name: "ceil",
        syntax: "ceil(value)",
        summary: "Rounds a number up to the nearest integer for each generated trace; integers pass through unchanged.",
        examples: &["ceil(range(0.1, 4.9))"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_ABS,
        name: "abs",
        syntax: "abs(value)",
        summary: "The absolute value of a number, for each generated trace. An integer produces an integer; a float produces a float.",
        examples: &["abs(normal(0, 5))"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_MIN,
        name: "min",
        syntax: "min(value, value, ...)",
        summary: "The smallest of two or more numbers, for each generated trace. All integers produce an integer; otherwise a float.",
        examples: &["min(range(1, 10), 5)", "min(var.limit, poisson(6), 100)"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_MAX,
        name: "max",
        syntax: "max(value, value, ...)",
        summary: "The largest of two or more numbers, for each generated trace. All integers produce an integer; otherwise a float.",
        examples: &["max(normal(50, 20), 0)"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        id: ids::FUNC_UUID,
        name: "uuid",
        syntax: "uuid()",
        summary: "A random version-4 UUID string for each generated trace, drawn from the seeded generator so runs stay reproducible.",
        examples: &["uuid()"],
        rules: UUID_RULES,
    },
    FuncDesc {
        id: ids::FUNC_HEX,
        name: "hex",
        syntax: "hex(length)",
        summary: "A random lowercase hexadecimal string of the given constant length, for each generated trace.",
        examples: &["hex(16)", "format(\"req_{}\", hex(8))"],
        rules: RANDOM_STRING_RULES,
    },
    FuncDesc {
        id: ids::FUNC_ALPHANUM,
        name: "alphanum",
        syntax: "alphanum(length)",
        summary: "A random alphanumeric string (0-9, A-Z, a-z) of the given constant length, for each generated trace.",
        examples: &["alphanum(12)"],
        rules: RANDOM_STRING_RULES,
    },
];

const BLOCKS: &[BlockDesc] = &[
    BlockDesc {
        id: ids::VARS,
        keyword: "vars",
        summary: "A block of named values shared across the shape via `var.<name>` references.",
        syntax: "vars { <name> = <value> ... }",
        name: NamePolicy::Forbidden,
        allowed_in: ROOT_ONLY,
        body: BodyDesc { fields: &[], open: true },
        rules: VARS_RULES,
        conventions: NO_CONVENTIONS,
    },
    BlockDesc {
        id: ids::TRACE,
        keyword: "trace",
        summary: "A named root trace containing fields and nested spans.",
        syntax: "trace \"<name>\" { ... }",
        name: NamePolicy::Required,
        allowed_in: ROOT_ONLY,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
        conventions: TRACE_CONVENTIONS,
    },
    BlockDesc {
        id: ids::TASK,
        keyword: "task",
        summary: "A named task span containing fields and nested spans.",
        syntax: "task \"<name>\" { ... }",
        name: NamePolicy::Required,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
        conventions: TASK_CONVENTIONS,
    },
    BlockDesc {
        id: ids::LLM,
        keyword: "llm",
        summary: "A named LLM span containing fields and nested spans.",
        syntax: "llm \"<name>\" { ... }",
        name: NamePolicy::Required,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
        conventions: LLM_CONVENTIONS,
    },
    BlockDesc {
        id: ids::TOOL,
        keyword: "tool",
        summary: "A named tool span containing fields and nested spans.",
        syntax: "tool \"<name>\" { ... }",
        name: NamePolicy::Required,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
        conventions: TOOL_CONVENTIONS,
    },
    BlockDesc {
        id: ids::FUNCTION,
        keyword: "function",
        summary: "A named function span containing fields and nested spans.",
        syntax: "function \"<name>\" { ... }",
        name: NamePolicy::Required,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: SPAN_FIELDS,
            open: false,
        },
        rules: NO_RULES,
        conventions: FUNCTION_CONVENTIONS,
    },
    BlockDesc {
        id: ids::REPEAT,
        keyword: "repeat",
        summary: "A dynamic block that stamps out its child blocks `count` times for each generated trace.",
        syntax: "repeat [\"<name>\"] { count = <number> ... }",
        name: NamePolicy::Optional,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: REPEAT_FIELDS,
            open: false,
        },
        rules: REPEAT_RULES,
        conventions: REPEAT_CONVENTIONS,
    },
    BlockDesc {
        id: ids::CHOICE,
        keyword: "choice",
        summary: "A dynamic block that includes exactly one of its child blocks, picked uniformly at random for each generated trace.",
        syntax: "choice [\"<name>\"] { ... }",
        name: NamePolicy::Optional,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: &[],
            open: false,
        },
        rules: NO_RULES,
        conventions: CHOICE_CONVENTIONS,
    },
    BlockDesc {
        id: ids::MAYBE,
        keyword: "maybe",
        summary: "A dynamic block that includes its child blocks with probability `chance` for each generated trace.",
        syntax: "maybe [\"<name>\"] { [chance = <number>] ... }",
        name: NamePolicy::Optional,
        allowed_in: IN_TRACE_SPAN_OR_DYNAMIC,
        body: BodyDesc {
            fields: MAYBE_FIELDS,
            open: false,
        },
        rules: MAYBE_RULES,
        conventions: MAYBE_CONVENTIONS,
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
    RuleDesc {
        id: ids::DYNAMIC_CHILDREN,
        summary: "A dynamic block (`repeat`, `choice`, or `maybe`) must contain at least one child block.",
    },
];

const EXAMPLES: &[Example] = &[
    Example {
        id: ids::MULTI_TURN_CONVERSATION,
        summary: "A multi-turn conversation drawn from a pool of authored alternatives.",
        note: "A starting point, not a template: widen the pool with more choice alternatives and let their lengths differ — real sessions cluster short with a long tail of long ones.",
        source: include_str!("../../examples/multi_turn_conversation.bt"),
        valid: true,
    },
    Example {
        id: ids::AGENT_TOOL_LOOP,
        summary: "An agent loop running a sampled number of think-then-tool rounds.",
        note: "Expand along loop iterations and the set of tools; every tool the agent could realistically call deserves an alternative in the choice.",
        source: include_str!("../../examples/agent_tool_loop.bt"),
        valid: true,
    },
    Example {
        id: ids::SUPERVISOR_AND_SUBAGENTS,
        summary: "A supervisor delegating to subagent task spans and synthesizing their results.",
        note: "Expand along the number of subagents and the delegation depth — a subagent can nest task, tool, and llm spans of its own.",
        source: include_str!("../../examples/supervisor_and_subagents.bt"),
        valid: true,
    },
    Example {
        id: ids::FANOUT_PARALLEL_WORKERS,
        summary: "A fanout stamping a sampled number of parallel workers, then reducing.",
        note: "Expand along fan width and worker heterogeneity; add choice alternatives so stamped workers stop being clones of each other.",
        source: include_str!("../../examples/fanout_parallel_workers.bt"),
        valid: true,
    },
    Example {
        id: ids::WINDOWED_SESSION,
        summary: "One window of a longer session, with earlier windows summarized into context.",
        note: "Expand along window position and carried context: more window variants, richer summaries, sessions that reference older windows.",
        source: include_str!("../../examples/windowed_session.bt"),
        valid: true,
    },
    Example {
        id: ids::RAG_PIPELINE,
        summary: "A retrieve, rerank, and generate pipeline grounding an answer in documents.",
        note: "Expand along retrieved document count, hit-versus-miss retrieval quality, and extra stages like query rewriting or guardrails.",
        source: include_str!("../../examples/rag_pipeline.bt"),
        valid: true,
    },
    Example {
        id: ids::ERROR_AND_ESCALATION,
        summary: "A happy path with a maybe-gated failure, retry, and human escalation.",
        note: "Expand along failure rate and failure variety — declines, timeouts, empty outputs — keeping each correlated failure inside one maybe block.",
        source: include_str!("../../examples/error_and_escalation.bt"),
        valid: true,
    },
];

pub(crate) static SPEC: Spec = Spec {
    schema_version: 1,
    language_version: 1,
    name: "bts",
    summary: "A declarative language for describing synthetic traces and spans.",
    surface: SurfaceDesc {
        notation: "EBNF",
        grammar: r##"
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
object         = "{", [ object_items | for_object ], "}" ;
object_items   = object_item, { ( "," | NEWLINE ), object_item }, [ "," ] ;
object_item    = attribute | "...", expression ;
for_array      = "for", bindings, "in", expression, ":", expression, [ "if", expression ] ;
for_object     = "for", bindings, "in", expression, ":", expression, "=>", expression, [ "if", expression ] ;
bindings       = identifier, [ ",", identifier ] ;
identifier     = ASCII_ALPHA, { ASCII_ALPHA | ASCII_DIGIT | "_" } ;
number         = ASCII_DIGIT, { ASCII_DIGIT }, [ ".", ASCII_DIGIT, { ASCII_DIGIT } ] ;
string         = quoted | multiline | heredoc ;
quoted         = '"', { ANY_EXCEPT_DOUBLE_QUOTE_OR_INTERPOLATION | escape | interpolation }, '"' ;
multiline      = '"""', NEWLINE, { ANY_EXCEPT_TRIPLE_QUOTE | escape | interpolation }, '"""' ;
heredoc        = "<<", [ "-" ], identifier, NEWLINE, { ANY | escape | interpolation }, NEWLINE, [ WHITESPACE ], identifier ;
escape         = "$${" ;
interpolation  = "${", reference, "}" ;
reference      = identifier, { ".", identifier } ;
comment        = ( "#" | "//" ), { ANY_EXCEPT_NEWLINE } ;
"##,
        notes: &[
            "Whitespace separates tokens and is otherwise insignificant, except that a line break can separate object items in place of a comma.",
            "The surface grammar accepts declarations generically; block placement and field types are semantic rules.",
            "`#` and `//` start a comment running to the end of the line; comments behave as whitespace and are literal text inside strings. There are no block comments.",
            "`<<` opens a heredoc only when its delimiter word (optionally after `-`) follows directly; anything else lexes as comparison operators.",
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
