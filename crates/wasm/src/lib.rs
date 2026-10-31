use serde::{Serialize, Serializer, ser::SerializeStruct};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export interface Diagnostic {
  phase: "lexing" | "parsing" | "validation" | "generation";
  message: string;
  range: { start: number; end: number };
}
export interface ValidationResult { valid: boolean; diagnostics: Diagnostic[] }
export interface OperationError {
  stage: string;
  message: string;
  diagnostics: Diagnostic[];
  details?: unknown;
}
export type OperationResult<T> = { ok: true; value: T } | { ok: false; error: OperationError };
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export interface GenerateOptions {
  count?: number;
  rate_per_second?: number;
  over_ms?: number;
  offset_ms?: number;
  start_ms?: number;
  until_ms?: number;
  seed: string;
  distribution?: "linear" | "sine";
  filter?: string;
  id_prefix?: string;
}
export interface AttachmentDescriptor { path: string; filename: string; content_type: string; key: string }
export interface GenerateOutput {
  provider: "braintrust";
  seed: string;
  trace_count: number;
  event_count: number;
  payload: { events: { [key: string]: JsonValue }[] };
  attachments: AttachmentDescriptor[];
}
export type GenerateResult = OperationResult<GenerateOutput>;
"#;

#[derive(Serialize)]
struct OperationError {
    stage: &'static str,
    message: String,
    #[serde(serialize_with = "serialize_diagnostics")]
    diagnostics: Vec<bts_lang::Diag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

impl OperationError {
    fn new(stage: &'static str, message: impl ToString) -> Self {
        Self {
            stage,
            message: message.to_string(),
            diagnostics: Vec::new(),
            details: None,
        }
    }
}

fn serialize_diagnostics<S: Serializer>(diagnostics: &[bts_lang::Diag], serializer: S) -> Result<S::Ok, S::Error> {
    diagnostics
        .iter()
        .map(WebDiagnostic)
        .collect::<Vec<_>>()
        .serialize(serializer)
}

fn serialize(value: &impl Serialize) -> Result<JsValue, JsValue> {
    fn check_numbers(value: &serde_json::Value) -> Result<(), JsValue> {
        match value {
            serde_json::Value::Number(number) => {
                if number.as_u64().is_some_and(|n| n > 9_007_199_254_740_991)
                    || number.as_i64().is_some_and(|n| n < -9_007_199_254_740_991)
                {
                    return Err(JsValue::from_str(
                        "result contains an integer outside JavaScript's safe range",
                    ));
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    check_numbers(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    check_numbers(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let value = serde_json::to_value(value).map_err(|error| JsValue::from_str(&error.to_string()))?;
    check_numbers(&value)?;
    // parsing JSON preserves authored keys such as __proto__ as own properties
    js_sys::JSON::parse(&value.to_string())
}

fn operation<T: Serialize>(result: Result<T, OperationError>) -> Result<JsValue, JsValue> {
    #[derive(Serialize)]
    struct Success<T> {
        ok: bool,
        value: T,
    }
    #[derive(Serialize)]
    struct Failure {
        ok: bool,
        error: OperationError,
    }
    match result {
        Ok(value) => match serialize(&Success { ok: true, value }) {
            Ok(value) => Ok(value),
            Err(error) => serialize(&Failure {
                ok: false,
                error: OperationError::new(
                    "serialize",
                    error
                        .as_string()
                        .unwrap_or_else(|| "result cannot be represented in JavaScript".to_owned()),
                ),
            }),
        },
        Err(error) => serialize(&Failure { ok: false, error }),
    }
}

fn options<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, OperationError> {
    serde_wasm_bindgen::from_value(value).map_err(|error| OperationError::new("options", error))
}

fn compile_source(source: &str) -> Result<bts_lang::Model, OperationError> {
    bts_lang::semantics::compile(source).map_err(|diagnostics| OperationError {
        stage: "compile",
        message: "source is invalid".to_owned(),
        diagnostics,
        details: None,
    })
}

/// Returns the shared language reference, including rules, examples, and versions.
#[wasm_bindgen(unchecked_return_type = "JsonValue")]
pub fn spec() -> Result<JsValue, JsValue> {
    serialize(&bts_lang::spec::SPEC)
}

/// Returns shared authoring guidance with instructions for this browser interface.
#[wasm_bindgen]
pub fn skill() -> String {
    let mut text = bts_core::skill::render(&bts_lang::spec::SPEC);
    text.push_str("\n## Browser workflow\n\n\
The host application provides the agent and maps these JavaScript functions to its tools. \
Use the tool names supplied by that application. Source is a string, and files are supplied by the host.\n\n\
1. Use spec() for language rules and examples.\n\
2. Author source from the user's requested population, then call validate(source). Fix diagnostics before continuing.\n\
3. Call generate(source, options) to preview Braintrust trace data. Supply a decimal-string seed, either count or rate_per_second, and either over_ms or start_ms. until_ms defaults to the current time. All time options use milliseconds since the epoch or durations in milliseconds.\n\
4. Inspect the returned events for realistic variation and coherent relationships. Native and browser builds use the same seeded random generator. Floating-point calculations can still differ slightly between platforms. Repeatable payloads also require an explicit time window and id_prefix for row and span IDs.\n\
5. Use build(source, options) to produce scorer files with an explicit project name and virtual existing files. The host saves or downloads the returned files.\n\
6. Use write(source, generationOptions, writeOptions, transport) only when the user requests delivery. The host supplies the asynchronous HTTP transport and authentication.\n\n\
Execution functions return { ok, value } or { ok, error }. Inspect error.stage, error.message, and error.diagnostics. Diagnostic ranges are UTF-8 byte offsets, not JavaScript string indexes. Generated numeric values must fit JavaScript's safe integer range.\n\n\
Attachment descriptors are returned for the host to resolve. Browser trace delivery currently rejects local attachments before making requests. Dataset, automation, and experiment synchronization are not exposed by this interface yet.\n");
    text
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateOptions {
    count: Option<u32>,
    rate_per_second: Option<f64>,
    over_ms: Option<f64>,
    offset_ms: Option<f64>,
    start_ms: Option<f64>,
    until_ms: Option<f64>,
    seed: String,
    distribution: Option<String>,
    filter: Option<String>,
    id_prefix: Option<String>,
}

#[derive(Serialize)]
struct Generated {
    provider: &'static str,
    seed: String,
    trace_count: usize,
    event_count: usize,
    payload: bts_core::providers::braintrust::mocking::data::Batch,
    attachments: Vec<BrowserAttachment>,
}

#[derive(Serialize)]
struct BrowserAttachment {
    path: String,
    filename: String,
    content_type: String,
    key: String,
}

fn milliseconds(value: f64, name: &str) -> Result<std::time::Duration, OperationError> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > 9_007_199_254_740_991.0 {
        return Err(OperationError::new(
            "options",
            format!("{name} must be a nonnegative safe integer in milliseconds"),
        ));
    }
    Ok(std::time::Duration::from_millis(value as u64))
}

fn generate_batch(source: &str, options: GenerateOptions) -> Result<Generated, OperationError> {
    use bts_core::mocking::data::request::{Volume, Window};
    use bts_core::mocking::data::{self, Format};
    use bts_core::providers::braintrust::mocking::data::Format as BraintrustFormat;
    use std::time::UNIX_EPOCH;
    let model = compile_source(source)?;
    let seed = options
        .seed
        .parse::<u64>()
        .map_err(|_| OperationError::new("options", "seed must be a decimal u64 string"))?;
    let volume = match (options.count, options.rate_per_second) {
        (Some(count), None) => Volume::Count(
            std::num::NonZeroUsize::new(count as usize)
                .ok_or_else(|| OperationError::new("options", "count must be positive"))?,
        ),
        (None, Some(rate)) => Volume::Rate(rate),
        _ => {
            return Err(OperationError::new(
                "options",
                "provide exactly one of count or rate_per_second",
            ));
        }
    };
    let until = UNIX_EPOCH
        .checked_add(milliseconds(options.until_ms.unwrap_or_else(js_sys::Date::now), "until_ms")?)
        .ok_or_else(|| OperationError::new("options", "until_ms is out of range"))?;
    let window = match (options.over_ms, options.start_ms, options.offset_ms) {
        (Some(over), None, offset) => Window::Relative {
            over: milliseconds(over, "over_ms")?,
            offset: milliseconds(offset.unwrap_or(0.0), "offset_ms")?,
        },
        (None, Some(start), None) => Window::Absolute {
            start: UNIX_EPOCH
                .checked_add(milliseconds(start, "start_ms")?)
                .ok_or_else(|| OperationError::new("options", "start_ms is out of range"))?,
            end: until,
        },
        _ => {
            return Err(OperationError::new(
                "options",
                "provide over_ms with optional offset_ms, or start_ms without offset_ms",
            ));
        }
    };
    let (count, over, until) =
        data::request::resolve(window, volume, until).map_err(|error| OperationError::new("options", error))?;
    let distribution = match options.distribution.as_deref().unwrap_or("linear") {
        "linear" => data::Distribution::Linear,
        "sine" => data::Distribution::Sine,
        _ => return Err(OperationError::new("options", "distribution must be linear or sine")),
    };
    let filter = match options.filter.as_deref() {
        Some(expression) => expression
            .parse::<bts_core::selection::filter::WriteFilter>()
            .map_err(|error| OperationError::new("filter", error))?,
        None => bts_core::selection::filter::WriteFilter::default(),
    };
    let mut batch = data::generate_filtered(model, count, over, distribution, until, seed, &filter, &BraintrustFormat)
        .map_err(|error| {
            let mut result = OperationError::new("generate", &error);
            if let data::Error::Plan(error) = error {
                result.diagnostics.push(error.into());
            }
            result
        })?;
    if let Some(prefix) = options.id_prefix {
        if prefix.is_empty() {
            return Err(OperationError::new("options", "id_prefix must not be empty"));
        }
        batch.assign_stable_ids(&prefix);
    }
    let attachments = batch
        .attachments
        .iter()
        .map(|a| BrowserAttachment {
            path: a.path.clone(),
            filename: a.filename.clone(),
            content_type: a.content_type.clone(),
            key: a.key.clone(),
        })
        .collect();
    let payload = BraintrustFormat
        .encode(&batch)
        .map_err(|error| OperationError::new("format", error))?;
    Ok(Generated {
        provider: "braintrust",
        seed: seed.to_string(),
        trace_count: batch.trace_count,
        event_count: batch.events.len(),
        payload,
        attachments,
    })
}

/// Generates Braintrust trace data with the shared compiler, generator, and provider format.
///
/// Inputs use milliseconds and a decimal-string seed. No files are read and no requests are made.
/// Authored attachment paths are returned for the host to resolve.
#[wasm_bindgen(unchecked_return_type = "GenerateResult")]
pub fn generate(
    source: &str,
    #[wasm_bindgen(unchecked_param_type = "GenerateOptions")] value: JsValue,
) -> Result<JsValue, JsValue> {
    operation(options(value).and_then(|options| generate_batch(source, options)))
}

#[derive(Serialize)]
struct ValidationResult<'a> {
    valid: bool,
    diagnostics: Vec<WebDiagnostic<'a>>,
}

struct WebDiagnostic<'a>(&'a bts_lang::Diag);

impl Serialize for WebDiagnostic<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut diagnostic = serializer.serialize_struct("Diagnostic", 3)?;
        diagnostic.serialize_field("phase", &self.0.when.to_string())?;
        diagnostic.serialize_field("message", &self.0.what)?;
        diagnostic.serialize_field("range", &WebRange(&self.0.r#where))?;
        diagnostic.end()
    }
}

struct WebRange<'a>(&'a bts_lang::SrcRange);

impl Serialize for WebRange<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut range = serializer.serialize_struct("Range", 2)?;
        range.serialize_field("start", &self.0.start)?;
        range.serialize_field("end", &self.0.end)?;
        range.end()
    }
}

/// Validates source and returns diagnostics from the first failing phase.
///
/// Invalid source returns data. Only serialization failures throw.
/// Diagnostic ranges use UTF-8 byte offsets into the original source.
#[wasm_bindgen(unchecked_return_type = "ValidationResult")]
pub fn validate(source: &str) -> Result<JsValue, JsValue> {
    let diagnostics = bts_lang::semantics::compile(source).err().unwrap_or_default();
    serde_wasm_bindgen::to_value(&ValidationResult {
        valid: diagnostics.is_empty(),
        diagnostics: diagnostics.iter().map(WebDiagnostic).collect(),
    })
    .map_err(|error| JsValue::from_str(&error.to_string()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildOptions {
    project: String,
    lang: Option<BuildLanguage>,
    #[serde(default = "build_default_root")]
    root: String,
    #[serde(default)]
    files: std::collections::BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum BuildLanguage {
    Python,
    Typescript,
}

fn build_default_root() -> String {
    "bts-scorers".to_owned()
}

struct BuildExistingFiles(std::collections::BTreeMap<String, String>);

impl bts_core::mocking::code::ExistingFiles for BuildExistingFiles {
    fn read(&self, path: &std::path::Path) -> std::io::Result<Option<String>> {
        let path = path
            .to_str()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "virtual file path is not UTF-8"))?;
        Ok(self.0.get(path).cloned())
    }
}

#[derive(serde::Serialize)]
struct BuildOutput {
    #[serde(serialize_with = "build_serialize_files")]
    files: Vec<bts_core::mocking::code::CodebaseFile>,
}

fn build_serialize_files<S: serde::Serializer>(
    files: &[bts_core::mocking::code::CodebaseFile],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(files.iter().map(BuildFile))
}

struct BuildFile<'a>(&'a bts_core::mocking::code::CodebaseFile);

impl serde::Serialize for BuildFile<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut file = serde::Serializer::serialize_struct(serializer, "BuildFile", 3)?;
        serde::ser::SerializeStruct::serialize_field(&mut file, "path", &self.0.path.to_string_lossy())?;
        serde::ser::SerializeStruct::serialize_field(&mut file, "contents", &self.0.contents)?;
        serde::ser::SerializeStruct::serialize_field(&mut file, "slugs", &self.0.slugs)?;
        serde::ser::SerializeStruct::end(file)
    }
}

/// Builds Braintrust scorer source and codebase files without filesystem access.
///
/// Supply the project name and any existing files as text keyed by relative path.
/// The optional language overrides authored choices. The root defaults to
/// `bts-scorers` and supplies the generated package name without selecting a directory.
/// The result contains planned files for the caller to inspect or save.
#[wasm_bindgen(unchecked_return_type = "BuildResult")]
pub fn build(
    source: &str,
    #[wasm_bindgen(unchecked_param_type = "BuildOptions")] build_options: JsValue,
) -> Result<JsValue, JsValue> {
    operation((|| {
        let build_options = crate::options::<BuildOptions>(build_options)?;
        if build_options.project.trim().is_empty() {
            return Err(OperationError::new("options", "project must not be empty"));
        }
        if build_options.root.trim().is_empty() {
            return Err(OperationError::new("options", "root must not be empty"));
        }
        for path in build_options.files.keys() {
            if path.is_empty()
                || !std::path::Path::new(path)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                return Err(OperationError::new(
                    "options",
                    format!("virtual file path `{path}` must be relative to the root"),
                ));
            }
        }
        let model = compile_source(source)?;
        if model.scorers.is_empty() {
            return Err(OperationError::new("build", "the shape declares no scorer blocks"));
        }
        let language = build_options.lang.map(|language| match language {
            BuildLanguage::Python => bts_lang::ScorerLang::Python,
            BuildLanguage::Typescript => bts_lang::ScorerLang::Typescript,
        });
        let target = bts_core::providers::braintrust::mocking::code::CodeTarget::new(build_options.project);
        let assembly = bts_core::mocking::code::assemble(
            &model.scorers,
            language,
            &bts_core::mocking::code::languages::Languages,
            &target,
        )
        .map_err(|error| OperationError::new("build", error))?;
        let existing = BuildExistingFiles(build_options.files);
        let context = bts_core::mocking::code::CodebaseContext {
            root: std::path::Path::new(&build_options.root),
            files: &existing,
        };
        let plan = bts_core::mocking::code::plan_codebase(&assembly, &target, &context)
            .map_err(|error| OperationError::new("build", error))?;
        Ok(BuildOutput { files: plan.files })
    })())
}

