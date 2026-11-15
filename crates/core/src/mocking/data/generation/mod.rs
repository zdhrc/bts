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
    use crate::mocking::data::{UnsupportedAttachment, Value};
    use crate::providers::braintrust::mocking::compile;
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
            r#"(sample_root:task {name: "sample"}) {input: sample.input, output: self.input.first + self.input.second};
(sample:agent) {input: {first: range (1 , 1000000000), second: range (1 , 1000000000)}, output: sample_root.output};
(sample)-[:contains]->(sample_root);
(sample)-[:next]->(sample_root);
(dataset_cases_inline:case {name: "inline"}) {input: (sample_root).input, expected: (sample_root).output, metadata: {first: (sample_root).input.first}, tags: [|"sample-${(sample_root).input.second}"|]};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_inline)|]};"#,
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
            r#"(support_root:task {name: "support"}) {input: support.input, output: reply.output};
(answer:task {name: "answer"}) {input: support_root.input.request};
(reply:tool {name: "reply"}) {input: {query: answer.input}, output: {answer: "answer", request: self.input.query}};
(support:agent) {input: {request: "question"}, output: support_root.output};
(support)-[:contains]->(support_root);
(support)-[:contains]->(answer);
(answer)-[:child_of]->(support_root);
(support)-[:contains]->(reply);
(reply)-[:child_of]->(answer);
(support)-[:next]->(support_root)-[:next]->(answer)-[:next]->(reply);
(deferred_root:task {name: "deferred"}) {input: deferred.input, output: 1 / range (0 , 0)};
(deferred:agent) {input: "question", output: deferred_root.output};
(deferred)-[:contains]->(deferred_root);
(deferred)-[:next]->(deferred_root);
(dataset_cases_trace:case {name: "trace"}) {source: (support_root)};
(dataset_cases_span:case {name: "span"}) {source: (reply)};
(dataset_cases_trace_override:case {name: "trace-override"}) {source: (support_root), expected: "reviewed trace"};
(dataset_cases_span_override:case {name: "span-override"}) {source: (reply), expected: null};
(dataset_cases_unused_output:case {name: "unused-output"}) {source: (deferred_root), expected: "reviewed"};
(dataset_cases_invalid_default:case {name: "invalid-default"}) {source: (deferred_root)};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_trace), (dataset_cases_span), (dataset_cases_trace_override), (dataset_cases_span_override), (dataset_cases_unused_output), (dataset_cases_invalid_default)|]};"#,
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
            r#"(first_root:task {name: "first"}) {input: first.input, output: self.input.number * 2};
(first:agent) {input: {label: "first", number: range (1 , 1000000)}, output: first_root.output};
(first)-[:contains]->(first_root);
(first)-[:next]->(first_root);
(second_root:task {name: "second"}) {input: second.input, output: self.input.number * 2};
(second:agent) {input: {label: "second", number: range (1 , 1000000)}, output: second_root.output};
(second)-[:contains]->(second_root);
(second)-[:next]->(second_root);
(dataset_cases_group:case {name: "group"}) {traces: [|(second_root) , (first_root) , (second_root)|]};
(dataset_cases_override:case {name: "override"}) {traces: [|(second_root) , (first_root)|], expected: "reviewed group"};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_group), (dataset_cases_override)|]};"#,
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
            r#"(empty_root:task {name: "empty"}) {input: empty.input};
(empty_1:task {name: "empty"}) {};
(empty:agent) {input: null, output: null};
(empty)-[:contains]->(empty_root);
(empty)-[:contains]->(empty_1);
(empty_1)-[:child_of]->(empty_root);
(empty)-[:next]->(empty_root)-[:next]->(empty_1);
(answer_root:task {name: "answer"}) {output: "answer", input: answer.input};
(answer:agent) {input: null, output: answer_root.output};
(answer)-[:contains]->(answer_root);
(answer)-[:next]->(answer_root);
(dataset_cases_trace:case {name: "trace"}) {source: (empty_root)};
(dataset_cases_span:case {name: "span"}) {source: (empty_1)};
(dataset_cases_answer:case {name: "answer"}) {source: (answer_root)};
(dataset_cases_group:case {name: "group"}) {traces: [|(empty_root) , (answer_root)|]};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_trace), (dataset_cases_span), (dataset_cases_answer), (dataset_cases_group)|]};"#,
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
            r#"(document_root:task {name: "document"}) {input: document.input};
