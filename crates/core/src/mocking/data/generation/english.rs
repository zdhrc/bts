use crate::dsl::EnglishOperation;
use crate::english::{self, RealizeOptions, Sentence, VariationOptions};
use rand::Rng;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaryOptions {
    #[serde(default = "enabled")]
    negation: bool,
    #[serde(default = "enabled")]
    condition_order: bool,
    #[serde(default)]
    rendering: RealizeOptions,
}

fn enabled() -> bool {
    true
}

impl Default for VaryOptions {
    fn default() -> Self {
        Self {
            negation: true,
            condition_order: true,
            rendering: RealizeOptions::default(),
        }
    }
}

pub(super) fn evaluate<R: Rng + ?Sized>(operation: EnglishOperation, args: &[Value], rng: &mut R) -> Result<Value, String> {
    run(operation, args, rng).map_err(|error| format!("{}: {error}", operation.as_str()))
}

fn run<R: Rng + ?Sized>(operation: EnglishOperation, args: &[Value], rng: &mut R) -> Result<Value, String> {
    use EnglishOperation::*;
    let (min, max) = match operation {
        Realize | Vary | FormatNumber | FormatDate | FormatTime => (1, 2),
        Combine | Refer | Remember => (2, 3),
        Unmentioned => (2, 2),
    };
    if !(min..=max).contains(&args.len()) {
        return Err(format!("expected {min} to {max} arguments"));
    }
    match operation {
        Realize => {
            let sentence: Sentence = decode(&args[0])?;
            let options = options(args.get(1))?;
            english::realize(&sentence, &options).map(Value::String).map_err(message)
        }
        Vary => {
            let options: VaryOptions = options(args.get(1))?;
            let variation = VariationOptions {
                negation: options.negation,
                condition_order: options.condition_order,
            };
            let output = if let Some(text) = args[0].as_str() {
                english::vary(text, &variation, rng)
            } else {
                let sentence: Sentence = decode(&args[0])?;
                english::vary_sentence(&sentence, &options.rendering, &variation, rng)
            };
            output.map(Value::String).map_err(message)
        }
        Combine => {
            let sentences: Vec<Sentence> = decode(&args[0])?;
            let relation = decode(&args[1])?;
            let options = options(args.get(2))?;
            english::composition::combine(&sentences, relation, &options)
                .map(Value::String)
                .map_err(message)
        }
        Refer => {
            let entity = decode(&args[0])?;
            let context = decode(&args[1])?;
            let options = options(args.get(2))?;
            english::discourse::refer(&entity, &context, &options)
                .map(Value::String)
                .map_err(message)
        }
        Remember => {
            let context = decode(&args[0])?;
            let entities: Vec<english::discourse::Entity> = decode(&args[1])?;
            let facts: Vec<String> = options(args.get(2))?;
            let context = english::discourse::remember(&context, &entities, &facts).map_err(message)?;
            serde_json::to_value(context).map_err(message)
        }
        Unmentioned => {
            let details: Vec<english::discourse::Detail> = decode(&args[0])?;
            let context = decode(&args[1])?;
            let selected = english::discourse::unmentioned(&details, &context).map_err(message)?;
            serde_json::to_value(selected).map_err(message)
        }
        FormatNumber => {
            let number = match &args[0] {
                Value::String(value) => value.clone(),
                Value::Number(number) => number.to_string(),
                _ => return Err("expected a number or decimal string".into()),
            };
            let options = options(args.get(1))?;
            english::formatting::format_number(&number, &options)
                .map(Value::String)
                .map_err(message)
        }
        FormatDate => {
            let date: String = decode(&args[0])?;
            let options = options(args.get(1))?;
            english::formatting::format_date(&date, &options)
                .map(Value::String)
                .map_err(message)
        }
        FormatTime => {
            let time: String = decode(&args[0])?;
            let options = options(args.get(1))?;
            english::formatting::format_time(&time, &options)
                .map(Value::String)
                .map_err(message)
        }
    }
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(message)
}

fn options<T: DeserializeOwned + Default>(value: Option<&Value>) -> Result<T, String> {
    value.map(decode).unwrap_or_else(|| Ok(T::default()))
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}
