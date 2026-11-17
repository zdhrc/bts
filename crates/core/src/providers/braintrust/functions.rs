//! The functions Braintrust contributes to expressions.
//!
//! [`DESCRIPTORS`] is what the language checks a call against. [`FUNCTIONS`] is
//! what runs: an evaluator for data generation and the helper a generated agent
//! calls. The two lists name the same functions in the same order.

use crate::mocking::data::{Attachment, Value};
use crate::mocking::{EvaluateError, Evaluation, Function, Helper};
use bts_lang::spec::{FUNC_ARG_TYPES_RULE, FUNC_ARITY_RULE, FuncDesc, Id, RuleDesc};
use bts_lang::{FieldType, FunctionDesc, ParamDesc};
use std::path::Path;
use std::sync::LazyLock;
use tiktoken_rs::CoreBPE;
use uuid::Uuid;

/// The largest noise size, limiting accidental allocations during evaluation.
pub const NOISE_SIZE_CAP: i64 = 8 << 20;

const NOISE_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const CALL_RULES: &[RuleDesc] = &[FUNC_ARITY_RULE, FUNC_ARG_TYPES_RULE];

const NOISE_RULES: &[RuleDesc] = &[
    FUNC_ARITY_RULE,
    FUNC_ARG_TYPES_RULE,
    RuleDesc {
        tags: &[],
        id: Id::new("rule.noise-size"),
        summary: "`noise` takes an integer size between 0 and 8388608 (8 MiB); a constant violation is rejected during validation, and a dynamic one fails the run during evaluation.",
    },
];

/// What the language checks a call to each function against.
pub const DESCRIPTORS: &[FunctionDesc] = &[
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.tokens"),
            name: "tokens",
            syntax: "tokens(value)",
            summary: "The exact number of tokens in a string, or in the JSON serialization of an array or object, as an integer, on evaluation. Null has no tokens and counts 0. Counts with the o200k_base encoding used by current OpenAI models.",
            examples: &["tokens(\"What is the weather in Tokyo?\")", "tokens(settings.prompt)"],
            rules: CALL_RULES,
        },
        params: &[ParamDesc {
            name: "value",
            accepts: &[FieldType::String, FieldType::Array, FieldType::Object],
            nullable: true,
            optional: false,
            constant: false,
            bounds: None,
        }],
        returns: FieldType::Number,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.attachment"),
            name: "attachment",
            syntax: "attachment(path, content_type)",
            summary: "References a local file as an attachment in a logged value. The path may be absolute or relative to the shape file's directory.",
            examples: &[
                "attachment(\"report.pdf\", \"application/pdf\")",
                "attachment(\"/tmp/report.pdf\", \"application/pdf\")",
            ],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "path",
                accepts: &[FieldType::String],
                nullable: false,
                optional: false,
                constant: true,
                bounds: None,
            },
            ParamDesc {
                name: "content_type",
                accepts: &[FieldType::String],
                nullable: false,
                optional: false,
                constant: true,
                bounds: None,
            },
        ],
        returns: FieldType::Object,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.noise"),
            name: "noise",
            syntax: "noise(size)",
            summary: "An opaque random base64-style string of the given size in bytes, for adding payload weight where storage size matters; the size may be dynamic, so payloads can vary per evaluation.",
            examples: &["noise(250000)", "noise(round(lognormal(80000, 0.9)))"],
            rules: NOISE_RULES,
        },
        params: &[ParamDesc {
            name: "size",
            accepts: &[FieldType::Number],
            nullable: false,
            optional: false,
            constant: false,
            bounds: Some((0, NOISE_SIZE_CAP)),
        }],
        returns: FieldType::String,
        shape: None,
    },
];

/// What runs each function, for data generation and for generated agents.
pub static FUNCTIONS: &[Function] = &[
    Function {
        name: "tokens",
        check: None,
        data: Some(tokens),
        python: Some(Helper {
            name: "_tokens",
            reads_as: Some("count_tokens"),
            random: false,
            seeded: false,
            counts: true,
            texts: false,
            text: PY_TOKENS,
            operation: None,
            runtime: None,
            scorer: None,
        }),
        typescript: Some(Helper {
            name: "_tokens",
            reads_as: Some("countTokens"),
            random: false,
            seeded: false,
            counts: true,
            texts: false,
            text: TS_TOKENS,
            operation: None,
            runtime: None,
            scorer: None,
        }),
    },
    // an agent has no way to package a local file, so only data generation attaches one
    Function {
        name: "attachment",
        check: Some(check_attachment),
        data: Some(attachment),
        python: None,
        typescript: None,
    },
    Function {
        name: "noise",
        check: None,
        data: Some(noise),
        python: Some(Helper {
            name: "_noise",
            reads_as: None,
            random: true,
            seeded: false,
            counts: false,
            texts: false,
            text: PY_NOISE,
            operation: None,
            runtime: None,
            scorer: None,
        }),
        typescript: Some(Helper {
            name: "_noise",
            reads_as: None,
            random: true,
            seeded: false,
            counts: false,
            texts: false,
            text: TS_NOISE,
            operation: None,
            runtime: None,
            scorer: None,
        }),
    },
];

