mod evaluation;
mod materialization;

pub use evaluation::{Error as EvaluationError, ErrorKind as EvaluationErrorKind};
pub use materialization::{Distribution, Error as MaterializationError, ErrorKind as MaterializationErrorKind};

use super::{AttachmentFormat, EventBatch, Request};
use crate::dsl::Model;
use crate::selection::filter::WriteFilter;
use std::fmt;
use std::time::{Duration, SystemTime};

/// Generates a batch ready for preview or writing, without making network requests.
///
/// Resolves attachment paths relative to `request.source` without reading their
/// contents. Generation and path-resolution errors retain their concrete causes.
pub fn prepare(request: Request<'_>, format: &dyn AttachmentFormat) -> Result<EventBatch, PrepareError> {
    let mut events = tracing::info_span!("generate")
        .in_scope(|| {
            generate_filtered(
                request.model,
                request.count,
                request.over,
                request.distribution,
                request.until,
                request.seed,
                request.filter,
                format,
            )
        })
        .map_err(PrepareError::Generate)?;
    events
        .resolve_attachment_paths(request.source)
        .map_err(PrepareError::Attachments)?;
    Ok(events)
}

#[derive(Debug)]
pub enum PrepareError {
    Generate(Error),
    Attachments(std::io::Error),
}
impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generate(e) => e.fmt(f),
            Self::Attachments(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for PrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Generate(source) => Some(source),
            Self::Attachments(source) => Some(source),
        }
    }
}

/// Evaluates a compiled model and places its traces in the window ending at `now`.
///
/// The model must preserve compiler-established references, types, and scopes.
/// This does not resolve attachment paths, read files, or deliver events. Use
/// [`prepare`] when attachments need to be resolved relative to a source file.
pub fn generate(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
    format: &dyn AttachmentFormat,
) -> Result<EventBatch, Error> {
    generate_filtered(model, count, over, distribution, now, seed, &WriteFilter::default(), format)
}

/// Generates events from selected blocks while preserving their reference scopes.
///
/// Native and WebAssembly builds use the same seeded random generator. Floating-point
/// calculations can still differ slightly between platforms. Event IDs remain fresh
/// unless the caller applies [`EventBatch::assign_stable_ids`].
///
/// Excluding a block also excludes its descendants. Returns an error when the
/// model has no traces, the filter excludes every trace, evaluation fails, or
/// the generated events cannot fit the requested window. The same model
/// invariants and attachment-path behavior as [`generate`] apply.
#[allow(clippy::too_many_arguments)]
pub fn generate_filtered(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
    filter: &WriteFilter,
    format: &dyn AttachmentFormat,
) -> Result<EventBatch, Error> {
    if model.traces.is_empty() {
        return Err(Error::EmptyShape);
    }
    if !model.traces.iter().any(|trace| filter.matches("trace", Some(&trace.name))) {
        return Err(Error::NoMatchingTraces);
    }

    let plan = tracing::info_span!("plan")
        .in_scope(|| evaluation::plan_with_filter(model, count, seed, filter, format))
        .map_err(Error::Plan)?;
    tracing::info_span!("materialize")
        .in_scope(|| materialization::materialize(plan, over, distribution, now))
        .map_err(Error::Materialize)
}

/// Generates a validated experiment task using a dataset row as its root inputs.
///
/// `task` must be a static task reference from an experiment in this same compiled
/// model. The row supplies input, expected, metadata, and tags. Authored scorer
/// spans are excluded. This evaluates the task locally without invoking scorers
/// or resolving attachment paths.
pub fn experiment_result(
    model: &Model,
    task: crate::dsl::RefId,
    row: &serde_json::Value,
    index: usize,
    seed: u64,
    now: SystemTime,
    format: &dyn AttachmentFormat,
) -> Result<EventBatch, Error> {
    let plan = evaluation::plan_experiment(model, task, row, index, seed, format).map_err(Error::Plan)?;
    materialization::materialize(plan, Duration::from_secs(1), Distribution::Linear, now).map_err(Error::Materialize)
}

#[derive(Debug)]
pub enum Error {
    EmptyShape,
    NoMatchingTraces,
    Plan(EvaluationError),
    Materialize(MaterializationError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyShape => formatter.write_str("shape must contain at least one trace"),
            Self::NoMatchingTraces => formatter.write_str("write filter excludes every trace block"),
            Self::Plan(source) => write!(formatter, "failed to evaluate an expression: {source}"),
            Self::Materialize(source) => write!(formatter, "failed to materialize traces: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(source) => Some(source),
            Self::Materialize(source) => Some(source),
            _ => None,
        }
    }
}