#[wasm_bindgen(typescript_custom_section)]
const BUILD_TYPESCRIPT: &str = r#"
export interface BuildOptions {
    project: string;
    lang?: "python" | "typescript";
    root?: string;
    files?: Record<string, string>;
}

export interface BuildOutput {
    files: Array<{ path: string; contents: string; slugs: string[] }>;
}

export type BuildResult = OperationResult<BuildOutput>;
"#;

#[wasm_bindgen(typescript_custom_section)]
const WRITE_TYPESCRIPT: &str = r#"
export interface WriteOptions {
    project_id: string;
    request_timeout_ms?: number;
    retry_attempts?: number;
    retry_max_elapsed_ms?: number;
}
export interface HttpRequest {
    method: string;
    path: string;
    headers: Record<string, string>;
    body: Uint8Array;
    timeout_ms: number;
    signal: AbortSignal;
}
export interface HttpResponse {
    status: number;
    headers?: Record<string, string>;
    body: string | Uint8Array;
}
/** Send once, honor the abort signal, and consume the response body before resolving. */
export type HttpTransport = (request: HttpRequest) => Promise<HttpResponse>;
export interface WriteOutput {
    provider: "braintrust";
    seed: string;
    trace_count: number;
    event_count: number;
    row_ids: string[];
}
export type WriteResult = OperationResult<WriteOutput>;
"#;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteOptions {
    project_id: String,
    request_timeout_ms: Option<f64>,
    retry_attempts: Option<u32>,
    retry_max_elapsed_ms: Option<f64>,
}

