use super::contracts::AttachmentFormat;
use serde_json::{Number, Value as JsonValue};
use std::{collections::BTreeMap, fmt, ops::Index};

/// A local attachment collected while evaluating a behavior spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attachment {
    pub path: String,
    pub filename: String,
    pub content_type: String,
    pub key: String,
}

/// A generated value that preserves attachments independently of their encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    Attachment(Attachment),
}

impl Value {
    /// Projects nested attachment references using the selected format.
    pub fn project(&self, format: &dyn AttachmentFormat) -> Result<JsonValue, UnsupportedAttachment> {
        Ok(match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Number(value) => JsonValue::Number(value.clone()),
            Self::String(value) => JsonValue::String(value.clone()),
            Self::Array(values) => {
                JsonValue::Array(values.iter().map(|value| value.project(format)).collect::<Result<_, _>>()?)
            }
            Self::Object(values) => JsonValue::Object(
                values
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), value.project(format)?)))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Attachment(attachment) => format.attachment(attachment).ok_or(UnsupportedAttachment)?,
        })
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(value) => value.as_i64(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => value.as_u64(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => value.as_f64(),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl From<JsonValue> for Value {
    fn from(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(value),
            JsonValue::Number(value) => Self::Number(value),
            JsonValue::String(value) => Self::String(value),
            JsonValue::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            JsonValue::Object(values) => {
                Self::Object(values.into_iter().map(|(key, value)| (key, Self::from(value))).collect())
            }
        }
    }
}

macro_rules! from_json {
    ($($ty:ty),* $(,)?) => {
        $(impl From<$ty> for Value {
            fn from(value: $ty) -> Self {
                JsonValue::from(value).into()
            }
        })*
    };
}

from_json!(
    bool, String, &str, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64
);

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(values: Vec<T>) -> Self {
        Self::Array(values.into_iter().map(Into::into).collect())
    }
}

impl Index<&str> for Value {
    type Output = Value;

    fn index(&self, key: &str) -> &Self::Output {
        self.get(key).unwrap_or(&Value::Null)
    }
}

impl Index<usize> for Value {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        self.as_array().and_then(|values| values.get(index)).unwrap_or(&Value::Null)
    }
}

/// The selected format has no representation for attachments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedAttachment;

impl fmt::Display for UnsupportedAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the selected format does not support attachments")
    }
}

impl std::error::Error for UnsupportedAttachment {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Format;

    impl AttachmentFormat for Format {
        fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
            Some(json!({"file": attachment.filename, "reference": attachment.key}))
        }
    }

    #[test]
    fn projects_only_typed_attachments() {
        let ordinary = json!({
            "type": "braintrust_attachment",
            "filename": "report.txt",
            "content_type": "text/plain",
            "key": "local"
        });
        let attachment = Attachment {
            path: "report.txt".to_owned(),
            filename: "report.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            key: "local".to_owned(),
        };
        let value = Value::Array(vec![ordinary.clone().into(), Value::Attachment(attachment)]);
        assert_eq!(
            value.project(&Format).unwrap(),
            json!([ordinary, {"file": "report.txt", "reference": "local"}])
        );
        assert_eq!(value.project(&()).unwrap_err(), UnsupportedAttachment);
        assert_eq!(Value::from(ordinary.clone()).project(&()).unwrap(), ordinary);
    }
}

/// Expressions evaluated by mock data and emitted by mock code.
pub mod expr {
    use crate::dsl::{BinOp, CtxRef, EnglishOperation, Number, RefId, ScorerArg, SrcRange, UnaryOp};

    pub use bts_lang::NOISE_SIZE_CAP;

