//! The functions Braintrust contributes to expressions.
//!
//! [`DESCRIPTORS`] is what the language checks a call against. [`FUNCTIONS`] is
//! what runs: an evaluator for data generation and the helper a generated agent
//! calls. The two lists name the same functions in the same order.

use crate::mocking::data::{Attachment, Value};
use crate::mocking::{EvaluateError, Evaluation, Function, Helper, Known};
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
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.json"),
            name: "json",
            syntax: "json(value)",
            summary: "The compact JSON text of a value, with the keys of an object in alphabetical order, on evaluation. Use it where an API carries JSON as a string, such as the arguments of a tool call in an assistant message or the content of a tool message. A string is given its JSON quotes.",
            examples: &[
                "json({ claim_id: \"CLM-204817\", status: \"open\" })",
                "json(settings.prompt)",
            ],
            rules: CALL_RULES,
        },
        params: &[ParamDesc {
            name: "value",
            accepts: &[
                FieldType::String,
                FieldType::Number,
                FieldType::Boolean,
                FieldType::Array,
                FieldType::Object,
            ],
            nullable: true,
            optional: false,
            constant: false,
            bounds: None,
        }],
        returns: FieldType::String,
        shape: None,
    },
    FunctionDesc {
        doc: FuncDesc {
            tags: &[],
            id: Id::new("func.decimal"),
            name: "decimal",
            syntax: "decimal(value, places)",
            summary: "Rounds a number to a constant number of decimal places, from 0 to 9, and gives a float, on evaluation. Halves round up. Use it for values a real system reports to a few places, such as a score of 0.86 or a price of 12.5.",
            examples: &["decimal(range(0.82, 0.95), 2)", "decimal(lognormal(40, 0.6), 1)"],
            rules: CALL_RULES,
        },
        params: &[
            ParamDesc {
                name: "value",
                accepts: &[FieldType::Number],
                nullable: false,
                optional: false,
                constant: false,
                bounds: None,
            },
            ParamDesc {
                name: "places",
                accepts: &[FieldType::Number],
                nullable: false,
                optional: false,
                constant: true,
                bounds: Some((0, 9)),
            },
        ],
        returns: FieldType::Number,
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
            plain: false,
            sure: Some(plain_value),
            returns: None,
            draws: None,
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
            plain: false,
            sure: Some(plain_value),
            returns: None,
            draws: None,
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
            plain: false,
            sure: Some(noise_fits),
            returns: Some(text),
            draws: None,
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
            plain: false,
            sure: Some(noise_fits),
            returns: Some(text),
            draws: None,
            text: TS_NOISE,
            operation: None,
            runtime: None,
            scorer: None,
        }),
    },
    Function {
        name: "json",
        check: None,
        data: Some(json),
        python: Some(Helper {
            name: "_json_text",
            reads_as: Some("json_text"),
            random: false,
            seeded: false,
            counts: false,
            texts: false,
            plain: false,
            sure: Some(plain_value),
            returns: Some(text),
            draws: None,
            text: PY_JSON,
            operation: None,
            runtime: None,
            scorer: None,
        }),
        typescript: Some(Helper {
            name: "_json_text",
            reads_as: Some("jsonText"),
            random: false,
            seeded: false,
            counts: false,
            texts: false,
            plain: false,
            sure: Some(plain_value),
            returns: Some(text),
            draws: None,
            text: TS_JSON,
            operation: None,
            runtime: None,
            scorer: None,
        }),
    },
    Function {
        name: "decimal",
        check: None,
        data: Some(decimal),
        python: Some(Helper {
            name: "_decimal",
            reads_as: Some("to_places"),
            random: false,
            seeded: false,
            counts: false,
            texts: false,
            plain: false,
            sure: Some(decimal_fits),
            returns: Some(decimal_gives),
            draws: None,
            text: PY_DECIMAL,
            operation: None,
            runtime: None,
            scorer: None,
        }),
        typescript: Some(Helper {
            name: "_decimal",
            reads_as: Some("toPlaces"),
            random: false,
            seeded: false,
            counts: false,
            texts: false,
            plain: false,
            sure: Some(decimal_fits),
            returns: Some(decimal_gives),
            draws: None,
            text: TS_DECIMAL,
            operation: None,
            runtime: None,
            scorer: None,
        }),
    },
];

