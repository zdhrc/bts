use crate::browser;
use serde::Serialize;
use wasm_bindgen::prelude::*;

// JSON encoding matches native dry-run, including object maps and JSON numbers.
fn json(value: &impl Serialize) -> Result<JsValue, JsValue> {
    let encoded = serde_json::to_string(value).map_err(|error| JsValue::from_str(&error.to_string()))?;
    js_sys::JSON::parse(&encoded)
}

#[wasm_bindgen]
pub fn version() -> String {
    browser::version().into()
}

#[wasm_bindgen(js_name = languageReference)]
pub fn language_reference() -> String {
    browser::language_reference()
}

#[wasm_bindgen(unchecked_return_type = "Diagnostic[]")]
pub fn check(source: &str) -> Result<JsValue, JsValue> {
    json(&browser::check(source))
}

#[wasm_bindgen(unchecked_return_type = "EventBatch")]
pub fn generate(
    source: &str,
    #[wasm_bindgen(unchecked_param_type = "GenerationOptions")] options: JsValue,
) -> Result<JsValue, JsValue> {
    let encoded = js_sys::JSON::stringify(&options)
        .map_err(|_| json(&browser::Error::message("options must be a JSON object")).unwrap_or_else(|error| error))?;
    let encoded = encoded
        .as_string()
        .ok_or_else(|| json(&browser::Error::message("options must be a JSON object")).unwrap_or_else(|error| error))?;
    let options =
        serde_json::from_str(&encoded).map_err(|error| json(&browser::Error::message(error)).unwrap_or_else(|error| error))?;
    browser::generate(source, options)
        .map_err(|error| json(&error).unwrap_or_else(|error| error))
        .and_then(|events| json(&events))
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export interface Location {
  /** Half-open UTF-8 byte offsets; one-based Unicode scalar line/column positions. */
  start: number; end: number; line: number; column: number; endLine: number; endColumn: number;
}
export interface Diagnostic { phase: string; message: string; location: Location | null }
export interface GenerationOptions {
  count: number;
  /** Decimal u64 string (0 through 18446744073709551615). */
  seed: string;
  /** RFC 3339 timestamps with explicit offsets, between 1970 and 9999 UTC. */
  start: string; end: string;
  distribution?: 'linear' | 'sine';
}
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface EventBatch { events: { [key: string]: Json }[] }
/** generate throws this object for validation/evaluation failures. */
export interface BtsError { message: string; diagnostics: Diagnostic[] }
"#;