(document:agent) {input: attachment ("report.txt" , "text/plain"), output: null};
(document)-[:contains]->(document_root);
(document)-[:next]->(document_root);
(dataset_cases_inline:case {name: "inline"}) {input: {files: [|attachment ("report.txt" , "text/plain")|]}};
(dataset_cases_trace:case {name: "trace"}) {source: (document_root)};
(dataset_cases_projected:case {name: "projected"}) {input: (document_root).input.filename};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_inline), (dataset_cases_trace), (dataset_cases_projected)|]};"#,
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
        let model = crate::providers::braintrust::mocking::compile(
            r#"(support_root:task {name: "support"}) {input: support.input, source: (support_root)};
(lookup:task {name: "lookup"}) {output: "answer"};
(support:agent) {input: "question", output: null};
(support)-[:contains]->(support_root);
(support)-[:contains]->(lookup);
(lookup)-[:child_of]->(support_root);
(support)-[:next]->(support_root)-[:next]->(lookup);
(dataset_cases_whole:case {name: "whole"}) {source: support_root.source};
(dataset_cases_span:case {name: "span"}) {source: (lookup)};
(dataset_cases_fields:case {name: "fields"}) {input: support_root.source.input, expected: lookup.output};
(dataset_cases_group:case {name: "group"}) {traces: [|support_root.source , (support_root)|]};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_whole), (dataset_cases_span), (dataset_cases_fields), (dataset_cases_group)|]};"#,
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
        let model = crate::providers::braintrust::mocking::compile(
            r#"
(workflow:agent) {input: "outer", output: null};
(workflow)-[:contains]->(root);
(workflow)-[:contains]->(unused);
(workflow)-[:contains]->(selected);
(workflow)-[:contains]->(first);
(workflow)-[:contains]->(second);
(workflow)-[:next]->(root:task) {input: workflow.input};
(root)=[:repeat {count: range(-2, -1)}]=>[-[:next]->(unused:tool) {output: "unused"}];
(unused)-[:child_of]->(root);
(root)-[:next]->(selected:task {name: "answer"}) {
 input: "fallback", output: self.input == "use" ? details.output : "no dependency"
}-[:next]->(first:llm) {output: second.output}-[:next]->(second:llm) {output: "reply"};
(selected)-[:child_of]->(root);
(first)-[:child_of]->(selected);
(second)-[:child_of]->(selected);
(context_agent:agent) {input: null, output: null};
(context_agent)-[:contains]->(details);
(context_agent)-[:contains]->(context_unused);
(context_agent)-[:next]->(details:task) {input: context_agent.input, output: "support context"};
(details)=[:repeat {count: range(-2, -1)}]=>[-[:next]->(context_unused:tool) {output: "unused"}];
(context_unused)-[:child_of]->(details);

(one:case) {input: "question"};
(cases:dataset) {cases: [|(one)|]};
(s:scorer) {score: 1};
(e:experiment) {dataset: (cases), task: (selected), scorers: [|(s)|]};
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
(good:agent) {input: "fallback", output: selected.output};
(good)-[:contains]->(selected);
(good)-[:next]->(selected:task) {input: good.input, output: "ok"};
(bad:agent) {input: null, output: bad_root.output};
(bad)-[:contains]->(bad_root);
(bad)-[:next]->(bad_root:task) {input: bad.input, output: 1 / range(0, 0)};

(one:case) {input: "question"};
(cases:dataset) {cases: [|(one)|]};
(s:scorer) {score: 1};
(e:experiment) {dataset: (cases), task: (selected), scorers: [|(s)|]};
"#;
        let model = crate::providers::braintrust::mocking::compile(source).unwrap();
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
        let model =
            crate::providers::braintrust::mocking::compile(&source.replace("output: \"ok\"", "output: bad_root.output"))
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
        assert!(matches!(error, Error::Plan(error) if error.kind() == EvaluationErrorKind::DivisionByZero));
    }

    #[test]
    fn dataset_fields_drive_selected_subtree_and_preserve_original_scopes() {
        let model = crate::providers::braintrust::mocking::compile(
            r#"+(text_task:node) {input: {type: "string"}}-[:extends]->(task);
+(text_tool:node) {output: {type: "string"}}-[:extends]->(tool);
(workflow_root:task {name: "workflow"}) {input: workflow.input, context: "root variable"};
(context_node:text_tool {name: "context"}) {output: workflow_root.context};
(answer:text_task {name: "answer"}) {input: "task fallback", expected: "task expectation", metadata: {tier: "free", channel: "chat"}, output: "${self.input}/${trim(self.expected)}/${trim(self.metadata.tier)}/${context_node.output}"};
(reply:llm {name: "reply"}) {input: answer.input, output: answer.output};
(quality:scorer {name: "quality"}) {score: 0};
(workflow:agent) {input: "outer fallback", output: null};
(workflow)-[:contains]->(workflow_root);
(workflow)-[:contains]->(context_node);
(context_node)-[:child_of]->(workflow_root);
(workflow)-[:contains]->(answer);
(answer)-[:child_of]->(workflow_root);
(workflow)-[:contains]->(reply);
(reply)-[:child_of]->(answer);
(workflow)-[:contains]->(quality);
(quality)-[:child_of]->(answer);
(workflow)-[:next]->(workflow_root)-[:next]->(context_node)-[:next]->(answer)-[:next]->(reply)-[:next]->(quality);
(dataset_cases_one:case {name: "one"}) {input: "dataset input"};
(dataset_cases:dataset {name: "cases"}) {cases: [|(dataset_cases_one)|]};
(quality_function:scorer {name: "quality"}) {score: 1};
(experiment_e:experiment {name: "e"}) {dataset: (dataset_cases), task: (answer), scorers: [|(quality_function)|]};"#,
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
        let model = crate::providers::braintrust::mocking::compile(
            r#"
(t:agent) {input: "fallback", output: selected.output};
(t)-[:contains]->(selected);
(t)-[:contains]->(lookup);
(t)-[:next]->(selected:task) {input: t.input, output: self.input};
(selected)=[:repeat {count: 2}]=>[-[:next]->(lookup:tool) {input: selected.input, output: context.index}];
(lookup)-[:child_of]->(selected);

(one:case) {input: "question"};
(cases:dataset) {cases: [|(one)|]};
(s:scorer) {score: 1};
(e:experiment) {dataset: (cases), task: (selected), scorers: [|(s)|]};
"#,
        )
        .unwrap();
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
