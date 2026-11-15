//! Machine-readable graph language rules and expression contracts.
use serde::Serialize;

/// The language reference used by documentation and validation tools.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Spec {
    /// Serialized reference format version.
    pub schema_version: u32,
    /// Language syntax version.
    pub language_version: u32,
    /// Language name.
    pub name: &'static str,
    /// Language purpose.
    pub summary: &'static str,
    /// Source grammar and layout.
    pub surface: SurfaceDesc,
    /// Expression forms.
    pub expressions: &'static [ExprDesc],
    /// Built-in function contracts.
    pub functions: &'static [FuncDesc],
    /// Default graph primitive contracts.
    pub primitives: &'static [PrimitiveDesc],
    /// Language-wide validation rules.
    pub rules: &'static [RuleDesc],
    /// Compilation examples and expected outcomes.
    pub examples: &'static [Example],
}
impl Spec {
    /// Looks up a built-in kind within its graph construct.
    pub fn primitive(&self, construct: &str, name: &str) -> Option<&PrimitiveDesc> {
        self.primitives
            .iter()
            .find(|primitive| primitive.construct == construct && primitive.name == name)
    }
    /// Looks up a built-in function.
    pub fn function(&self, name: &str) -> Option<&FuncDesc> {
        self.functions.iter().find(|f| f.name == name)
    }
    /// Looks up a language rule by its stable identifier.
    pub fn rule(&self, id: Id) -> Option<&RuleDesc> {
        self.rules
            .iter()
            .chain(self.expressions.iter().flat_map(|e| e.rules))
            .chain(self.functions.iter().flat_map(|f| f.rules))
            .find(|r| r.id == id)
    }
}
/// A default behavior associated with a graph construct.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct PrimitiveDesc {
    /// Kind name.
    pub name: &'static str,
    /// Graph construct that recognizes the primitive.
    pub construct: &'static str,
    /// Structural and behavioral contract.
    pub summary: &'static str,
}
/// A stable textual identifier for an entry in the language reference.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Id(&'static str);

impl Id {
    /// Wraps a static identifier string without checking whether it exists in the reference.
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    /// Returns the underlying identifier string.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The notation, grammar, and general notes for source text.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct SurfaceDesc {
    /// The notation used by the grammar.
    pub notation: &'static str,
    /// The source grammar as text.
    pub grammar: &'static str,
    /// Additional guidance for readers.
    pub notes: &'static [&'static str],
}

/// Documentation for one supported expression form.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct ExprDesc {
    /// Reference labels such as feature availability tags.
    pub tags: &'static [&'static str],
    /// The stable reference identifier.
    pub id: Id,
    /// The documented source form.
    pub syntax: &'static str,
    /// A short explanation for readers.
    pub summary: &'static str,
    /// Examples demonstrating this entry.
    pub examples: &'static [&'static str],
    /// Rules associated with this entry.
    pub rules: &'static [RuleDesc],
}

/// Documentation for a language function and its constraints.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct FuncDesc {
    /// Reference labels such as feature availability tags.
    pub tags: &'static [&'static str],
    /// The stable reference identifier.
    pub id: Id,
    /// The name written in source.
    pub name: &'static str,
    /// The documented source form.
    pub syntax: &'static str,
    /// A short explanation for readers.
    pub summary: &'static str,
    /// Examples demonstrating this entry.
    pub examples: &'static [&'static str],
    /// Rules associated with this entry.
    pub rules: &'static [RuleDesc],
}

/// A validation rule in the language reference.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct RuleDesc {
    /// Reference labels such as feature availability tags.
    pub tags: &'static [&'static str],
    /// The stable reference identifier.
    pub id: Id,
    /// A short explanation for readers.
    pub summary: &'static str,
}

/// An example source module and its expected validity.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct Example {
    /// Reference labels such as feature availability tags.
    pub tags: &'static [&'static str],
    /// The stable reference identifier.
    pub id: Id,
    /// A short explanation for readers.
    pub summary: &'static str,
    /// Additional guidance about the example.
    pub note: &'static str,
    /// The complete source text for the example.
    pub source: &'static str,
    /// Whether compilation is expected to accept this example.
    pub valid: bool,
}

/// Stable identifiers for expression and function contracts.
pub mod ids {
    use super::Id;
    /// Stable language rule or function identifier.
    pub const FUNC_CHOICE: Id = Id::new("func.choice");
    /// Stable language rule or function identifier.
    pub const FUNC_RANGE: Id = Id::new("func.range");
    /// Stable language rule or function identifier.
    pub const FUNC_WEIGHTED: Id = Id::new("func.weighted");
    /// Stable language rule or function identifier.
    pub const FUNC_NORMAL: Id = Id::new("func.normal");
    /// Stable language rule or function identifier.
    pub const FUNC_LOGNORMAL: Id = Id::new("func.lognormal");
    /// Stable language rule or function identifier.
    pub const FUNC_EXPONENTIAL: Id = Id::new("func.exponential");
    /// Stable language rule or function identifier.
    pub const FUNC_PARETO: Id = Id::new("func.pareto");
    /// Stable language rule or function identifier.
    pub const FUNC_BETA: Id = Id::new("func.beta");
    /// Stable language rule or function identifier.
    pub const FUNC_POISSON: Id = Id::new("func.poisson");
    /// Stable language rule or function identifier.
    pub const FUNC_CHANCE: Id = Id::new("func.chance");
    /// Stable language rule or function identifier.
    pub const FUNC_UPPER: Id = Id::new("func.upper");
    /// Stable language rule or function identifier.
    pub const FUNC_LOWER: Id = Id::new("func.lower");
    /// Stable language rule or function identifier.
    pub const FUNC_TRIM: Id = Id::new("func.trim");
    /// Stable language rule or function identifier.
    pub const FUNC_REPLACE: Id = Id::new("func.replace");
    /// Stable language rule or function identifier.
    pub const FUNC_SPLIT: Id = Id::new("func.split");
    /// Stable language rule or function identifier.
    pub const FUNC_RENDER: Id = Id::new("func.render");
    /// Stable language rule or function identifier.
    pub const FUNC_COMPOSE: Id = Id::new("func.compose");
    /// Stable language rule or function identifier.
    pub const FUNC_SAMPLE: Id = Id::new("func.sample");
    /// Stable language rule or function identifier.
    pub const FUNC_REALIZE: Id = Id::new("func.realize");
    /// Stable language rule or function identifier.
    pub const FUNC_VARY: Id = Id::new("func.vary");
    /// Stable language rule or function identifier.
    pub const FUNC_EXPRESS: Id = Id::new("func.express");
    /// Stable language rule or function identifier.
    pub const FUNC_EXPRESSION: Id = Id::new("func.expression");
    /// Stable language rule or function identifier.
    pub const FUNC_EXPRESSION_CANDIDATES: Id = Id::new("func.expression_candidates");
    /// Stable language rule or function identifier.
    pub const FUNC_COMBINE: Id = Id::new("func.combine");
    /// Stable language rule or function identifier.
    pub const FUNC_REFER: Id = Id::new("func.refer");
    /// Stable language rule or function identifier.
    pub const FUNC_REMEMBER: Id = Id::new("func.remember");
    /// Stable language rule or function identifier.
    pub const FUNC_UNMENTIONED: Id = Id::new("func.unmentioned");
    /// Stable language rule or function identifier.
    pub const FUNC_FORMAT_NUMBER: Id = Id::new("func.format_number");
    /// Stable language rule or function identifier.
    pub const FUNC_FORMAT_DATE: Id = Id::new("func.format_date");
    /// Stable language rule or function identifier.
    pub const FUNC_FORMAT_TIME: Id = Id::new("func.format_time");
    /// Stable language rule or function identifier.
    pub const FUNC_JOIN: Id = Id::new("func.join");
    /// Stable language rule or function identifier.
    pub const FUNC_CONTAINS: Id = Id::new("func.contains");
    /// Stable language rule or function identifier.
    pub const FUNC_STARTS_WITH: Id = Id::new("func.starts_with");
    /// Stable language rule or function identifier.
    pub const FUNC_ENDS_WITH: Id = Id::new("func.ends_with");
    /// Stable language rule or function identifier.
    pub const FUNC_LEN: Id = Id::new("func.len");
    /// Stable language rule or function identifier.
    pub const FUNC_TOKENS: Id = Id::new("func.tokens");
    /// Stable language rule or function identifier.
    pub const FUNC_FORMAT: Id = Id::new("func.format");
    /// Stable language rule or function identifier.
    pub const FUNC_CLAMP: Id = Id::new("func.clamp");
    /// Stable language rule or function identifier.
    pub const FUNC_ROUND: Id = Id::new("func.round");
    /// Stable language rule or function identifier.
    pub const FUNC_FLOOR: Id = Id::new("func.floor");
    /// Stable language rule or function identifier.
    pub const FUNC_CEIL: Id = Id::new("func.ceil");
    /// Stable language rule or function identifier.
    pub const FUNC_ABS: Id = Id::new("func.abs");
    /// Stable language rule or function identifier.
    pub const FUNC_MIN: Id = Id::new("func.min");
    /// Stable language rule or function identifier.
    pub const FUNC_MAX: Id = Id::new("func.max");
    /// Stable language rule or function identifier.
    pub const FUNC_UUID: Id = Id::new("func.uuid");
    /// Stable language rule or function identifier.
    pub const FUNC_HEX: Id = Id::new("func.hex");
    /// Stable language rule or function identifier.
    pub const FUNC_ALPHANUM: Id = Id::new("func.alphanum");
    /// Stable language rule or function identifier.
    pub const FUNC_NOISE: Id = Id::new("func.noise");
    /// Stable language rule or function identifier.
    pub const FUNC_ATTACHMENT: Id = Id::new("func.attachment");
    /// Stable language rule or function identifier.
    pub const SCALAR_INTERPOLATION: Id = Id::new("rule.scalar-interpolation");
    /// Stable language rule or function identifier.
    pub const UNIQUE_OBJECT_KEYS: Id = Id::new("rule.unique-object-keys");
    /// Stable language rule or function identifier.
    pub const FINITE_NUMBERS: Id = Id::new("rule.finite-numbers");
    /// Stable language rule or function identifier.
    pub const KNOWN_FUNCTIONS: Id = Id::new("rule.known-functions");
    /// Stable language rule or function identifier.
    pub const CHOICE_ALTERNATIVES: Id = Id::new("rule.choice-alternatives");
    /// Stable language rule or function identifier.
    pub const RANGE_BOUNDS: Id = Id::new("rule.range-bounds");
    /// Stable language rule or function identifier.
    pub const FUNC_ARITY: Id = Id::new("rule.function-arity");
    /// Stable language rule or function identifier.
    pub const FUNC_ARG_TYPES: Id = Id::new("rule.function-argument-types");
    /// Stable language rule or function identifier.
    pub const WEIGHTED_OPTIONS: Id = Id::new("rule.weighted-options");
    /// Stable language rule or function identifier.
    pub const DIST_PARAMS: Id = Id::new("rule.distribution-params");
    /// Stable language rule or function identifier.
    pub const FORMAT_TEMPLATE: Id = Id::new("rule.format-template");
    /// Stable language rule or function identifier.
    pub const SPLIT_SEPARATOR: Id = Id::new("rule.split-separator");
    /// Stable language rule or function identifier.
    pub const JOIN_ELEMENTS: Id = Id::new("rule.join-elements");
    /// Stable language rule or function identifier.
    pub const CLAMP_BOUNDS: Id = Id::new("rule.clamp-bounds");
    /// Stable language rule or function identifier.
    pub const INTEGER_RESULTS: Id = Id::new("rule.integer-results");
    /// Stable language rule or function identifier.
    pub const RANDOM_LENGTH: Id = Id::new("rule.random-string-length");
    /// Stable language rule or function identifier.
    pub const NOISE_SIZE: Id = Id::new("rule.noise-size");
    /// Stable language rule or function identifier.
    pub const OPERAND_TYPES: Id = Id::new("rule.operand-types");
    /// Stable language rule or function identifier.
    pub const INDEXABLE_TARGETS: Id = Id::new("rule.indexable-targets");
    /// Stable language rule or function identifier.
    pub const INDEX_BOUNDS: Id = Id::new("rule.index-bounds");
    /// Stable language rule or function identifier.
    pub const SLICEABLE_TARGETS: Id = Id::new("rule.sliceable-targets");
    /// Stable language rule or function identifier.
    pub const SLICE_BOUNDS: Id = Id::new("rule.slice-bounds");
    /// Stable language rule or function identifier.
    pub const SPREAD_OPERANDS: Id = Id::new("rule.spread-operands");
    /// Stable language rule or function identifier.
    pub const FOR_COLLECTIONS: Id = Id::new("rule.for-collections");
    /// Stable language rule or function identifier.
    pub const STATIC_FOR: Id = Id::new("rule.static-for");
    /// Stable language rule or function identifier.
    pub const BOOLEAN_CONDITIONS: Id = Id::new("rule.boolean-conditions");
    /// Stable language rule or function identifier.
    pub const NONZERO_DIVISORS: Id = Id::new("rule.nonzero-divisors");
    /// Stable language rule or function identifier.
    pub const STATIC_STRUCTURE: Id = Id::new("rule.static-structure");
}
const FINITE_NUMBERS_RULE: RuleDesc = RuleDesc {
    tags: &[],
    id: ids::FINITE_NUMBERS,
    summary: "Every number must be representable as a finite integer or floating-point value.",
};