#[derive(Serialize)]
struct WriteOutput {
    provider: &'static str,
    seed: String,
    trace_count: usize,
    event_count: usize,
    row_ids: Vec<String>,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn monotonic_now() -> f64;
    #[wasm_bindgen(js_namespace = globalThis, js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, delay: i32) -> i32;
    #[wasm_bindgen(js_namespace = globalThis, js_name = clearTimeout)]
    fn clear_timeout(timer: i32);
    type AbortController;
    #[wasm_bindgen(constructor)]
    fn new() -> AbortController;
    #[wasm_bindgen(method, getter)]
    fn signal(this: &AbortController) -> JsValue;
    #[wasm_bindgen(method)]
    fn abort(this: &AbortController);
}

struct BrowserTransport(js_sys::Function);

fn transport_error(error: JsValue) -> bts_core::providers::braintrust::mocking::data::traces::TransportError {
    let message = error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&error, &"message".into())
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_else(|| "host transport failed".to_owned());
    let retryable = js_sys::Reflect::get(&error, &"retryable".into())
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    bts_core::providers::braintrust::mocking::data::traces::TransportError { message, retryable }
}

impl bts_core::providers::braintrust::mocking::data::traces::Transport for BrowserTransport {
    async fn send(
        &self,
        request: bts_core::providers::braintrust::mocking::data::traces::HttpRequest,
    ) -> Result<
        bts_core::providers::braintrust::mocking::data::traces::HttpResponse,
        bts_core::providers::braintrust::mocking::data::traces::TransportError,
    > {
        use wasm_bindgen_futures::JsFuture;
        let result = async {
            let controller = AbortController::new();
            let headers: std::collections::BTreeMap<_, _> = request.headers.into_iter().collect();
            let input = serialize(&serde_json::json!({
                "method": request.method, "path": request.path, "headers": headers,
                "timeout_ms": request.timeout.as_millis() as u64,
            }))?;
            js_sys::Reflect::set(&input, &"body".into(), &js_sys::Uint8Array::from(request.body.as_slice()))?;
            js_sys::Reflect::set(&input, &"signal".into(), &controller.signal())?;
            let pending = js_sys::Promise::resolve(&self.0.call1(&JsValue::UNDEFINED, &input)?);
            let mut callback = None;
            let mut timer = 0;
            let deadline = js_sys::Promise::new(&mut |_, reject| {
                let on_timeout = Closure::<dyn FnMut()>::new(move || {
                    let error = js_sys::Error::new("host transport request timed out");
                    let _ = js_sys::Reflect::set(&error, &"retryable".into(), &JsValue::TRUE);
                    let _ = reject.call1(&JsValue::UNDEFINED, &error);
                });
                timer = set_timeout(
                    on_timeout.as_ref().unchecked_ref(),
                    request.timeout.as_millis().min(i32::MAX as u128) as i32,
                );
                callback = Some(on_timeout);
            });
            let promises = js_sys::Array::new();
            promises.push(&pending);
            promises.push(&deadline);
            let response = JsFuture::from(js_sys::Promise::race(&promises)).await;
            clear_timeout(timer);
            controller.abort();
            drop(callback);
            let response = response?;
            let status = js_sys::Reflect::get(&response, &"status".into())?
                .as_f64()
                .filter(|status| status.fract() == 0.0 && (100.0..=599.0).contains(status))
                .ok_or_else(|| JsValue::from_str("host response status must be an HTTP status code"))?
                as u16;
            let headers = js_sys::Reflect::get(&response, &"headers".into())?;
            let headers: std::collections::BTreeMap<String, String> = if headers.is_undefined() {
                Default::default()
            } else {
                serde_wasm_bindgen::from_value(headers).map_err(|error| JsValue::from_str(&error.to_string()))?
            };
            let body = js_sys::Reflect::get(&response, &"body".into())?;
            let body = if let Some(text) = body.as_string() {
                text.into_bytes()
            } else if let Some(bytes) = body.dyn_ref::<js_sys::Uint8Array>() {
                bytes.to_vec()
            } else {
                return Err(JsValue::from_str("host response body must be a string or Uint8Array"));
            };
            Ok(bts_core::providers::braintrust::mocking::data::traces::HttpResponse {
                status,
                headers: headers.into_iter().collect(),
                body,
            })
        }
        .await;
        result.map_err(transport_error)
    }