/// Evaluates a compiled dataset case's authored fields without delivery.
///
/// The case must belong to `model`, with its resolved references intact. The
/// returned batch stores the case fields in its root input. Its timestamps use
/// the current clock, and its attachment paths have not been resolved.
pub fn case_data(
    model: &Model,
    case: &crate::dsl::DatasetCase,
    seed: u64,
    format: &dyn AttachmentFormat,
) -> Result<EventBatch, Error> {
    let plan = evaluation::plan_case_data(model, case_input(case), seed, format).map_err(Error::Plan)?;
    materialization::materialize(plan, Duration::from_secs(3600), Distribution::Linear, SystemTime::now())
        .map_err(Error::Materialize)
}

fn case_input(case: &crate::dsl::DatasetCase) -> crate::dsl::Value {
    use crate::dsl::{DatasetSource, Object, ObjectField, Value};
    let mut fields = Vec::new();
    if let DatasetSource::Inline(input) = &case.source {
        fields.push(ObjectField {
            key: "input".to_owned(),
            value: input.clone(),
        });
    }
    for (key, value) in [
        ("expected", &case.expected),
        ("metadata", &case.metadata),
        ("tags", &case.tags),
    ] {
        if let Some(value) = value {
            fields.push(ObjectField {
                key: key.to_owned(),
                value: value.clone(),
            });
        }
    }
    Value::Object(Object { elem: fields })
}