const CHOICE_RULES: &[RuleDesc] = &[RuleDesc {
    tags: &[],
    id: ids::CHOICE_ALTERNATIVES,
    summary: "`choice` takes at least one alternative.",
}];

const RANGE_RULES: &[RuleDesc] = &[RuleDesc {
    tags: &[],
    id: ids::RANGE_BOUNDS,
    summary: "`range` takes exactly two finite numbers with min <= max.",
}];

const FUNC_ARITY_RULE: RuleDesc = RuleDesc {
    tags: &[],
    id: ids::FUNC_ARITY,
    summary: "A function takes the arguments its syntax documents; `choice`, `weighted`, `min`, `max`, and `format` accept a variable number. `compose` accepts an optional separator.",
};

const FUNC_ARG_TYPES_RULE: RuleDesc = RuleDesc {
    tags: &[],
    id: ids::FUNC_ARG_TYPES,
    summary: "A function argument must have the documented type, known before evaluation; an argument whose type is only known during evaluation (e.g. a `choice` with mixed alternatives) is rejected.",
};

const DIST_PARAMS_RULE: RuleDesc = RuleDesc {
    tags: &[],
    id: ids::DIST_PARAMS,
    summary: "Distribution parameters are constant finite numbers: `normal` takes stddev >= 0, `lognormal` takes median > 0 and sigma >= 0, `exponential` and `poisson` take mean > 0, `pareto` takes min > 0 and shape > 0, `beta` takes alpha > 0 and beta > 0, and `chance` takes a probability between 0 and 1.",
};

const SAMPLING_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, DIST_PARAMS_RULE, FINITE_NUMBERS_RULE];

const WEIGHTED_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    RuleDesc {
        tags: &[],
        id: ids::WEIGHTED_OPTIONS,
        summary: "`weighted` takes at least one `[value, weight]` pair; weights are constant non-negative finite numbers, at least one of them positive.",
    },
];

const TEXT_FUNC_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE];

const SPLIT_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::SPLIT_SEPARATOR,
        summary: "`split` requires a non-empty separator; a constant empty separator is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
];

const JOIN_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::JOIN_ELEMENTS,
        summary: "`join` stringifies string, number, and boolean elements; any other element fails the run during evaluation.",
    },
];

const FORMAT_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::FORMAT_TEMPLATE,
        summary: "`format` takes a constant string template with exactly one `{}` placeholder per remaining argument, replaced in order.",
    },
];

const NUMERIC_FUNC_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE, FINITE_NUMBERS_RULE];

const CLAMP_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::CLAMP_BOUNDS,
        summary: "`clamp` bounds must satisfy min <= max; a constant violation is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
];

const ROUNDING_FUNC_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::INTEGER_RESULTS,
        summary: "`round`, `floor`, and `ceil` produce integers; a result outside the 64-bit integer range fails the run during evaluation.",
    },
];

const RANDOM_STRING_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    RuleDesc {
        tags: &[],
        id: ids::RANDOM_LENGTH,
        summary: "`hex` and `alphanum` take a constant non-negative integer length.",
    },
];

const UUID_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE];

const NOISE_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: ids::NOISE_SIZE,
        summary: "`noise` takes an integer size between 0 and 8388608 (8 MiB); a constant violation is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
];