    /// A compiled value or an expression left for a consumer to evaluate.
    ///
    /// Compilation folds expressions whose values are known. References, random
    /// functions, and expressions depending on runtime data remain represented here.
    /// This type does not provide an evaluator.
    #[derive(Debug, Clone)]
    pub enum Value {
        /// A string value.
        Str(String),
        /// Text containing interpolated values.
        Template(Template),
        /// A numeric value.
        Num(Number),
        /// A boolean value.
        Bool(bool),
        /// An explicit null value.
        Null,
        /// An ordered collection.
        Array(Array),
        /// A collection of named entries.
        Object(Object),
        /// An attachment reference to be materialized by a consumer.
        Attachment {
            /// The attachment path written in source.
            path: String,
            /// The name to use when presenting the attachment.
            filename: String,
            /// The attachment media type.
            content_type: String,
            /// The attachment key used to connect payloads with uploads.
            key: String,
        },
        /// A scoped variable resolved by name during evaluation.
        VarRef(String),
        /// A validated runtime context reference.
        CtxRef(CtxRef),
        /// An argument supplied when a scorer is invoked.
        ArgRef(ScorerArg),
        /// A block reference resolved through the owning model.
        BlockRef {
            /// The index of the resolved reference in [`crate::mocking::Model::refs`].
            ref_id: RefId,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
        /// A function call.
        Func {
            /// The function and its arguments.
            func: Func,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
        /// An operation with one operand.
        Unary {
            /// The operator applied to the operands.
            op: UnaryOp,
            /// The expression to which the operator applies.
            operand: Box<Value>,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
        /// An operation with two operands.
        Binary {
            /// The operator applied to the operands.
            op: BinOp,
            /// The left operand.
            lhs: Box<Value>,
            /// The right operand.
            rhs: Box<Value>,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
        /// A conditional expression that evaluates only the selected branch.
        Cond {
            /// The condition controlling this expression or branch.
            cond: Box<Value>,
            /// The expression evaluated when the condition is true.
            then: Box<Value>,
            /// The expression evaluated when the condition is false.
            otherwise: Box<Value>,
        },
        /// An element selection.
        Index {
            /// The value to index, slice, or inspect.
            target: Box<Value>,
            /// The expression selecting an element.
            index: Box<Value>,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
        /// A selection between optional bounds.
        Slice {
            /// The value to index, slice, or inspect.
            target: Box<Value>,
            /// The inclusive lower bound, or `None` to start at the beginning.
            start: Option<Box<Value>>,
            /// The exclusive upper bound, or `None` to continue to the end.
            end: Option<Box<Value>>,
            /// The expression’s byte range in the original source.
            range: SrcRange,
        },
    }

    /// A validated function call whose result is computed by a consumer.
    ///
    /// Argument structure is checked during compilation. Runtime values can still
    /// cause failures such as an invalid index or numeric overflow.
    #[derive(Debug, Clone)]
    pub enum Func {
        /// Reads a binding from the selected graph occurrence.
        FlowRead {
            node: super::super::super::NodeId,
            binding: usize,
        },
        /// Caches an edge or fork property for its source occurrence.
        FlowProperty { binding: usize, value: Box<Value> },
        /// Reads occurrence-local runtime context.
        FlowContext(Vec<String>),
        /// Requires a value supplied by an incoming edge.
        FlowInput { binding: usize, ty: bts_lang::FieldType },
        /// Selects one eligible fork branch, or minus one when none qualify.
        ForkPick(Vec<Vec<Value>>),
        /// Requires every listed junction input before activating its target.
        Arrivals { ready: Box<Value> },
        /// Folds items into one value, starting from an initial value.
        ///
        /// The combine expression reads the running value through `FoldAccumulator`
        /// and the current item through `FoldArrival`.
        Fold {
            items: Box<Value>,
            initial: Box<Value>,
            combine: Box<Value>,
        },
        /// The running value of the innermost fold.
        FoldAccumulator,
        /// The current item of the innermost fold, as a record with a `value` key.
        FoldArrival,
        /// Checks the value delivered to a schema-constrained field.
        Check { value: Box<Value>, ty: bts_lang::FieldType },
        /// An English language operation evaluated by the consumer.
        English {
            /// The English operation to perform.
            operation: EnglishOperation,
            /// Arguments in authored order, including optional arguments when supplied.
            args: Vec<Value>,
        },
        /// An attachment reference to be materialized by a consumer.
        Attachment {
            /// The attachment path written in source.
            path: String,
            /// The attachment media type.
            content_type: String,
        },
        /// One selected alternative.
        Choice(Vec<Value>),
        /// A uniform sample between numeric bounds.
        Range(Range),
        /// A sample selected using relative weights.
        Weighted(Vec<WeightedOption>),
        /// A normal distribution sample.
        Normal {
            /// The distribution mean.
            mean: f64,
            /// The distribution standard deviation.
            stddev: f64,
        },
        /// A lognormal distribution sample.
        Lognormal {
            /// The distribution median.
            median: f64,
            /// The standard deviation in logarithmic space.
            sigma: f64,
        },
        /// An exponential distribution sample.
        Exponential {
            /// The distribution mean.
            mean: f64,
        },
        /// A Pareto distribution sample.
        Pareto {
            /// The lower bound.
            min: f64,
            /// The Pareto shape parameter.
            shape: f64,
        },
        /// A beta distribution sample.
        Beta {
            /// The first beta distribution parameter.
            alpha: f64,
            /// The second beta distribution parameter.
            beta: f64,
        },
        /// A Poisson distribution sample.
        Poisson {
            /// The distribution mean.
            mean: f64,
        },
        /// A boolean sampled with the given probability.
        Chance {
            /// The probability of returning true.
            probability: f64,
        },
        /// Converts text to uppercase.
        Upper {
            /// The text expression to evaluate.
            text: Box<Value>,
        },
        /// Converts text to lowercase.
        Lower {
            /// The text expression to evaluate.
            text: Box<Value>,
        },
        /// Removes surrounding whitespace.
        Trim {
            /// The text expression to evaluate.
            text: Box<Value>,
        },
        /// Replaces matching text.
        Replace {
            /// The text expression to evaluate.
            text: Box<Value>,
            /// The text to replace.
            from: Box<Value>,
            /// The replacement text.
            to: Box<Value>,
        },
        /// Splits text using a separator.
        Split {
            /// The text expression to evaluate.
            text: Box<Value>,
            /// The separator used to split or join text.
            separator: Box<Value>,
        },
        /// Joins array elements using a separator.
        Join {
            /// The array expression to join.
            array: Box<Value>,
            /// The separator used to split or join text.
            separator: Box<Value>,
        },
        /// Substitutes named values into text.
        Render {
            /// The template to render.
            template: Box<Value>,
            /// The named scalar values.
            values: Box<Value>,
        },
        /// Joins nonempty text components.
        Compose {
            /// The text components, including optional nulls.
            array: Box<Value>,
            /// The separator between components.
            separator: Box<Value>,
        },
        /// Samples array positions without replacement in source order.
        Sample {
            /// The source array.
            array: Box<Value>,
            /// The number of positions to select.
            count: Box<Value>,
        },
        /// Tests whether the target contains a value.
        Contains {
            /// The value to index, slice, or inspect.
            target: Box<Value>,
            /// The value or text to search for.
            needle: Box<Value>,
        },
        /// Tests a text prefix.
        StartsWith {
            /// The text expression to evaluate.
            text: Box<Value>,
            /// The prefix to test.
            prefix: Box<Value>,
        },
        /// Tests a text suffix.
        EndsWith {
            /// The text expression to evaluate.
            text: Box<Value>,
            /// The suffix to test.
            suffix: Box<Value>,
        },
        /// Measures the length of a value.
        Len {
            /// The value to index, slice, or inspect.
            target: Box<Value>,
        },
        /// Counts tokens using the consumer’s tokenizer.
        Tokens {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
        },
        /// Interleaves literal pieces with evaluated arguments.
        Format {
            /// Literal text around each format argument. There is one more piece than argument.
            pieces: Vec<String>,
            /// Function arguments in source order.
            args: Vec<Value>,
        },
        /// Restricts a number to the supplied bounds.
        Clamp {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
            /// The lower bound.
            min: Box<Value>,
            /// The upper bound.
            max: Box<Value>,
        },
        /// Rounds a number to the nearest integer.
        Round {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
        },
        /// Rounds a number downward.
        Floor {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
        },
        /// Rounds a number upward.
        Ceil {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
        },
        /// Takes the absolute value.
        Abs {
            /// The value expression to inspect or evaluate.
            value: Box<Value>,
        },
        /// Finds the smallest argument.
        Min(Vec<Value>),
        /// Finds the largest argument.
        Max(Vec<Value>),
        /// Produces a UUID string.
        Uuid,
        /// Produces random hexadecimal text.
        Hex {
            /// The requested output length.
            length: usize,
        },
        /// Produces random alphanumeric text.
        Alphanum {
            /// The requested output length.
            length: usize,
        },
        /// Produces a requested amount of generated noise.
        Noise {
            /// The requested noise size.
            size: Box<Value>,
        },
    }

    /// One candidate value and its relative sampling weight.
    #[derive(Debug, Clone)]
    pub struct WeightedOption {
        /// The value expression to inspect or evaluate.
        pub value: Value,
        /// The relative sampling weight. Zero disables this candidate.
        pub weight: f64,
    }

    /// Inclusive bounds for an integer or floating-point sample.
    #[derive(Debug, Clone, Copy)]
    pub enum Range {
        /// An integer value or integer bounds.
        Int {
            /// The lower bound.
            min: i64,
            /// The upper bound.
            max: i64,
        },
        /// A floating-point value or floating-point bounds.
        Float {
            /// The lower bound.
            min: f64,
            /// The upper bound.
            max: f64,
        },
    }

    /// Text interleaved with values to be resolved by a consumer.
    #[derive(Debug, Clone)]
    pub struct Template {
        /// Template parts in source order.
        pub parts: Vec<Part>,
    }

    /// One literal or evaluated part of a text template.
    #[derive(Debug, Clone)]
    pub enum Part {
        /// Literal text copied into the result.
        Lit(String),
        /// A reference expression whose meaning depends on its context.
        Ref(CtxRef),
        /// A scoped variable resolved by name during evaluation.
        VarRef(String),
        /// An expression whose interpolated value is checked at runtime.
        Dynamic(Value),
    }

    /// An ordered collection of values and runtime spreads.
    #[derive(Debug, Clone)]
    pub struct Array {
        /// Entries in source order.
        pub elem: Vec<ArrayElem>,
    }

    /// A single array value or a collection expanded into the array.
    #[derive(Debug, Clone)]
    pub enum ArrayElem {
        /// One value in an array.
        Item(Value),
        /// A collection expanded into its surrounding container.
        Spread(Value),
    }

    /// An ordered collection of named value expressions.
    #[derive(Debug, Clone)]
    pub struct Object {
        /// Entries in source order.
        pub elem: Vec<ObjectField>,
    }

    /// A key and its compiled value expression.
    #[derive(Debug, Clone)]
    pub struct ObjectField {
        /// The key identifying this entry.
        pub key: String,
        /// The value expression to inspect or evaluate.
        pub value: Value,
    }
}