    async fn sleep(&self, delay: std::time::Duration) {
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            set_timeout(&resolve, delay.as_millis().min(i32::MAX as u128) as i32);
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }

    fn now(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(monotonic_now() / 1000.0)
    }

    fn wall_time(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(js_sys::Date::now() as u64)
    }
}

/// Generates and delivers traces through a host-supplied asynchronous HTTP transport.
///
/// The host resolves provider paths, adds authentication, honors the abort signal,
/// and returns a fully consumed response body. Core handles retries and chunking.
/// Rejections may set `retryable: true` for connection failures or timeouts.
/// Keep credentials out of error messages. Failures include acknowledged row IDs
/// in `error.details`. Interrupted requests may have written additional rows.
#[wasm_bindgen(unchecked_return_type = "WriteResult")]
pub async fn write(
    source: &str,
    #[wasm_bindgen(unchecked_param_type = "GenerateOptions")] generation_options: JsValue,
    #[wasm_bindgen(unchecked_param_type = "WriteOptions")] write_options: JsValue,
    #[wasm_bindgen(unchecked_param_type = "HttpTransport")] transport: js_sys::Function,
) -> Result<JsValue, JsValue> {
    use bts_core::providers::braintrust::mocking::data::traces;
    let result = async {
        let options: WriteOptions = options(write_options)?;
        let project_id = uuid::Uuid::parse_str(&options.project_id).map_err(|error| OperationError::new("options", error))?;
        let mut limits = traces::WriteOptions::default();
        if let Some(timeout) = options.request_timeout_ms {
            limits.request_timeout = milliseconds(timeout, "request_timeout_ms")?;
            if timeout > i32::MAX as f64 {
                return Err(OperationError::new(
                    "options",
                    "request_timeout_ms exceeds the browser timer limit",
                ));
            }
        }
        if let Some(attempts) = options.retry_attempts {
            limits.retry_attempts = attempts;
        }
        if let Some(elapsed) = options.retry_max_elapsed_ms {
            limits.retry_max_elapsed = milliseconds(elapsed, "retry_max_elapsed_ms")?;
        }
        if limits.request_timeout.is_zero() || limits.retry_attempts == 0 || limits.retry_max_elapsed.is_zero() {
            return Err(OperationError::new(
                "options",
                "write timeouts and retry_attempts must be positive",
            ));
        }
        let generated = generate_batch(source, crate::options(generation_options)?)?;
        let receipt = traces::write_async(&BrowserTransport(transport), project_id, &generated.payload, &limits)
            .await
            .map_err(|error| {
                let mut result = OperationError::new("write", &error);
                result.details = Some(serde_json::json!({
                    "row_ids": error.acknowledged.row_ids(), "row_count": error.acknowledged.row_count(),
                }));
                result
            })?;
        Ok(WriteOutput {
            provider: generated.provider,
            seed: generated.seed,
            trace_count: generated.trace_count,
            event_count: generated.event_count,
            row_ids: receipt.row_ids().to_vec(),
        })
    }
    .await;
    operation(result)
}