const FUNCS: &[FuncDesc] = &[
    FuncDesc {
        tags: &[],
        id: ids::FUNC_CHOICE,
        name: "choice",
        syntax: "choice(value, ...)",
        summary: "Picks one of its alternatives uniformly at random on evaluation. Alternatives may be any value, including nested functions.",
        examples: &["choice(\"gpt-4o\", \"gpt-4o-mini\")", "choice(1, 2, range(5, 9))"],
        rules: CHOICE_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_RANGE,
        name: "range",
        syntax: "range(min, max)",
        summary: "Samples a number uniformly between min and max (inclusive) on evaluation. Two integer bounds sample an integer; otherwise a float.",
        examples: &["range(80, 400)", "range(0.0, 1.0)"],
        rules: RANGE_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_WEIGHTED,
        name: "weighted",
        syntax: "weighted([value, weight], ...)",
        summary: "Picks one of its `[value, weight]` pairs at random on evaluation, with probability proportional to its weight. Values may be any value, including nested functions; weights are constant non-negative numbers that need not sum to 1.",
        examples: &[
            "weighted([|\"gpt-4o\", 8|], [|\"gpt-4o-mini\", 2|])",
            "weighted([|range(1, 3), 0.9|], [|10, 0.1|])",
        ],
        rules: WEIGHTED_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_NORMAL,
        name: "normal",
        syntax: "normal(mean, stddev)",
        summary: "Samples a float from a normal (Gaussian) distribution with the given mean and standard deviation on evaluation.",
        examples: &["normal(0.7, 0.1)", "clamp(normal(100, 15), 0, 200)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_LOGNORMAL,
        name: "lognormal",
        syntax: "lognormal(median, sigma)",
        summary: "Samples a positive float from a log-normal distribution on evaluation, parameterized by its median and the sigma of the underlying normal; `lognormal(300, 0.5)` centers near 300 with a long right tail.",
        examples: &["lognormal(300, 0.5)", "clamp(lognormal(120, 0.8), 10, 30000)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_EXPONENTIAL,
        name: "exponential",
        syntax: "exponential(mean)",
        summary: "Samples a positive float from an exponential distribution with the given mean on evaluation.",
        examples: &["exponential(250)", "round(exponential(30))"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_PARETO,
        name: "pareto",
        syntax: "pareto(min, shape)",
        summary: "Samples a heavy-tailed float at least min from a Pareto distribution on evaluation; smaller shapes produce heavier tails.",
        examples: &["pareto(100, 1.5)", "round(pareto(50, 2))"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_BETA,
        name: "beta",
        syntax: "beta(alpha, beta)",
        summary: "Samples a float between 0 and 1 from a beta distribution with the given shape parameters on evaluation.",
        examples: &["beta(2, 5)", "round(beta(8, 2) * 100)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_POISSON,
        name: "poisson",
        syntax: "poisson(mean)",
        summary: "Samples a non-negative integer count from a Poisson distribution with the given mean on evaluation.",
        examples: &["poisson(3)", "poisson(0.4)"],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_CHANCE,
        name: "chance",
        syntax: "chance(probability)",
        summary: "Produces true with the given probability on evaluation, false otherwise.",
        examples: &["chance(0.1)", "chance(0.9) ? \"hit\" : \"miss\""],
        rules: SAMPLING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_UPPER,
        name: "upper",
        syntax: "upper(text)",
        summary: "Uppercases a string on evaluation.",
        examples: &["upper(\"info\")", "upper(choice(\"get\", \"post\"))"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_LOWER,
        name: "lower",
        syntax: "lower(text)",
        summary: "Lowercases a string on evaluation.",
        examples: &["lower(settings.model)", "lower(\"WARN\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_TRIM,
        name: "trim",
        syntax: "trim(text)",
        summary: "Removes leading and trailing whitespace from a string on evaluation.",
        examples: &["trim(\"  padded  \")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_REPLACE,
        name: "replace",
        syntax: "replace(text, from, to)",
        summary: "Replaces every occurrence of from with to in a string on evaluation; an empty from leaves the string unchanged.",
        examples: &["replace(settings.model, \"gpt-\", \"\")", "replace(\"a b c\", \" \", \"-\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_SPLIT,
        name: "split",
        syntax: "split(text, separator)",
        summary: "Splits a string around each occurrence of the non-empty separator on evaluation, producing an array of strings.",
        examples: &["split(\"a,b,c\", \",\")", "split(settings.path, \"/\")[0]"],
        rules: SPLIT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_RENDER,
        name: "render",
        syntax: "render(template, values)",
        summary: "Substitutes {name} placeholders with string, number, or boolean values from an object. Double braces escape literal braces. Missing names, non-scalar replacements, empty names, and unmatched braces fail during evaluation. Extra values are allowed and replacements are not recursively rendered. Numeric rendering preserves authored float types, including a trailing .0 for whole floats and the sign of negative zero. Nonzero floats use scientific notation below 0.00001 in magnitude or at and above 1e16, with no leading zeros in the exponent.",
        examples: &[r#"render("Hello {name}!", { name: "Sam" })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_COMPOSE,
        name: "compose",
        syntax: "compose(parts[, separator])",
        summary: "Joins string parts, skipping null and empty strings. The separator defaults to a space. Whitespace and punctuation are preserved. Other element types fail during evaluation.",
        examples: &[r#"compose([|"Hi!", null, "How can I help?"|])"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_SAMPLE,
        name: "sample",
        syntax: "sample(array, count)",
        summary: "Selects array positions uniformly without replacement and returns their values in source order. Duplicate values remain distinct positions. Count must be an integer from zero through the array length, otherwise evaluation fails.",
        examples: &[r#"sample([|"parking", "keys", "hours"|], 2)"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_REALIZE,
        name: "realize",
        syntax: "realize(sentence[, options])",
        summary: "Renders English without random choices. The sentence object requires clause and accepts optional condition. A clause requires subject {text: string, person: first|second|third (default third), number: singular|plural (default singular)} and predicate {verb, complement?: string}. Verb is a packaged lowercase base word or an object containing all five lowercase forms: base, third_person_singular, past, past_participle, present_participle. Clause options are tense: present|past|future (present), aspect: simple|progressive|perfect|perfect_progressive (simple), polarity: positive|negative (positive), form: statement|question (statement), and modal: can|could|may|might|must|should|will|would (absent by default). A modal requires tense present, with temporal relationships expressed through the chosen modal and aspect. A condition requires subject and predicate and accepts tense, aspect, and polarity with the same defaults. It is introduced by if. Rendering options are negation: expanded|contracted (expanded) and condition_position: before|after (before). Authored subject agreement and explicit verb forms remain caller-owned. Unsupported structures and unknown fields fail during evaluation.",
        examples: &[
            r#"realize({ clause: { subject: { text: "the guest" }, predicate: { verb: "arrive", complement: "after midnight" }, tense: "future" } })"#,
        ],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_EXPRESS,
        name: "express",
        syntax: "express(source[, options])",
        summary: "Generates English from author-approved expressions of an intent. source contains intent: nonempty identifier, families: nonempty array, constructions: array (default []), facts: [{id,text}] (default []), optional_details: fact IDs (default []), and protected: literal strings (default []). A construction is {id,predicate:{verb,complement?},weight:1,requires:[],excludes:[],mentions:[]}. Verb accepts the same known base string or explicit five forms as realize. Compatible verbs, objects, particles, and complements stay together as one construction. A family is {id,kind,weight:1,constructions:[],patterns:[],requires:[],excludes:[],context_requires:[],establishes:[],mentions:[],reason:null,text:null}. kind is request, desire, possibility, procedure, need, reason_request, follow_up, or authored. Built-in families use a selected action in request, first-person, or procedural constructions. Empty construction and pattern lists allow all compatible choices. requires and mentions need supplied fact IDs, excludes rejects supplied fact IDs, and context_requires needs established context markers. reason_request needs reason: a supplied fact ID. authored and follow_up need complete text and cannot specify constructions, patterns, or reason. follow_up also requires nonempty context_requires. Author-supplied alternatives are trusted to express the intended meaning. No semantic equivalence or hidden circumstances are inferred. Use render, realize, and ordinary bts choices to prepare domain-specific text and circumstances. options contains speaker, context, and repeat_penalty (default 0.25, range 0..1). speaker contains wording: vary style controls (default neutral), family_weights and pattern_weights: identifier-to-weight objects, greetings and closings: [{text,weight:1}], greeting_probability (0.5), closing_probability (0), detail_probability (0.5), and max_details (1, integer 0..16). All probabilities are in 0..1 and weights in 0..1000000. Profiles are ordinary values that can be sampled once and reused across turns. Greetings apply only with empty history. Optional supplied details are sampled without replacement until max_details or the first failed detail gate, excluding already expressed or established facts. context contains established: identifiers and recent: selection records, both default empty. Family and construction repeats within the same intent are multiplied by repeat_penalty, with zero excluding repeats. Selection normalizes weights by family, construction, and pattern, independent of vocabulary size. Evaluation fails if no positive-weight choice is eligible. No random draw is consumed for a sole choice or probability zero or one. Uses scoped seeded randomness and performs no network calls. For composable messages, source also accepts topic: an explicit topic or entity identifier and components: [{id,role,forms,mentions:[],requires:[],context_requires:[],slot:null,option:null}]. Roles are current, outcome, reason, constraint, answer, previous, replacement, acknowledgment, clarification, follow_up_request, and detail. Each form is {id,text,shape:complete,reference:explicit,weight:1,context_requires:[]}. Shapes are complete, fragments, clause, and value. Clauses and values must omit terminal punctuation and are only used in recipes requiring them. Use realize to prepare grammatical component text. Contextual forms require a topic and nonempty component or form context_requires. Family message is {layout,bindings:{role:component_id},relation:null,allow_apology:false,details:[]}. Layouts are request_only, request_reason, reason_request, outcome_request, current_desired, constraint_request, answer, correction, acknowledgment, clarification, and follow_up_request. current_desired and response layouts require kind structured with no text, reason, constructions, or patterns. Other layouts wrap a regular action family or authored request. reason_request family kind cannot also use message. Message plans cannot use source optional_details. details explicitly binds detail components, with no added facts. Cause links require relation cause on request_reason or reason_request layouts and declarative first-person actions. Contrast requires relation contrast on current_desired. Without those relationships only separate sentences are eligible. Apology variants require allow_apology on correction. Every binding must match the component role. Speaker maps organization (request_first, circumstance_first, outcome_first, current_then_desired, response), elaboration (minimal, relevant_context, detailed), sentence_shape (complete, short_sentences, fragments), and reference_style (explicit, contextual) independently weight applicable choices. Omitted weights are one, zero excludes. Organization and elaboration multiply family weights. Form weights normalize within each recipe and shape and reference preferences weight eligible recipes and forms. Message plans govern fragments and disable the separate surface fragment rewrite. Context additionally accepts topic, known fact IDs, requested slot IDs, offered option IDs, and allowed_acts (answer, correction, acknowledgment, clarification, follow_up_request), all empty or absent by default. Responses require matching topic and an explicitly allowed act. Answers and clarifications require a requested slot. Component option requires an offered option ID. Corrections require previous and replacement components with matching slots and nonempty, disjoint mentions, and previous facts must already be established. Components may use supplied facts or explicitly known facts. Context never derives meaning from agent text. Message expansion is limited to 16384 choices per family.",
        examples: &[
            r#"express({ intent: "change_flight", constructions: [|{ id: "change", predicate: { verb: "change", complement: "my flight" } }|], families: [|{ id: "request", kind: "request" }, { id: "desire", kind: "desire" }|] }, { speaker: { wording: { style: "conversational" } } })"#,
        ],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_EXPRESSION,
        name: "expression",
        syntax: "expression(source[, options])",
        summary: "Uses the same inputs and random choices as express and returns an object with text, base_text before surface styling, selection: {intent,family,construction,pattern}, used_facts, edits, and context. construction is null for authored and follow_up families. edits contain rule, range with UTF-8 byte start/end offsets relative to the text immediately before that edit, replacement, and stage. The returned context adds explicit family establishes and actually expressed fact IDs, then appends the selection to recent, retaining at most 32 entries. Input context is never mutated. Reuse a speaker value and pass the returned context to later turns explicitly. Nothing stores hidden conversation state. Composable selections also contain message: {layout,recipe,forms,organization,elaboration,sentence_shape,reference_style}, with forms recorded as component_id/form_id and each identifier escaping ~ as ~0 and / as ~1. Topic-scoped outputs add expressed facts to known. Answers consume their requested slot and responses consume allowed_acts. Corrections remove previous facts from known while retaining their historical mentions in established. A changed topic clears pending requested slots, offered options, and allowed acts. The caller supplies the next agent turn context explicitly.",
        examples: &[r#"expression(settings.intent, { speaker: settings.speaker, context: settings.previous.context })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_EXPRESSION_CANDIDATES,
        name: "expression_candidates",
        syntax: "expression_candidates(source[, options])",
        summary: "Validates express inputs and lists applicable base realizations without consuming randomness. Each candidate contains selection, text, used_facts, establishes, and normalized probability. An empty array means no eligible positive-weight choice. Optional greetings, details, closings, and surface edits are not expanded. Pattern IDs can restrict families or configure speaker preferences: request_please, request_could_you, request_would_you, desire_would_like, desire_want, desire_looking_to, possibility_can_i, possibility_possible, possibility_able, procedure_how_do, procedure_how_can, procedure_go_about, need_need_to, reason_request_could_you, reason_request_please, authored, follow_up, and structured. Composable message candidates expand eligible component forms and include message selection metadata. The procedure_go_about pattern uses the supplied verb's present participle. Invalid IDs in a family's patterns or constructions fail. Speaker weight entries for unused IDs are allowed so profiles can be reused across scenarios.",
        examples: &[r#"expression_candidates(settings.intent, { speaker: settings.speaker })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_VARY,
        name: "vary",
        syntax: "vary(text_or_sentence[, options])",
        summary: "Varies English prose or a sentence object accepted by realize using scoped seeded randomness. Existing calls using only negation: boolean (true), condition_order: boolean (true), and rendering preserve their original behavior: uniform selection between the original and applicable alternatives, with unsupported prose unchanged. Adding any style control opts into bounded composition. style accepts neutral, professional, conversational, warm, terse, text_message, enthusiastic, or informal_chat. Explicit controls override preset defaults: contractions, shorthand, casual, slang, reductions, fragments, warmth, soften_requests, negation, and condition_order are booleans. New-mode negation only contracts recognized negative statements. All presets disable typos and request softening by default. punctuation accepts preserve, omit_period, exclamation, or expressive and affects the final terminal mark, preserving question marks, abbreviations, and code-like tokens. casing accepts preserve or lowercase and only lowercases reviewed everyday words and pronoun contractions. probability (default 1, range 0..1) gates wording, tone, and presentation once per message. max_changes (default 3, integer 0..16) bounds total edits. typos accepts probability (default 0, per message independently), max_errors (default 1, integer 0..16), and kinds (default all of common, omission, transposition, repetition). Typos apply only to reviewed lowercase words and share max_changes. protected is an array of nonempty case-sensitive literal strings. Quotations, code-like tokens, and authored structured subject references are protected automatically. Arbitrary prose names cannot be inferred reliably. alternatives is an array of {from: nonempty text, to: nonempty array of nonempty text} author-approved phrase families, matched literally at word boundaries. Author-supplied alternatives may authorize meaning changes. Built-in tone rules require warmth or soften_requests and never invent promises, urgency, or apologies. Fragments are limited to supported first-person progressive statements. Reductions require recognized infinitive constructions. Changes run in grammatical, phrase, tone, presentation, and typo stages with nonoverlapping choices within each stage. No draw is consumed for a zero or certain probability gate, a sole candidate, or no eligible edit. rendering applies only to structured input. Invalid options fail during evaluation.",
        examples: &[
            r#"vary("The guest cannot check in.", { negation: true, condition_order: false })"#,
            r#"vary("I am checking the file. Thank you.", { style: "text_message", max_changes: 6 })"#,
            r#"vary("please send the message", { style: "neutral", typos: { probability: 0.1, max_errors: 1 } })"#,
            r#"vary("Send the file.", { style: "professional", soften_requests: true })"#,
        ],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_COMBINE,
        name: "combine",
        syntax: "combine(sentences, relation[, options])",
        summary: "Combines an array of at least two unconditional structured statements accepted by realize. Relation is and, but, or, or because. And, but, and or preserve input order and retain each subject. Because requires exactly two inputs ordered [effect, cause] and renders Because cause, effect. It does not infer causality or substitute one relation for another. Conditional sentences and questions are rejected to avoid changing their scope. Rendering options match realize. Invalid structures or options fail during evaluation.",
        examples: &[r#"combine([|settings.arrival, settings.availability|], "and")"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_REFER,
        name: "refer",
        syntax: "refer(entity, context[, options])",
        summary: "Returns an English name or an explicitly supplied pronoun without mutating context. Entity fields are id and label (required nonempty strings), person: first|second|third (third), number: singular|plural (singular), and optional pronouns {subject, object, possessive_determiner, possessive}, all four single-word strings. Context is {entities: entity array, mentioned: entity-id string array, facts: fact-id string array}, with every array defaulting to empty. Entity definitions and ids must agree, mentioned ids must have definitions, and context ids must be unique. Options are form: subject|object|possessive_determiner|possessive (subject) and style: auto|name|pronoun (auto). Auto uses a supplied pronoun only after introduction and when unambiguous among mentioned entities, otherwise the label. Name always uses the label. Pronoun requires introduction and an unambiguous supplied form or fails. No gender or entity identity is inferred.",
        examples: &[r#"refer(settings.guest, settings.context)"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_REMEMBER,
        name: "remember",
        syntax: "remember(context, entities[, facts])",
        summary: "Returns a new context object with entities introduced and facts established. Context fields are entities (array of entities accepted by refer), mentioned (array of entity-id strings), and facts (array of fact-id strings), each defaulting to empty. Entities is an array of entity objects. The optional facts argument is an array of nonempty fact-id strings and defaults to []. Existing definitions are retained, identical entities and repeated facts are deduplicated, and conflicting definitions for one id fail. The input context is unchanged. Carry the returned value explicitly across turns. Starting from {} resets the conversation scope. Unknown fields and invalid context references fail during evaluation.",
        examples: &[r#"remember({}, [|settings.guest|], [|"arrival_time"|])"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_UNMENTIONED,
        name: "unmentioned",
        syntax: "unmentioned(details, context)",
        summary: "Returns an array of detail objects whose id is absent from context.facts, preserving source order and text. Each detail is exactly {id: string, text: string}, both nonempty and without surrounding whitespace or control characters. Context has the entities, mentioned, and facts arrays documented by refer, each defaulting to empty. Selection is deterministic and never changes context. Identical repeated details are deduplicated. Conflicting text for one id fails even if that fact was already established. Use remember to mark details established only after they have been communicated.",
        examples: &[r#"unmentioned([|{ id: "arrival_time", text: "Arrival is after midnight." }|], settings.context)"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_FORMAT_NUMBER,
        name: "format_number",
        syntax: "format_number(number_or_decimal_text[, options])",
        summary: "Formats a number or exact decimal string using a packaged English regional profile. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US), grouping: boolean (true), and fraction_digits: integral number from zero through nine, including 1.0 (omitted by default). Explicit precision rounds decimal ties away from zero and pads trailing zeros. Without precision, trailing fractional zeros are trimmed. All profiles use a decimal point and comma grouping. En-IN groups the final three integer digits then pairs, while other profiles use groups of three. Negative zero becomes zero. Decimal strings may contain an exponent and avoid floating-point input ambiguity. Expanded input is bounded to 4096 digits. Invalid values, unknown fields, or unsupported locales fail during evaluation. No OS locale or external data is consulted.",
        examples: &[r#"format_number("1234567.50", { locale: "en-IN" })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_FORMAT_DATE,
        name: "format_date",
        syntax: "format_date(date[, options])",
        summary: "Formats a strict YYYY-MM-DD Gregorian date with year 0001 through 9999. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US) and style: short|long (long). Short output uses MM/DD/YYYY for en-US, YYYY-MM-DD for en-CA, and DD/MM/YYYY otherwise. Long output uses English month names, month-first for en-US and en-CA, and day-first otherwise. Invalid calendar dates, unknown fields, or unsupported locales fail during evaluation. No clock or OS locale is consulted.",
        examples: &[r#"format_date("2026-11-10", { locale: "en-GB", style: "long" })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_FORMAT_TIME,
        name: "format_time",
        syntax: "format_time(time[, options])",
        summary: "Formats a strict HH:MM or HH:MM:SS local clock value. Hours range from 00 through 23 and minutes and seconds from 00 through 59. Options are locale: en-US|en-GB|en-CA|en-AU|en-IN (en-US), hour_cycle: h12|h24 (omitted), and seconds: boolean (false). Omitted hour_cycle selects h24 for en-GB and h12 otherwise. H12 includes AM or PM. Seconds are displayed only when requested, with missing seconds treated as 00. No current time, OS locale, or timezone conversion is implied. Invalid times, unknown fields, or unsupported locales fail during evaluation.",
        examples: &[r#"format_time("17:30", { locale: "en-US" })"#],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_JOIN,
        name: "join",
        syntax: "join(array, separator)",
        summary: "Joins the elements of an array into one string with the separator between them on evaluation; string, number, and boolean elements are stringified.",
        examples: &["join(settings.tags, \", \")", "join([|1, 2, 3|], \"-\")"],
        rules: JOIN_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_CONTAINS,
        name: "contains",
        syntax: "contains(target, needle)",
        summary: "True when a string target contains needle as a substring, or an array target contains needle as an element, on evaluation.",
        examples: &["contains(settings.model, \"mini\")", "contains(settings.tags, \"prod\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_STARTS_WITH,
        name: "starts_with",
        syntax: "starts_with(text, prefix)",
        summary: "True when a string starts with the given prefix, on evaluation.",
        examples: &["starts_with(settings.model, \"gpt\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_ENDS_WITH,
        name: "ends_with",
        syntax: "ends_with(text, suffix)",
        summary: "True when a string ends with the given suffix, on evaluation.",
        examples: &["ends_with(settings.model, \"-mini\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_LEN,
        name: "len",
        syntax: "len(value)",
        summary: "The number of characters in a string or elements in an array, as an integer, on evaluation.",
        examples: &["len(settings.models)", "len(\"hello\")"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_TOKENS,
        name: "tokens",
        syntax: "tokens(value)",
        summary: "The exact number of tokens in a string, or in the JSON serialization of an array or object, as an integer, on evaluation. Counts with the o200k_base encoding used by current OpenAI models.",
        examples: &["tokens(\"What is the weather in Tokyo?\")", "tokens(settings.prompt)"],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_FORMAT,
        name: "format",
        syntax: "format(template, value, ...)",
        summary: "Replaces each `{}` placeholder in a constant string template with the corresponding argument, stringified, in order, on evaluation. Arguments and `${...}` interpolation may contain expressions, including functions.",
        examples: &["format(\"model={} tokens={}\", settings.model, range(80, 400))"],
        rules: FORMAT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_CLAMP,
        name: "clamp",
        syntax: "clamp(value, min, max)",
        summary: "Limits a number to the inclusive range [min, max] on evaluation. Three integers produce an integer; otherwise a float.",
        examples: &[
            "clamp(lognormal(300, 0.7), 20, 30000)",
            "clamp(settings.temperature, 0.0, 1.0)",
        ],
        rules: CLAMP_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_ROUND,
        name: "round",
        syntax: "round(value)",
        summary: "Rounds a number to the nearest integer on evaluation; integers pass through unchanged.",
        examples: &["round(lognormal(120, 0.5))", "round(settings.score * 100)"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_FLOOR,
        name: "floor",
        syntax: "floor(value)",
        summary: "Rounds a number down to the nearest integer on evaluation; integers pass through unchanged.",
        examples: &["floor(exponential(4))"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_CEIL,
        name: "ceil",
        syntax: "ceil(value)",
        summary: "Rounds a number up to the nearest integer on evaluation; integers pass through unchanged.",
        examples: &["ceil(range(0.1, 4.9))"],
        rules: ROUNDING_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_ABS,
        name: "abs",
        syntax: "abs(value)",
        summary: "The absolute value of a number, on evaluation. An integer produces an integer, a float produces a float.",
        examples: &["abs(normal(0, 5))"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_MIN,
        name: "min",
        syntax: "min(value, value, ...)",
        summary: "The smallest of two or more numbers, on evaluation. All integers produce an integer; otherwise a float.",
        examples: &["min(range(1, 10), 5)", "min(settings.limit, poisson(6), 100)"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_MAX,
        name: "max",
        syntax: "max(value, value, ...)",
        summary: "The largest of two or more numbers, on evaluation. All integers produce an integer; otherwise a float.",
        examples: &["max(normal(50, 20), 0)"],
        rules: NUMERIC_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_UUID,
        name: "uuid",
        syntax: "uuid()",
        summary: "A random version-4 UUID string on evaluation, drawn from the seeded generator so runs stay reproducible.",
        examples: &["uuid()"],
        rules: UUID_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_HEX,
        name: "hex",
        syntax: "hex(length)",
        summary: "A random lowercase hexadecimal string of the given constant length, on evaluation.",
        examples: &["hex(16)", "format(\"req_{}\", hex(8))"],
        rules: RANDOM_STRING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_ALPHANUM,
        name: "alphanum",
        syntax: "alphanum(length)",
        summary: "A random alphanumeric string (0-9, A-Z, a-z) of the given constant length, on evaluation.",
        examples: &["alphanum(12)"],
        rules: RANDOM_STRING_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_ATTACHMENT,
        name: "attachment",
        syntax: "attachment(path, content_type)",
        summary: "References a local file as an attachment in a logged value. The path may be absolute or relative to the shape file's directory.",
        examples: &[
            "attachment(\"report.pdf\", \"application/pdf\")",
            "attachment(\"/tmp/report.pdf\", \"application/pdf\")",
        ],
        rules: TEXT_FUNC_RULES,
    },
    FuncDesc {
        tags: &[],
        id: ids::FUNC_NOISE,
        name: "noise",
        syntax: "noise(size)",
        summary: "An opaque random base64-style string of the given size in bytes, for adding payload weight where storage size matters; the size may be dynamic, so payloads can vary per evaluation.",
        examples: &["noise(250000)", "noise(round(lognormal(80000, 0.9)))"],
        rules: NOISE_RULES,
    },
];

const LANGUAGE_RULES: &[RuleDesc] = &[
    RuleDesc {
        tags: &[],
        id: Id::new("kind.declarations"),
        summary: "The + marker introduces a root definition, written as a graph statement. Its head is a node declaration whose name is the defined name and whose kind names the construct: node, edge, fork, junction or trait. Edges from the head relate it to other definitions: extends names its base, has attaches a trait and on lists the constructs a trait applies to. A kind without extends derives from its construct root. Bases resolve forward to declared kinds or built-in roots and primitives. Names must be unquoted identifiers in definitions, relationship targets and instances. Kinds and traits are unique in one namespace separate from instance names, cannot shadow built-ins or reserved names, and cannot form inheritance cycles. A kind extends at most one base, and bases and instances must use the same construct category. A definition-only module is valid. Derived kinds retain their base primitive validation and operation identity.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.schemas"),
        summary: "The braces in a definition head hold property descriptors and its body holds field descriptors, mirroring an instance's properties and content. Only node kinds and traits on node have a body. On an edge kind the header key direction is reserved: it takes directed, undirected or either instead of a descriptor. Omitting direction inherits the base allowance. Extensions may narrow either but cannot widen or contradict an inherited allowance. On a node kind the header key open is reserved: it takes a literal boolean instead of a descriptor. A node kind is closed unless open is true, and a kind whose base is closed cannot set it. Each descriptor accepts type, required, default and role. Types are any, string, number, boolean, array and object. Omitted type is any for a new field and inherited for an override. required is a literal boolean. Defaults are constant expressions without instance references or function evaluation. role does not install an operation. The input role preserves references to the field and its nested values as runtime reads, even when its authored fallback is constant. Consumers may supply a value at that boundary. Constant folding and branch proofs cannot assume the fallback is the supplied value. Output roles preserve field reference identity without installing evaluation behavior. Other roles remain descriptive metadata. Duplicate keys and unknown descriptors are errors. Nested object schemas and collection element schemas are not yet supported.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.inheritance"),
        summary: "Extensions inherit properties, fields, requiredness, defaults, roles and traits. They may add entries, narrow any to a specific type, require optional fields and override defaults. They cannot change a concrete inherited type or make a required field optional. Effective defaults must satisfy the effective type. Declared properties are strict. Content fields are strict for a closed kind and free for an open one. Described content fields receive type and requiredness checks. Bindings retain declared types for subsequent references even when their values depend on runtime context. Consumers must validate runtime values against these types. Each reachable alternative of a conditional expression must satisfy its expected type, while genuinely unknown runtime values defer validation. Missing fields receive defaults when available, otherwise required fields must be explicitly supplied. An authored <- counts as a declared input, not a delivered value.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.relationships"),
        summary: "A relationship is one unnamed directed edge without properties from the definition head to a name: -[:extends]->(base), -[:has]->(trait) or -[:on]->(construct). Targets are references resolved in the kind namespace, never declarations. One relationship may follow the head on its line. Two or more are aligned continuation lines, so each starts at the head. A relationship that continues from another relationship's target, an edge list, a fork, a loop-back or an undirected edge is an error. Kinds accept extends and has. Traits accept extends and on.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.traits"),
        summary: "A trait is a named schema of property and field descriptors that kinds attach with has. It carries no primitive behavior and cannot be instantiated. A trait lists the constructs it applies to with on, and a kind may only have traits that are on its construct. Only nodes have content fields, so a trait with fields is on node alone. A trait may extend one trait: it inherits that trait's entries under the kind inheritance rules and its on list, and may list on only to narrow it. A trait cannot use has. A kind's traits are those of its base, those it attaches and every trait they extend, and attaching one it already has changes nothing. Trait entries are merged after the base and before the kind's own descriptors, which may restrict them like inherited entries. An entry declared by two unrelated traits, or by a trait and the kind's base, is an error at the has relationship. Models retain each kind's traits.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.inputs"),
        summary: "A direct node content field with value <- requires upstream data for each occurrence. Lowering preserves an Input with binding identity and the schema type, or any for an undescribed field. Authored expressions may read that input through ordinary references. Runtime data availability is not proven by compilation. Declared inputs suppress schema defaults for that field. Defaults are fallback values for omitted fields, not for undelivered required inputs.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.destinations"),
        summary: "For feeds and its specializations, into resolves a field identity without reading its value. It must select a direct content field on the edge target explicitly marked <-. Properties, other nodes, missing fields and authored value fields are invalid destinations. The model retains FieldRef separately from Input and BindingRef, so a destination does not add a read dependency. Known payload types are checked against the input type. Multiple suppliers may name the same destination, without defining arrival ordering, merging or runtime delivery.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.direction"),
        summary: "Ordinary connectors are -[headers]-> for directed edges and -[headers]- for undirected edges. Each declaration creates a distinct identity, including parallel edges and self-connections. The root edge kind and link permit either direction. next, requires, contains and feeds require directed edges. incidence requires undirected edges. Direction is set in an edge kind's definition header, not an instance property. Directed and undirected connections can coexist between the same nodes.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.network"),
        summary: "A network is an ordinary node kind with required nodes and edges arrays of statically resolved references. Membership is explicit and unique. Every selected edge must have both endpoints in the node selection. Isolated nodes are allowed. A network creates no containment, copies, activation or implicit membership for itself. Overlapping selections are allowed. Constraints apply only to selected relationships, so a containment DAG and cyclic behavior may coexist.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.network-policy"),
        summary: "Network properties are nodes, edges, validate, acyclic, connected, self_loops, max_parallel, incoming, outgoing and degree. validate is definition by default, materialized or both. Compilation always checks configuration and membership. Definition and both enforce structural constraints during compilation. Materialized and both require enforcement within each materialization group by a consumer, which must reject unsupported enforcement rather than silently skip it. Materialized membership expands selected definitions to their occurrences and connections within the same group.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.network-structure"),
        summary: "acyclic is none by default, directed or underlying. directed requires all selected edges to be directed and prohibits directed cycles. underlying ignores direction but preserves distinct edge identities, so parallel edges and self-connections form cycles. connected is none by default, weak or strong. Weak ignores direction. Strong respects directed edges and traverses undirected edges either way. A singleton satisfies either connectivity requirement. An empty selection fails weak and strong connectivity.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.network-counts"),
        summary: "self_loops defaults to true. False rejects selected connections from a node to itself, including loop-back edges. max_parallel is an optional nonnegative integer bound. Directed connections count by ordered endpoint pair and undirected connections by unordered pair, in separate categories. Kind and loop marker do not divide these counts. incoming and outgoing contain optional nonnegative integer min and max bounds for every selected node, counting directed edges only. degree has the same shape and counts undirected edges only. Parallel edges count separately. Directed self-connections contribute one to incoming and outgoing. Undirected self-connections contribute two to degree. Bounds are inclusive and min cannot exceed max. Omitted bounds are unrestricted.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.relations"),
        summary: "relation nodes and incidence edges encode multi-party relationships without a new primitive. Each undirected incidence connects exactly one relation or relation-derived node to one participant. The role property is member by default, input or output. Member-only relationships are undirected. Directed relationships require at least one input and output and cannot mix member roles. Roles express relationship direction without initiating execution. Relation participants, inputs and outputs accept inclusive min and max incidence-count bounds. unique defaults to true and prohibits repeated participant identity within the same role, while allowing one participant to be both input and output. Separate relation nodes preserve distinct identities even with identical participants.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.weighted"),
        summary: "weighted forks select exactly one eligible branch using relative weights and seeded randomness. Each edge in a branch requires an explicit finite nonnegative weight and bundled weights must agree. Evaluate all branch guards before evaluating its weights. Zero excludes a branch. If no positive-weight branch qualifies, no branch is selected and that path stops. Each activation samples again. count is not supported. Known invalid weights fail compilation and dynamic weights are checked by the consumer.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.each"),
        summary: "each forks require an items array expression. Known non-array values are rejected and runtime values must be checked by consumers. Every array item creates a distinct occurrence of every branch destination, retaining definition identity and ancestry. Destination context.item and context.index identify its item and zero-based index. Every listed branch participates, so branch guards are prohibited. An optional concurrency property is a positive integer, with runtime-dependent values checked by the consumer. Omission leaves scheduling unconstrained. Empty arrays create no branch occurrences. Compilation preserves the array expression and graph definitions rather than instantiating occurrences.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.observation"),
        summary: "Built-in junctions accept observe: arrival by default or result. Arrival observes data delivered along a listed edge. Result observes the emitted payload of the destination occurrence reached through that edge, not every occurrence of its node definition. Consumers define payload production through authored values or registered operations. An undirected arrival records which endpoint it reached. Fork context identifies the branch destination. A standalone undirected arrival must carry its destination explicitly.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.groups"),
        summary: "An execution has a default occurrence group. each and repeat create nested iteration groups and retain a shared batch identity for history. Attached junctions observe the fork batch in its enclosing group, so repeated branches can contribute multiple arrivals. Junctions inside an iteration use that iteration group. Cycle visits preserve their enclosing group. A group closes explicitly after all contributing occurrences settle, including failures and cancellations. Outside a fork a consumer must supply group identity and a closing event. Silence and elapsed time do not close groups implicitly. Independent groups are never combined implicitly. Definition identity, occurrence identity, ancestry, correlation and observation identity are distinct runtime responsibilities. An accumulate junction repeatedly consumes one observation per listed input within its group in FIFO order. Unmatched arrivals or a missing listed input fail at closure. reduce consumes repeated observations, including multiple occurrences of the same listed edge.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.reduce"),
        summary: "reduce requires initial and combine. Each observation updates context.accumulator using combine, with the observed payload in context.arrival.value. Emit one result on explicit group closure. An empty group emits initial. order is source by default or arrival. Source order follows the stable logical occurrence order assigned by the producer, including each item index and branch position, independent of scheduling. Arrival order uses actual observation order and may vary with scheduling. Consumers must provide that ordering or reject the configuration. Compiler validation does not execute combination or group scheduling.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.failures"),
        summary: "Built-in junction policies on_failure and on_cancel accept fail by default or skip. A failed required contributor prevents a successful group result under fail. Skip excludes the failed or cancelled contributor. Inactive unselected branches are not failures or cancellations. Skipped contributors cannot satisfy a quorum count, and an impossible quorum must fail when the group closes. A reduce with no surviving contributions emits initial. first and quorum may emit once their criteria are met and do not retroactively retract an emitted result due to unrelated later failures. Waiting junctions must account for inactive inputs explicitly rather than wait for observations that cannot occur.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.sources"),
        // provider schemas are opt-in data contracts, not installed runtime operations
        summary: "Callers may supply named definition sources before compilation through semantics::compile_with_definitions or compile_module_with_definitions. Definition sources contain only definitions, for kinds and traits, and cannot declare graph instances or paths. Supplied and user definitions share one namespace and one inheritance and validation system. Forward references across sources are allowed. Duplicate names, reserved names, unknown bases or traits, inheritance cycles and cross-construct extension remain errors. Supplied definitions are registered in caller order followed by user definitions, without override precedence. Unknown instance kinds are errors. Source ranges retain local byte offsets and a source index, with user source at zero and definitions in caller order starting at one. Models retain source names and resolved kind identities. Empty user source follows the explicit allow_empty policy independently of supplied definitions, and supplied kinds are still validated when empty user source is allowed. No package loading, provider execution or external I/O occurs.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("kind.contracts"),
        summary: "Kind schemas describe configuration and data. Primitive validators enforce structural and value rules. Runtime operations are optional and are supplied by consumers. Derived kinds retain inherited validation and operation identity, and may add restrictions. Traits add schema entries without operations. A schema alone cannot give arbitrary properties executable meaning. Caller-supplied definition sources provide schema integration. Provider registry loading, package identity and executable operation registration remain separate integration work. Existing compilation retains the contracts needed by that work without executing consumer operations.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.lowering"),
        summary: "Compilation registers declarations before resolving endpoints and field dependencies. Each node, edge, fork and junction has a model-local identity. Names and source paths do not create implicit containment edges. Root and attached paths preserve authoring structure alongside the flattened graph tables.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.fields"),
        summary: "Every instance kind is a built-in or a declared kind. An unknown kind is an error at the kind name. Properties are strict for every kind except the root node kind. A built-in accepts its fixed configuration properties, edge kinds also accept when, weight and max_traversals, and a declared kind accepts those of its base plus its own. Content fields are strict for a closed node kind and free for an open one. The built-in node kinds are open. A declared node kind is closed unless it sets open. A closed kind still holds free-form content under a declared field of type any or object. Duplicate properties or body keys are errors. The body key props is reserved. A missing authored field is an error, while context paths explicitly describe externally supplied values. An unknown name reports the nearest declared name when one is close.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.accessors"),
        summary: "Node fields use node.field or (node).field, and self refers to the owning node. Every configurable element exposes .props. Directed edges expose .source and .target. Undirected edges expose .left and .right in authored order, independent of traversal or pattern orientation. Both expose residual .scope. Forks expose .source and .paths, with no singular target. Within directed edge properties, source and target resolve their endpoints. Within undirected edge properties, left and right resolve their endpoints. Using the wrong orientation accessors is an error. Fork properties expose source.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.paths"),
        summary: "A fork's .paths is an ordered array of branch arrays containing exact edge handles. Each branch has one connection step with one or more parallel edge declarations. [fork].[edge] requires direct membership in that fork. Selector membership is checked against branch target identities, including statically resolved aliases.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.junctions"),
        summary: "A junction requires a kind and preserves its explicitly listed input edges and destination. Optional properties follow the kind inside the junction brackets, before its input references. Properties belong to the junction, not its destination, and use the ordinary expression rules. Context reads in those properties belong to the junction. Inputs of an attached junction belong to the attached fork. Root junctions can join separately declared edges. Overlapping input sets on sibling junctions are allowed and do not create implicit edges between their destinations. Built-in junction input lists contain distinct edge identities. Grouped junctions use correlate as an explicit group key when supplied, otherwise a runtime consumer must supply the activation group. Independent groups must not be combined implicitly. Each input has an ordered queue within its execution group and correlation partition. Group closure occurs when no runnable or in-flight work can produce further arrivals, not through silence or elapsed time. A waiting junction alone does not keep the group open. An agent execution owns the default group, each iteration owns a nested group, and cycle visits preserve their group. Correlation partitions a group without extending its lifetime. The kind determines the combination behavior, properties configure it.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.junction-scope"),
        summary: "Junction targets explicitly read context.arrivals records with edge and value keys. Junctions do not merge arrival values into target fields. merge and first inherit the forwarded arrival scope. accumulate, quorum and reduce combine contributing scopes. References shared by those scopes resolve only when they identify the same occurrence. Different occurrences of one definition are ambiguous, rather than selecting whichever completed last. Targets disambiguate through explicitly carried arrival values. Mocking processes arrivals deterministically in scheduled order.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.bindings"),
        summary: "Constant values fold during compilation. Dynamic fields retain binding identities through aliases, collection access and interpolation. Explicit nested object keys have independent bindings, permitting sibling dependencies without duplicate sampling. The model prescribes no activation lifetime, evaluation scheduler or automatic scope merge.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.cycles"),
        summary: "Cycle punctuation explicitly marks directed loop-back behavior, independently of mathematical cycles. It always requires ~[headers]~> with an arrowhead. There is no undirected loop-back connector. Ordinary self-connections are allowed in either direction category. Compilation does not require a marked edge to close a topological cycle or require unmarked edges to form a DAG. Network acyclicity checks inspect all selected connections regardless of loop markers. Graph cycles do not permit circular field dependencies. Each traversed loop-back creates a fresh target occurrence and evaluates its outgoing paths again. source denotes the current occurrence and target the new occurrence. Explicit feeds supplies values before target evaluation, with no implicit copying of prior fields. max_traversals is an optional nonnegative integer bound on traversals of that edge within its execution group. A qualifying traversal beyond the limit fails rather than silently terminating. Mocking also enforces a 10000 occurrence execution budget per agent invocation. Cycle edges can be fork branches and do not implicitly suppress other outgoing edges. Exclusive forks express mutually exclusive retry and exit paths. context.history exposes completed earlier occurrences in the current execution or repetition batch and excludes in-flight occurrences. Its view is fixed when the reading occurrence starts, even when its fields are evaluated later.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.guards"),
        summary: "The when edge property requires a boolean value, with context-dependent checks deferred to evaluation. Conditional forks allow overlapping guards. Parallel forks prohibit guards. Exclusive forks require pairwise disjoint branch predicates. The current proof handles constants, aliases, negation, conjunction and disjunction over up to twelve distinct atoms. Equality to distinct scalar literals on the same subject is mutually exclusive. Equality and inequality, and complementary numeric comparisons at the same bound, share normalized predicates. Shared bindings retain draw identity and independent function calls remain distinct. General arithmetic interval reasoning is not performed. Unproved disjointness is a validation error. Exhaustive coverage and defaults are not required.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.pattern_constraints"),
        summary: "A pattern contains exactly one @ selection marker on a typed node or edge constraint. Kinds and constant properties filter existing definitions. Runtime-dependent matching is rejected. Each edge constraint in a step matches a distinct edge with the same endpoints, respecting its direction and ordinary or loop-back connector. An undirected pattern matches either endpoint orientation while retaining the selected edge identity and authored left and right endpoints. Patterns do not declare graph elements.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("expression.validation"),
        summary: "All operands and conditional arms receive static validation even when runtime evaluation would skip them. Residual logical operators and conditional expressions preserve runtime short-circuit behavior. Function calls remain expressions for an evaluator, including deterministic calls. This crate does not execute functions, load attachments or provide domain validation.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.names"),
        summary: "Named nodes, edges and forks share one document namespace. Forward references are resolved after declaration registration. The names self, source, target, left, right, context, true, false and null are reserved for contextual references and literals, including when quoted. They cannot name graph declarations or comprehension bindings. Property and content keys may use these names.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.references"),
        summary: "References preserve graph identity through aliases, statically selected collection elements and constant conditional expressions. Node endpoints must resolve statically to nodes, junction inputs to exact edges. Both node and edge boundaries accept reference expressions, including fields and array indexes. A standalone quoted name inside a boundary looks up that graph name. Dynamic endpoint or input selection is rejected. Referencing an existing edge does not redeclare it or change its endpoints. Properties can read configuration from another edge through its reference.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.placement"),
        summary: "Instance bodies and properties contain fields, never declarations. Kind declarations are separate root statements. Properties configure elements, bodies supply contents.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.attachment"),
        summary: "Indentation attaches continuation lines. Aligned edge, cycle and fork lines share an origin, the last node of the nearest shallower line. A line indented deeper continues from the last node of the line it sits under. Indentation also attaches selectors and junctions to forks. Selectors must select the opposite endpoints of branches in their attached fork. Fork origin determines the branch destination even for undirected edges.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.dependencies"),
        summary: "Circular field dependencies are errors, independently of explicitly marked graph cycles.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.patterns"),
        summary: "Path-pattern references select one distinct definition. Multiple matching paths to the same definition are allowed.",
    },
    RuleDesc {
        tags: &[],
        id: Id::new("graph.context"),
        summary: "Context reads remain explicit residual values owned by their graph element. A consumer supplies their meaning for the selected behavior, so the same node definition can be interpreted by different consumers without introducing a separate kind. Lowering does not prescribe scope propagation or scheduling.",
    },
    RuleDesc {
        tags: &[],
        id: ids::FINITE_NUMBERS,
        summary: "Every number must be representable as a finite integer or floating-point value.",
    },
    RuleDesc {
        tags: &[],
        id: ids::UNIQUE_OBJECT_KEYS,
        summary: "An object key may appear at most once in an object.",
    },
    RuleDesc {
        tags: &[],
        id: ids::STATIC_STRUCTURE,
        summary: "Collection structure must be statically inspectable where a comprehension or spread requires it. Graph handles are identities rather than collection values.",
    },
    RuleDesc {
        tags: &[],
        id: ids::KNOWN_FUNCTIONS,
        summary: "A function call may only use the documented functions listed in this spec.",
    },
    RuleDesc {
        tags: &[],
        id: ids::OPERAND_TYPES,
        summary: "Operator operands must have a known type: `+ - * / %` and `< <= > >=` take numbers, `== !=` compare two strings, two numbers, or two booleans, and `&& || !` take booleans.",
    },
    RuleDesc {
        tags: &[],
        id: ids::NONZERO_DIVISORS,
        summary: "Division and remainder require a nonzero divisor; a constant zero divisor is rejected during validation, and a divisor that evaluates to zero fails evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::BOOLEAN_CONDITIONS,
        summary: "The condition of `?:` must be a boolean.",
    },
    RuleDesc {
        tags: &[],
        id: ids::CHOICE_ALTERNATIVES,
        summary: "`choice` takes at least one alternative.",
    },
    RuleDesc {
        tags: &[],
        id: ids::RANGE_BOUNDS,
        summary: "`range` takes exactly two finite numbers with min <= max.",
    },
    RuleDesc {
        tags: &[],
        id: ids::FUNC_ARITY,
        summary: "A function takes the arguments its syntax documents; `choice`, `weighted`, `min`, `max`, and `format` accept a variable number. `compose` accepts an optional separator.",
    },
    RuleDesc {
        tags: &[],
        id: ids::FUNC_ARG_TYPES,
        summary: "A function argument must have the documented type, known before evaluation; an argument whose type is only known during evaluation (e.g. a `choice` with mixed alternatives) is rejected.",
    },
    RuleDesc {
        tags: &[],
        id: ids::DIST_PARAMS,
        summary: "Distribution parameters are constant finite numbers: `normal` takes stddev >= 0, `lognormal` takes median > 0 and sigma >= 0, `exponential` and `poisson` take mean > 0, `pareto` takes min > 0 and shape > 0, `beta` takes alpha > 0 and beta > 0, and `chance` takes a probability between 0 and 1.",
    },
    RuleDesc {
        tags: &[],
        id: ids::WEIGHTED_OPTIONS,
        summary: "`weighted` takes at least one `[value, weight]` pair; weights are constant non-negative finite numbers, at least one of them positive.",
    },
    RuleDesc {
        tags: &[],
        id: ids::SPLIT_SEPARATOR,
        summary: "`split` requires a non-empty separator; a constant empty separator is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::FORMAT_TEMPLATE,
        summary: "`format` takes a constant string template with exactly one `{}` placeholder per remaining argument, replaced in order.",
    },
    RuleDesc {
        tags: &[],
        id: ids::CLAMP_BOUNDS,
        summary: "`clamp` bounds must satisfy min <= max; a constant violation is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::RANDOM_LENGTH,
        summary: "`hex` and `alphanum` take a constant non-negative integer length.",
    },
    RuleDesc {
        tags: &[],
        id: ids::NOISE_SIZE,
        summary: "`noise` takes an integer size between 0 and 8388608 (8 MiB); a constant violation is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::INDEXABLE_TARGETS,
        summary: "Indexing requires an array or object target: arrays take an integer index, objects take a string key.",
    },
    RuleDesc {
        tags: &[],
        id: ids::INDEX_BOUNDS,
        summary: "An array index must be within bounds and an object key must be present; constant violations are rejected during validation, dynamic ones fail the run during evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::SLICEABLE_TARGETS,
        summary: "Slicing requires an array target; slice bounds are numbers.",
    },
    RuleDesc {
        tags: &[],
        id: ids::SLICE_BOUNDS,
        summary: "Slice bounds are non-negative integers, clamped to the array length; a start at or past the end produces an empty array. Constant violations are rejected during validation, dynamic ones fail the run during evaluation.",
    },
    RuleDesc {
        tags: &[],
        id: ids::SPREAD_OPERANDS,
        summary: "A spread operand must be an array (inside arrays) or an object (inside objects) whose shape is known before evaluation. Later object entries override keys introduced by a spread, but two explicit keys may still not collide.",
    },
    RuleDesc {
        tags: &[],
        id: ids::FOR_COLLECTIONS,
        summary: "A for expression iterates an array or object whose shape is known before evaluation; element values may still be dynamic.",
    },
    RuleDesc {
        tags: &[],
        id: ids::STATIC_FOR,
        summary: "For expressions unroll during validation: a filter condition must resolve to a constant boolean and an object key to a constant string.",
    },
    RuleDesc {
        tags: &[],
        id: ids::SCALAR_INTERPOLATION,
        summary: "An interpolated variable must be a string, number, or boolean; a dynamic variable must have one of those types known before evaluation.",
    },
];
const PRIMITIVES: &[PrimitiveDesc] = &[
    PrimitiveDesc {
        name: "network",
        construct: "node",
        summary: "An explicit selection of nodes and edges with structural constraints. Definition checks run during compilation. Materialized checks are mandatory consumer obligations within each materialization group.",
    },
    PrimitiveDesc {
        name: "relation",
        construct: "node",
        summary: "One multi-party relationship represented by incidence edges. Member incidences form an undirected relationship. Input and output incidences form a directed relationship with at least one of each. Membership does not imply execution.",
    },
    PrimitiveDesc {
        name: "incidence",
        construct: "edge",
        summary: "Undirected membership between one relation node and one participant. The role property is member, input or output. Input and output roles describe the direction of the relationship, not execution along the incidence edge.",
    },
    PrimitiveDesc {
        name: "weighted",
        construct: "fork",
        summary: "Select one eligible branch using explicit nonnegative relative weights and seeded randomness. All edges in a branch must agree on weight and all their guards must pass. No positive eligible weight means no selection. Each activation samples again.",
    },
    PrimitiveDesc {
        name: "repeat",
        construct: "fork",
        summary: "Instantiate every listed branch count times with distinct destination occurrences. Count is a nonnegative integer evaluated once when the fork activates. Zero creates no occurrences. Each occurrence exposes context.index and context.count, with nested forks supplying their own innermost context. context.history.<node_name> exposes payload records from completed earlier occurrences in the same repetition group. Consumers supply the payload fields and preserve group identity when evaluating history. The group closes after every contributing occurrence settles.",
    },
    PrimitiveDesc {
        name: "each",
        construct: "fork",
        summary: "For each array item, instantiate every listed branch with a distinct destination occurrence. Item and index are available as context.item and context.index. Preserve definition identity separately from occurrence identity and group ancestry. The group closes after every contributing occurrence settles.",
    },
    PrimitiveDesc {
        name: "reduce",
        construct: "junction",
        summary: "Fold repeated observations within one group using initial and combine. The combine expression reads context.accumulator and context.arrival.value. Emit once at group closure, or initial for an empty group. The target reads the folded value through context.accumulator and all consumed arrivals through context.arrivals in processing order. Observation mode selects edge arrivals or destination occurrence results.",
    },
    PrimitiveDesc {
        name: "accumulate",
        construct: "junction",
        summary: "Queue arrivals separately for each listed input within a correlation group. Repeatedly consume one arrival from each queue and create a fresh target occurrence. context.arrivals contains edge and value records in declared input order. Missing inputs or unmatched leftovers fail at group closure. Arrivals are consumed once per junction, independently of overlapping junctions.",
    },
    PrimitiveDesc {
        name: "merge",
        construct: "junction",
        summary: "Forward each incoming arrival independently to a fresh target occurrence with its input identity and data preserved. context.arrivals contains that single arrival. Do not wait for other inputs or combine their scopes.",
    },
    PrimitiveDesc {
        name: "first",
        construct: "junction",
        summary: "Emit the earliest arrival within a correlation group to the target, preserving its input identity and data. Emit at most once per group and ignore later arrivals. No arrivals means no emission. context.arrivals contains the winning arrival. This does not implicitly cancel upstream work.",
    },
    PrimitiveDesc {
        name: "quorum",
        construct: "junction",
        summary: "Emit one grouped arrival after count distinct listed inputs have arrived within a correlation group. Preserve the contributing input identities and data. count is a required constant integer from one through the number of listed inputs. Repeated arrivals from one input count once. Select once per group and ignore later arrivals. An unmet quorum fails at closure. context.arrivals lists contributing inputs in declared input order.",
    },
    PrimitiveDesc {
        name: "link",
        construct: "edge",
        summary: "Directed or undirected adjacency between nodes. Establishes a relationship without prescribing activation or data transfer.",
    },
    PrimitiveDesc {
        name: "next",
        construct: "edge",
        summary: "Directed progression from source to target. An eligible traversal advances behavior to the target. It does not imply copying source fields into the target.",
    },
    PrimitiveDesc {
        name: "requires",
        construct: "edge",
        summary: "The source depends on the target as a prerequisite. This direction expresses dependency rather than progression. The consumer defines how a prerequisite is satisfied.",
    },
    PrimitiveDesc {
        name: "contains",
        construct: "edge",
        summary: "The source structurally contains the target. Membership alone does not activate the target, merge scopes, or impose single-parent tree constraints.",
    },
    PrimitiveDesc {
        name: "feeds",
        construct: "edge",
        summary: "Directed data transfer from source to target. The value property specifies the payload when supplied. With into, the destination is a field reference to a direct content field on this edge's target that declares <-. A destination requires value, whose known type must match the destination schema. Data transfer does not itself imply progression or automatic copying of all source fields.",
    },
    PrimitiveDesc {
        name: "parallel",
        construct: "fork",
        summary: "All branches participate. Edge guards are prohibited.",
    },
    PrimitiveDesc {
        name: "conditional",
        construct: "fork",
        summary: "Matching branches participate. Conditions are boolean, overlap is permitted, and no default is required.",
    },
    PrimitiveDesc {
        name: "exclusive",
        construct: "fork",
        summary: "At most one branch participates. Branch predicates must be provably disjoint, without requiring exhaustive coverage.",
    },
    PrimitiveDesc {
        name: "vars",
        construct: "node",
        summary: "A node of named expression bindings. The evaluate property accepts lazy or eager. Evaluation lifetime is a consumer concern.",
    },
];
const EXPRESSIONS: &[ExprDesc] = &[
    ExprDesc {
        tags: &[],
        id: Id::new("expression.values"),
        syntax: "null | true | false | number | string | [|values|] | {key: value}",
        summary: "Scalars and comma-separated containers, with optional trailing commas.",
        examples: &["[||]", "{ready: true}"],
        rules: &[],
    },
    ExprDesc {
        tags: &[],
        id: Id::new("expression.references"),
        syntax: "(node) | [edge_or_fork] | [fork].[edge] | ?<path>",
        summary: "Static graph references, with one @ marker in a path pattern.",
        examples: &["(entry)", "[checks].[lookup]", "?<(entry)-[:next]->(@:task)>"],
        rules: &[],
    },
    ExprDesc {
        tags: &[],
        id: Id::new("expression.operations"),
        syntax: "${expression} | !value | -value | a + b | a && b | condition ? yes : no",
        summary: "Existing operator precedence, checked numeric arithmetic and short-circuit conditions.",
        examples: &["${1 + 2} * 3", "true ? 1 : 2"],
        rules: &[],
    },
    ExprDesc {
        tags: &[],
        id: Id::new("expression.collections"),
        syntax: "value[index] | value[start:end] | ...value | [|for x in values: expression|]",
        summary: "Indexing, slicing, spreads and comprehensions retain their type and bounds checks.",
        examples: &["[|for x in [|1, 2|]: x + 1|]"],
        rules: &[],
    },
];
const EXAMPLES: &[Example] = &[
    Example {
        tags: &[],
        id: Id::new("example.kind-input"),
        summary: "A typed upstream input and a field destination",
        note: "Compilation retains the input and destination identities without delivering data.",
        source: r#"+(receiver:node) {id: {type: "string", required: true}}; (a:node) {id: "p-1"} -[:feeds {value: source.id, into: target.id}]->(b:receiver) {id: <-};"#,
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.kind-property"),
        summary: "Declared properties are strict",
        note: "Additional content fields are allowed, additional properties are rejected.",
        source: r#"+(record:node {name: {type: "string"}}); (a:record {typo: true});"#,
        valid: false,
    },
    Example {
        tags: &[],
        id: Id::new("example.kind-junction"),
        summary: "A junction kind retains its base operation",
        note: "The head names the junction construct, and extends names the base whose operation the kind keeps.",
        source: "+(joined:junction)-[:extends]->(accumulate); (:node)-[e:link]->(:node); >[:joined [e]]>>(:node);",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.trait"),
        summary: "A kind has a trait's properties and fields",
        note: "The trait applies to node kinds. Its property goes in the instance header and its field in the body.",
        source: r#"+(metered:trait {unit: {type: "string", default: "ms"}}) {metrics: {type: "object", required: true}}-[:on]->(node); +(lookup:node)-[:has]->(metered); (a:lookup {unit: "s"}) {metrics: {elapsed: 2}};"#,
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.trait-collision"),
        summary: "Two traits cannot declare the same field",
        note: "The error is reported at the second has relationship.",
        source: r#"+(metered:trait) {metrics: {type: "object"}}-[:on]->(node); +(counted:trait) {metrics: {type: "object"}}-[:on]->(node); +(lookup:node)
    -[:has]->(metered)
    -[:has]->(counted);"#,
        valid: false,
    },
    Example {
        tags: &[],
        id: Id::new("example.closed-kind"),
        summary: "A declared kind takes only its declared fields",
        note: "The misspelled field is rejected at its key, with the nearest declared name.",
        source: r#"+(record:node) {value: {type: "number"}}; (a:record) {valeu: 1};"#,
        valid: false,
    },
    Example {
        tags: &[],
        id: Id::new("example.open-kind"),
        summary: "An open kind takes any field",
        note: "The open header key keeps the free content of the root node kind.",
        source: "+(notes:node {open: true}); (a:notes) {anything: 1};",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.unknown-kind"),
        summary: "An unknown kind is an error",
        note: "Every kind is a built-in or a definition, for nodes, edges, forks and junctions alike.",
        source: "(a:node)-[:nxet]->(b:node);",
        valid: false,
    },
    Example {
        tags: &[],
        id: Id::new("example.graph"),
        summary: "A graph with a forward reference",
        note: "The root node kind takes any property and field.",
        source: "(a:node)-[:next]->(b); (b:node);",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.fan-out"),
        summary: "Aligned continuation lines share an origin",
        note: "All three edges leave a. Writing them as separate statements means the same.",
        source: "(a:node)\n    -[:next]->(b:node)\n    -[:next]->(c:node)\n    -[:next]->(d:node);",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.chain"),
        summary: "Deeper continuation lines chain",
        note: "Each line continues from the last node of the line it sits under, so the edges run a to b, b to c and c to d.",
        source: "(a:node)\n    -[:next]->(b:node)\n        -[:next]->(c:node)\n            -[:next]->(d:node);",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.expressions"),
        summary: "Generic field expressions",
        note: "Constant arithmetic folds during compilation.",
        source: "(values:vars) {n: ${1 + 2} * 3, items: [|for x in [|1, 2|]: x * 2|]};",
        valid: true,
    },
    Example {
        tags: &[],
        id: Id::new("example.duplicate"),
        summary: "Names are globally unique",
        note: "Kinds do not create namespaces.",
        source: "(a:node); (a:vars);",
        valid: false,
    },
    Example {
        tags: &[],
        id: Id::new("example.missing"),
        summary: "References must resolve",
        note: "No implicit declarations.",
        source: "(missing);",
        valid: false,
    },
];
/// The supported graph language and default expression contracts.
pub static SPEC: Spec = Spec {
    schema_version: 2,
    language_version: 2,
    name: "bts",
    summary: "A language for defining graphs, references and expressions without domain-specific node schemas.",
    surface: SurfaceDesc {
        notation: "Typed graph elements with comma-separated properties and content fields.",
        grammar: r#"document = { (root_path | definition), ";" }
definition = "+(", identifier, ":", construct, [fields], ")", [fields], {relationship}
construct = "node" | "edge" | "fork" | "junction" | "trait"
relationship = "-[:", ("extends" | "has" | "on"), "]->(", identifier, ")"
root_path = (node | junction), steps
attached_path = (selector | junction), steps
steps = {edge | cycle}, [fork | arms]
arms = arm, {arm}
arm = (edge | cycle), steps | fork
node = "(", header, ")", [fields] | "(", reference_expression, ")"
header = [name], ":", kind, [fields]
headers = header, {",", header}, [","]
edge = "-[", headers, ("]->" | "]-"), node
cycle = "~[", headers, "]~>", node
fork = "=[", header, "]=>[", (edge | cycle), {",", (edge | cycle)}, [","], "]", {attached_path}
selector = "->(", reference_expression, ")"
junction = ">[:", kind, [fields], junction_input, {",", junction_input}, [","], "]>>", node
junction_input = edge_reference | pattern
fields = "{", [field, {",", field}, [","]], "}"
field = identifier, ":", (expression | "<-")
name = identifier | plain_string
kind = identifier
edge_reference = "[", reference_expression, "]", {".[", name, "]"}
pattern = "?<", pattern_node, {pattern_step}, ">"
pattern_node = "(", reference_expression, ")" | "(", ["@"], ":", kind, [fields], ")"
pattern_step = ("-[", pattern_headers, ("]->" | "]-") | "~[", pattern_headers, "]~>"), pattern_node
pattern_headers = pattern_header, {",", pattern_header}, [","]
pattern_header = ["@"], ":", kind, [fields]
array = "[|", [expression, {",", expression}, [","]], "|]"
object = "{", [field, {",", field}, [","]], "}"
group = "${", expression, "}"
array_for = "[|for", bindings, "in", expression, ":", expression, ["if", expression], "|]"
object_for = "{for", bindings, "in", expression, ":", expression, "=>", expression, ["if", expression], "}"
bindings = identifier, [",", identifier]
expression = conditional
conditional = logical_or, ["?", expression, ":", conditional]
logical_or = logical_and, {"||", logical_and}
logical_and = equality, {"&&", equality}
equality = comparison, {("==" | "!="), comparison}
comparison = additive, {("<" | "<=" | ">" | ">="), additive}
additive = multiplicative, {("+" | "-"), multiplicative}
multiplicative = unary, {("*" | "/" | "%"), unary}
unary = ("!" | "-"), unary | postfix
postfix = primary, {"[", expression, "]" | "[", [expression], ":", [expression], "]" | ".", identifier | ".[", name, "]"}
primary = scalar | template | array | object | array_for | object_for | group | call | reference
call = identifier, "(", [expression, {",", expression}, [","]], ")"
reference = identifier | "(", reference_expression, ")" | edge_reference | pattern
reference_expression = expression
"#,
        notes: &[
            "This grammar describes token forms. The layout rules below constrain arm and attached_path repetition and legal positions. Scalar and template tokenization includes quoted, multiline and heredoc strings, interpolation, finite numbers, booleans and null.",
            "Spaces establish continuation levels, with no fixed width. A step that begins a line is a continuation line, and it is indented deeper than the line it attaches to. Aligned continuation lines are siblings. A dedent returns to an established level. Tabs are not indentation. Delimited values and strings do not create graph scope.",
            "Each arm begins a continuation line. Its origin is the last node of the nearest shallower line, so aligned arms share one origin and a deeper line continues the chain. Steps on one line chain, including a step that follows a node body on its closing line. A fork may be an arm, and arms aligned with it after its attached paths share its origin.",
            "A definition's relationships follow the arm rules: one may sit on the head's line, and two or more each begin an aligned continuation line.",
            "One selector or junction may continue a fork on the same line. Another sibling requires a new aligned line. Its attachment is to the enclosing fork, never the preceding sibling's final node. Edge and cycle lines do not align with a fork's selectors and junctions.",
            "The <- marker is legal only as the entire value of a direct instance node content field. It is not an expression and is forbidden in properties, nested objects, arrays and schema descriptors.",
            "Node bodies contain only fields. Node declarations are legal at path origins and connection targets. No declarations occur in expression values or properties. Empty node parentheses are invalid.",
            "Fork entries contain exactly one ordinary directed or undirected edge step. Edge lists share their endpoints but retain separate names, properties and identities. Fork lists and junction lists are nonempty.",
            "Root paths require semicolons. Internal entries require commas between entries on one line or multiple lines. A trailing comma is optional, including for a single entry. Array and object spreads use ...expression in an entry position.",
            "References use parentheses and brackets, arrays use [| |], expression grouping uses ${ }.",
            "Every kind is a built-in or a declared name. Built-in kinds describe distinct structural or computational contracts, not interchangeable labels. This crate preserves those kinds and checks static rules, while runtime consumers implement their behavior. Definitions provide local kind schemas, same-category extension and traits. Package loading and runtime operation registration remain future work. Schema declarations do not install executable behavior. Consumers must honor inherited primitive contracts or explicitly reject unsupported capabilities.",
        ],
    },
    expressions: EXPRESSIONS,
    functions: FUNCS,
    primitives: PRIMITIVES,
    rules: LANGUAGE_RULES,
    examples: EXAMPLES,
};