// `tokens` and `json` write out whatever plain data they are given
fn plain_value(args: &[Known]) -> bool {
    matches!(args, [value] if value.plain())
}

fn text(_: &[Known]) -> Known {
    Known::Text(None)
}

// a written whole number from zero up to a limit
fn written_count(value: &Known, limit: f64) -> Option<f64> {
    match value {
        Known::Number { low, high, whole: true } if low == high && (0.0..=limit).contains(low) => Some(*low),
        _ => None,
    }
}

// `noise` raises for a size that is no count or is past its limit
fn noise_fits(args: &[Known]) -> bool {
    matches!(args, [size] if written_count(size, 8_388_608.0).is_some())
}

// the number `decimal` rounds, which scaling by its places keeps finite, and those places
fn decimal_parts(args: &[Known]) -> Option<(f64, f64, f64)> {
    let [Known::Number { low, high, .. }, places] = args else {
        return None;
    };
    let places = written_count(places, 9.0)?;
    let safe = 9_007_199_254_740_991.0;
    (*low >= -safe && *high <= safe).then_some((*low, *high, places))
}

// `decimal` raises for what is no finite number, and for places that are no whole number from 0 to 9
fn decimal_fits(args: &[Known]) -> bool {
    decimal_parts(args).is_some()
}

// rounding moves a number by at most half of its last place
fn decimal_gives(args: &[Known]) -> Known {
    match decimal_parts(args) {
        Some((low, high, places)) => {
            let half = 0.5 / 10f64.powf(places);
            Known::Number {
                low: low - half,
                high: high + half,
                whole: false,
            }
        }
        None => Known::NUMBER,
    }
}

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

// the same text the token count is taken from: compact, with keys in order
fn json(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [value] = args else { return Err(EvaluateError::Shape) };
    let projected = (evaluation.project(value)).map_err(|_| EvaluateError::UnsupportedAttachment)?;
    Ok(Value::String(projected.to_string()))
}

// half up at the last place kept, by the same arithmetic in data and in an agent
fn decimal(args: &[Value], _: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::Number(value), Value::Number(places)] = args else {
        return Err(EvaluateError::Shape);
    };
    let (Some(value), Some(places)) = (value.as_f64(), places.as_u64().filter(|places| *places <= 9)) else {
        return Err(EvaluateError::Invalid(
            "decimal takes a number and a whole number of places from 0 to 9".into(),
        ));
    };
    let scale = 10f64.powi(places as i32);
    let rounded = (value * scale + 0.5).floor() / scale;
    serde_json::Number::from_f64(rounded)
        .map(Value::Number)
        .ok_or_else(|| EvaluateError::Invalid("decimal takes a finite number".into()))
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

const PY_JSON: &str = r#"
def _json_text(value):
    return _json.dumps(_value(value), ensure_ascii=False, separators=(",", ":"), sort_keys=True)
"#;

const TS_JSON: &str = r#"
function _json_text(value: unknown): string {
    // a float that is whole keeps its point, as it does in generated data
    const write = (given: unknown): string => {
        const item: any = _force(given);
        if (item instanceof _Float) return Number.isInteger(item.value) ? item.value.toFixed(1) : String(item.value);
        if (Array.isArray(item)) return "[" + item.map(write).join(",") + "]";
        if (item !== null && typeof item === "object") {
            return "{" + Object.keys(item).sort().map(key => JSON.stringify(key) + ":" + write(item[key])).join(",") + "}";
        }
        return item === undefined ? "null" : JSON.stringify(item);
    };
    return write(value);
}
"#;

const PY_DECIMAL: &str = r#"
def _decimal(value, places):
    import math
    value, places = _num(value), _count(places)
    if places > 9: raise ValueError("decimal takes a number and a whole number of places from 0 to 9")
    scale = float(10 ** places)
    return math.floor(value * scale + 0.5) / scale
"#;

const TS_DECIMAL: &str = r#"
function _decimal(value: unknown, places: unknown): number {
    const kept = _count(places);
    if (kept > 9) throw new RangeError("decimal takes a number and a whole number of places from 0 to 9");
    const scale = 10 ** kept;
    return _float(Math.floor(_num(value) * scale + 0.5) / scale);
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