// building the encoder parses the embedded vocab, do it once
static BPE: LazyLock<CoreBPE> = LazyLock::new(|| tiktoken_rs::o200k_base().expect("the embedded vocab parses"));

/// Counts the tokens of text with the encoding `tokens` uses.
pub fn count_tokens(text: &str) -> usize {
    BPE.encode_ordinary(text).len()
}

fn tokens(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [value] = args else { return Err(EvaluateError::Shape) };
    // containers count their compact json serialization
    let count = match value {
        // nothing was said, so nothing is counted
        Value::Null => 0,
        Value::String(text) => count_tokens(text),
        value => {
            let projected = (evaluation.project(value)).map_err(|_| EvaluateError::UnsupportedAttachment)?;
            count_tokens(&projected.to_string())
        }
    };
    Ok(Value::from(count as i64))
}

fn check_attachment(args: &[bts_lang::Value]) -> Result<(), String> {
    let [bts_lang::Value::Str(path), bts_lang::Value::Str(content_type)] = args else {
        return Err("takes a constant path and a constant MIME type".to_owned());
    };
    if Path::new(path).file_name().is_none() {
        return Err("path must be a constant file path".to_owned());
    }
    if content_type.is_empty() {
        return Err("content_type must be a non-empty constant MIME type".to_owned());
    }
    Ok(())
}

fn attachment(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::String(path), Value::String(content_type)] = args else {
        return Err(EvaluateError::Shape);
    };
    let filename = Path::new(path)
        .file_name()
        .ok_or(EvaluateError::Invalid("attachment path does not name a file".into()))?
        .to_string_lossy()
        .into_owned();
    let mut key_bytes = [0; 16];
    evaluation.fill(&mut key_bytes);
    key_bytes[6] = (key_bytes[6] & 0x0f) | 0x40;
    key_bytes[8] = (key_bytes[8] & 0x3f) | 0x80;
    Ok(Value::Attachment(Attachment {
        path: path.clone(),
        filename,
        content_type: content_type.clone(),
        key: Uuid::from_bytes(key_bytes).to_string(),
    }))
}

fn noise(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    // a reference-fed size settles its type here
    let [Value::Number(size)] = args else {
        return Err(EvaluateError::Shape);
    };
    match size.as_i64() {
        Some(size) if (0..=NOISE_SIZE_CAP).contains(&size) => {
            Ok(Value::String(evaluation.random_text(NOISE_CHARSET, size as usize)))
        }
        _ => Err(EvaluateError::Invalid(
            "noise size is not an integer between 0 and 8388608".into(),
        )),
    }
}

const PY_TOKENS: &str = r#"
def _tokens(value):
    import tiktoken
    value = _value(value)
    if value is None: return 0
    text = value if isinstance(value, str) else _json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return len(tiktoken.get_encoding("o200k_base").encode_ordinary(text))
"#;

const TS_TOKENS: &str = r#"
import { getEncoding as _getEncoding } from "js-tiktoken";
const _encoding = _getEncoding("o200k_base");
function _tokens(value: unknown): number {
    value = _serialize(value);
    if (value === null || value === undefined) return 0;
    const sorted = (item: any): any => Array.isArray(item) ? item.map(sorted) : item !== null && typeof item === "object" ? Object.fromEntries(Object.keys(item).sort().map(key => [key, sorted(item[key])])) : item;
    return _encoding.encode(typeof value === "string" ? value : JSON.stringify(sorted(value)), [], []).length;
}
"#;

const PY_NOISE: &str = r#"
def _noise(size, random_source):
    size = _count(size)
    if size > 8388608: raise ValueError("noise exceeds 8388608 characters")
    return _random_text("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/", size, random_source)
"#;

const TS_NOISE: &str = r#"
function _noise(size: unknown, randomSource: () => number): string {
    const length = _count(size);
    if (length > 8388608) throw new RangeError("noise exceeds 8388608 characters");
    return _random_text("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/", length, randomSource);
}
"#;