/// Snapshots an eval case without reading the clock or accessing external services.
///
/// Trace and span sources supply their input and, unless overridden, output as
/// expected. Groups preserve source order as arrays. References within one case
/// share an instance, so sampled inputs and expected values stay consistent.
pub fn case_snapshot(model: &Model, case: &crate::dsl::DatasetCase, seed: u64) -> Result<super::Value, Error> {
    use crate::dsl::{Accessor, Array, ArrayElem, Child, DatasetSource, Field, NodeId, RefId, SpanFields, Value};

    fn child_fields(children: &[Child], node: NodeId) -> Option<&SpanFields> {
        children.iter().find_map(|child| match child {
            Child::Span(span) if span.node == node => Some(&span.fields),
            Child::Span(span) => child_fields(&span.children, node),
            Child::Repeat(block) => child_fields(&block.children, node),
            Child::Choice(block) => child_fields(&block.children, node),
            Child::Maybe(block) => child_fields(&block.children, node),
            Child::Scorer(_) => None,
        })
    }

    fn field(model: &mut Model, id: RefId, field: Field) -> Option<Value> {
        let mut reference = model.refs[id.0 as usize].clone();
        let Accessor::Block { node, .. } = reference.accessor else {
            return None;
        };
        let fields = model.traces.iter().find_map(|trace| {
            if trace.node == node {
                Some(&trace.fields)
            } else {
                child_fields(&trace.children, node)
            }
        })?;
        match field {
            Field::Input => fields.input.as_ref()?,
            Field::Output => fields.output.as_ref()?,
            _ => unreachable!("snapshots only read source input and output"),
        };
        reference.accessor = Accessor::Field(field);
        let range = reference.range;
        let ref_id = RefId(model.refs.len() as u32);
        model.refs.push(reference);
        Some(Value::BlockRef { ref_id, range })
    }

    let mut model = model.clone();
    let mut case = case.clone();
    match &case.source {
        DatasetSource::Inline(_) => {}
        DatasetSource::Trace(id) | DatasetSource::Span(id) => {
            let input = field(&mut model, *id, Field::Input).unwrap_or(Value::Null);
            if case.expected.is_none() {
                case.expected = field(&mut model, *id, Field::Output);
            }
            case.source = DatasetSource::Inline(input);
        }
        DatasetSource::Group(ids) => {
            let inputs = ids
                .iter()
                .map(|id| ArrayElem::Item(field(&mut model, *id, Field::Input).unwrap_or(Value::Null)))
                .collect();
            if case.expected.is_none() {
                let outputs = ids
                    .iter()
                    .map(|id| ArrayElem::Item(field(&mut model, *id, Field::Output).unwrap_or(Value::Null)))
                    .collect();
                case.expected = Some(Value::Array(Array { elem: outputs }));
            }
            case.source = DatasetSource::Inline(Value::Array(Array { elem: inputs }));
        }
    }
    let plan = evaluation::plan_case_data(&model, case_input(&case), seed, &()).map_err(Error::Plan)?;
    Ok(plan
        .events
        .into_vec()
        .remove(0)
        .fields
        .input
        .expect("case input is always present"))
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::data::{UnsupportedAttachment, Value};
    use serde_json::{Value as JsonValue, json};

    fn snapshot(model: &Model, case: usize, seed: u64) -> JsonValue {
        case_snapshot(model, &model.datasets[0].cases[case], seed)
            .unwrap()
            .project(&())
            .unwrap()
    }

    #[test]
    fn inline_references_share_samples_and_respect_the_seed() {
        let model = compile(
            r#"trace "sample" {
                input = { first = range(1, 1000000000), second = range(1, 1000000000) }
                output = self.input.first + self.input.second
            }
            dataset "cases" {
                case "inline" {
                    input = trace.sample.input
                    expected = trace.sample.output
                    metadata = { first = trace.sample.input.first }
                    tags = ["sample-${trace.sample.input.second}"]
                }
            }"#,
        )
        .unwrap();
        let row = snapshot(&model, 0, 42);
        let first = row["input"]["first"].as_i64().unwrap();
        let second = row["input"]["second"].as_i64().unwrap();
        assert_eq!(row["expected"], first + second);
        assert_eq!(row["metadata"]["first"], first);
        assert_eq!(row["tags"], json!([format!("sample-{second}")]));
        assert_eq!(row, snapshot(&model, 0, 42));
        assert_ne!(row["input"], snapshot(&model, 0, 43)["input"]);
    }

    #[test]
    fn trace_and_nested_span_sources_default_to_output_and_allow_expected_overrides() {
        let model = compile(
            r#"trace "support" {
                input = { request = "question" }
                output = task.answer.tool.reply.output
                task "answer" {
                    input = trace.input.request
                    tool "reply" {
                        input = { query = task.answer.input }
                        output = { answer = "answer", request = self.input.query }
                    }
                }
            }
            trace "deferred" { input = "question" output = 1 / range(0, 0) }
            dataset "cases" {
                case "trace" { trace = trace.support }
                case "span" { span = trace.support.task.answer.tool.reply }
                case "trace-override" { trace = trace.support expected = "reviewed trace" }
                case "span-override" { span = trace.support.task.answer.tool.reply expected = null }
                case "unused-output" { trace = trace.deferred expected = "reviewed" }
                case "invalid-default" { trace = trace.deferred }
            }"#,
        )
        .unwrap();
        let expected = json!({"answer": "answer", "request": "question"});
        assert_eq!(
            snapshot(&model, 0, 42),
            json!({"input": {"request": "question"}, "expected": expected})
        );
        assert_eq!(
            snapshot(&model, 1, 42),
            json!({"input": {"query": "question"}, "expected": expected})
        );
        assert_eq!(snapshot(&model, 2, 42)["expected"], "reviewed trace");
        assert_eq!(
            snapshot(&model, 3, 42),
            json!({"input": {"query": "question"}, "expected": null})
        );
        assert_eq!(snapshot(&model, 4, 42), json!({"input": "question", "expected": "reviewed"}));
        let error = case_snapshot(&model, &model.datasets[0].cases[5], 42).unwrap_err();
        assert!(matches!(error, Error::Plan(error) if error.kind() == EvaluationErrorKind::DivisionByZero));
    }

    #[test]
    fn grouped_sources_preserve_order_and_reuse_repeated_trace_values() {
        let model = compile(
            r#"trace "first" { input = { label = "first", number = range(1, 1000000) } output = self.input.number * 2 }
               trace "second" { input = { label = "second", number = range(1, 1000000) } output = self.input.number * 2 }
               dataset "cases" {
                   case "group" { traces = [trace.second, trace.first, trace.second] }
                   case "override" { traces = [trace.second, trace.first] expected = "reviewed group" }
               }"#,
        )
        .unwrap();
        let row = snapshot(&model, 0, 17);
        let inputs = row["input"].as_array().unwrap();
        let expected = row["expected"].as_array().unwrap();
        assert_eq!(
            inputs
                .iter()
                .map(|value| value["label"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["second", "first", "second"]
        );
        assert_eq!(inputs[0], inputs[2]);
        assert_eq!(expected[0], expected[2]);
        for (input, output) in inputs.iter().zip(expected) {
            assert_eq!(*output, input["number"].as_i64().unwrap() * 2);
        }
        assert_eq!(snapshot(&model, 1, 17)["expected"], "reviewed group");
    }

    #[test]
    fn missing_source_fields_preserve_null_input_and_absent_expected() {
        let model = compile(
            r#"trace "empty" { task "empty" {} }
               trace "answer" { output = "answer" }
               dataset "cases" {
                   case "trace" { trace = trace.empty }
                   case "span" { span = trace.empty.task.empty }
                   case "answer" { trace = trace.answer }
                   case "group" { traces = [trace.empty, trace.answer] }
               }"#,
        )
        .unwrap();
        assert_eq!(snapshot(&model, 0, 42), json!({"input": null}));
        assert_eq!(snapshot(&model, 1, 42), json!({"input": null}));
        assert_eq!(snapshot(&model, 2, 42), json!({"input": null, "expected": "answer"}));
        assert_eq!(
            snapshot(&model, 3, 42),
            json!({"input": [null, null], "expected": [null, "answer"]})
        );
    }

    #[test]
    fn attachments_stay_typed_and_fail_without_a_projection() {
        let model = compile(
            r#"trace "document" { input = attachment("report.txt", "text/plain") }
               dataset "cases" {
                   case "inline" { input = { files = [attachment("report.txt", "text/plain")] } }
                   case "trace" { trace = trace.document }
                   case "projected" { input = trace.document.input.filename }
               }"#,
        )
        .unwrap();
        let inline = case_snapshot(&model, &model.datasets[0].cases[0], 42).unwrap();
        assert!(matches!(inline["input"]["files"][0], Value::Attachment(_)));
        assert_eq!(inline.project(&()).unwrap_err(), UnsupportedAttachment);
        let trace = case_snapshot(&model, &model.datasets[0].cases[1], 42).unwrap();
        assert!(matches!(trace["input"], Value::Attachment(_)));
        assert_eq!(trace.project(&()).unwrap_err(), UnsupportedAttachment);
        let error = case_snapshot(&model, &model.datasets[0].cases[2], 42).unwrap_err();
        assert!(matches!(error, Error::Plan(error) if error.kind() == EvaluationErrorKind::UnsupportedAttachment));
    }
}

#[cfg(test)]
mod experiment_tests {
    use super::*;
    use crate::mocking::data::Value;
    use serde_json::json;

    fn text(value: &Option<Value>) -> Option<&str> {
        value.as_ref()?.as_str()
    }

    #[test]
    fn materializes_fields_from_a_resolved_case_source() {
        let model = crate::dsl::semantics::compile(
            r#"
            vars { source = trace["support"] }
            dataset "cases" {
                case "whole" { trace = var.source }
                case "span" { span = var.source.task["lookup"] }
                case "fields" { input = var.source.input expected = var.source.task["lookup"].output }
                case "group" { traces = [var.source, trace["support"]] }
            }
            trace "support" { input = "question" task "lookup" { output = "answer" } }
        "#,
        )
        .unwrap();
        let batch = case_data(&model, &model.datasets[0].cases[2], 3, &()).unwrap();
        assert_eq!(
            batch.events[0].input,
            Some(json!({"input":"question","expected":"answer"}).into())
        );
    }

    #[test]
    fn selected_task_evaluates_only_used_dependencies() {
        let model = crate::dsl::semantics::compile(
            r#"
            trace "workflow" {
                input = "outer"
                repeat "unused" { count = range(-2, -1) tool "unused" { output = "unused" } }
                task "answer" {
                    input = "fallback"
                    output = self.input == "use" ? trace.context.task.details.output : "no dependency"
                    llm "first" { output = llm.second.output }
                    llm "second" { output = "reply" }
                }
            }
            trace "context" {
                task "details" { output = "support context" }
                repeat "unused" { count = range(-2, -1) tool "unused" { output = "unused" } }
            }
            trace "unrelated" { repeat "unused" { count = range(-2, -1) tool "unused" { output = "unused" } } }
            dataset "cases" { case "one" { input = "use" } }
            scorer "s" { code { score = 1 } }
            experiment "e" { dataset = dataset.cases task = trace.workflow.task.answer scorers = [scorer.s] }
        "#,
        )
        .unwrap();
        for (input, expected) in [("use", "support context"), ("skip", "no dependency")] {
            let batch = experiment_result(
                &model,
                model.experiments[0].task,
                &json!({"input":input}),
                0,
                42,
                SystemTime::now(),
                &(),
            )
            .unwrap();
            let events = &batch.events;
            assert_eq!(events.len(), 3);
            assert_eq!(text(&events[0].output), Some(expected));
            assert_eq!(events[1].name, "first");
            assert_eq!(events[2].name, "second");
            assert_eq!(events[1].output, events[2].output);
        }
    }

    #[test]
    fn selected_trace_skips_unrelated_errors_but_reports_used_dependency_errors() {
        let source = r#"
            trace "good" { input = "fallback" output = "ok" }
            trace "bad" { repeat "steps" { count = range(-2, -1) tool "lookup" { output = "unused" } } }
            dataset "cases" { case "one" { input = "question" } }
            scorer "s" { code { score = 1 } }
            experiment "e" { dataset = dataset.cases task = trace.good scorers = [scorer.s] }
        "#;
        let model = crate::dsl::semantics::compile(source).unwrap();
        assert!(
            experiment_result(
                &model,
                model.experiments[0].task,
                &json!({"input":"question"}),
                0,
                42,
                SystemTime::now(),
                &(),
            )
            .is_ok()
        );
        let model = crate::dsl::semantics::compile(
            &source.replace("output = \"ok\"", "output = trace.bad.repeat.steps[0].tool.lookup.output"),
        )
        .unwrap();
        let error = experiment_result(
            &model,
            model.experiments[0].task,
            &json!({"input":"question"}),
            0,
            42,
            SystemTime::now(),
            &(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("repeat count is negative"));
    }

    #[test]
    fn dataset_fields_drive_selected_subtree_and_preserve_original_scopes() {
        let model = crate::dsl::semantics::compile(
            r#"
            vars { context = "root variable" }
            trace "workflow" {
                input = "outer fallback"
                tool "context" { output = var.context }
                task "answer" {
                    input = "task fallback"
                    expected = "task expectation"
                    metadata = { tier = "free", channel = "chat" }
                    output = "${self.input}/${self.expected}/${self.metadata.tier}/${tool.context.output}"
                    llm "reply" { input = task.answer.input output = task.answer.output }
                    scorer "quality" { score = 0 }
                }
            }
            dataset "cases" { case "one" { input = "dataset input" } }
            scorer "quality" { code { score = 1 } }
            experiment "e" { dataset = dataset.cases task = trace.workflow.task.answer scorers = [scorer.quality] }
        "#,
        )
        .unwrap();
        let row = json!({"input":"dataset input", "expected":"dataset expectation", "metadata":{"tier":"enterprise"}, "tags":["regression"]});
        let batch = experiment_result(&model, model.experiments[0].task, &row, 0, 42, SystemTime::now(), &()).unwrap();
        let events = &batch.events;
        assert_eq!(events.len(), 2);
        assert_eq!(text(&events[0].input), Some("dataset input"));
        assert_eq!(
            text(&events[0].output),
            Some("dataset input/dataset expectation/enterprise/root variable")
        );
        assert_eq!(events[0].metadata.as_ref().unwrap()["channel"].as_str(), Some("chat"));
        assert_eq!(events[0].tags.as_ref(), ["regression"]);
        assert_eq!(text(&events[1].input), Some("dataset input"));
        assert_eq!(events[1].output, events[0].output);
        assert!(events[0].span_parents.is_empty());
        assert_eq!(events[1].span_parents.as_ref(), std::slice::from_ref(&events[0].span_id));
        assert!(events.iter().all(|event| event.scores.is_none()));
    }

    #[test]
    fn whole_trace_generation_uses_dataset_input_and_dynamic_children() {
        let model = crate::dsl::semantics::compile(r#"
            trace "t" { input = "fallback" output = trace.input repeat "steps" { count = 2 tool "lookup" { input = trace.input output = repeat.index } } }
            dataset "cases" { case "one" { input = "question" } }
            scorer "s" { code { score = 1 } }
            experiment "e" { dataset = dataset.cases task = trace.t scorers = [scorer.s] }
        "#).unwrap();
        let batch = experiment_result(
            &model,
            model.experiments[0].task,
            &json!({"input":"question"}),
            0,
            1,
            SystemTime::now(),
            &(),
        )
        .unwrap();
        assert_eq!(batch.events.len(), 3);
        assert_eq!(text(&batch.events[0].output), Some("question"));
        assert_eq!(text(&batch.events[1].input), Some("question"));
        assert_eq!(text(&batch.events[2].input), Some("question"));
        assert_eq!(batch.events[0].expected, Some(Value::Null));
    }
}
