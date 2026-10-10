pub(crate) mod materializer;
pub(crate) mod planner;

use crate::dsl::{Model, WriteFilter};
use std::fmt;
use std::time::{Duration, SystemTime};

pub(crate) use materializer::stable_uuid;
pub(crate) use materializer::{Distribution, EventBatch};
pub(crate) use planner::Attachment;

pub(crate) fn generate(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
) -> Result<EventBatch, Error> {
    generate_filtered(model, count, over, distribution, now, seed, &WriteFilter::default())
}

pub(crate) fn generate_filtered(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
    filter: &WriteFilter,
) -> Result<EventBatch, Error> {
    if model.traces.is_empty() {
        return Err(Error::EmptyShape);
    }
    if !model.traces.iter().any(|trace| filter.matches("trace", Some(&trace.name))) {
        return Err(Error::NoMatchingTraces);
    }

    let plan = tracing::info_span!("plan")
        .in_scope(|| planner::plan_with_filter(model, count, seed, filter))
        .map_err(Error::Plan)?;
    tracing::info_span!("materialize")
        .in_scope(|| materializer::materialize(plan, over, distribution, now))
        .map_err(Error::Materialize)
}

pub(crate) fn experiment_result(
    model: &Model,
    task: crate::dsl::RefId,
    row: &serde_json::Value,
    index: usize,
    seed: u64,
    now: SystemTime,
) -> Result<EventBatch, Error> {
    let plan = planner::plan_experiment(model, task, row, index, seed).map_err(Error::Plan)?;
    materializer::materialize(plan, Duration::from_secs(1), Distribution::Linear, now).map_err(Error::Materialize)
}

#[derive(Debug)]
pub(crate) enum Error {
    EmptyShape,
    NoMatchingTraces,
    Plan(planner::Error),
    Materialize(materializer::Error),
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

impl std::error::Error for Error {}

pub(crate) fn case_data(model: &Model, case: &crate::dsl::DatasetCase, seed: u64) -> Result<EventBatch, Error> {
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
    let plan = planner::plan_case_data(model, Value::Object(Object { elem: fields }), seed).map_err(Error::Plan)?;
    materializer::materialize(plan, Duration::from_secs(3600), Distribution::Linear, SystemTime::now())
        .map_err(Error::Materialize)
}

#[cfg(test)]
mod experiment_tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn selected_task_evaluates_only_used_dependencies() {
        let model = crate::dsl::compile(
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
            )
            .unwrap();
            let payload = serde_json::to_value(batch).unwrap();
            let events = payload["events"].as_array().unwrap();
            assert_eq!(events.len(), 3);
            assert_eq!(events[0]["output"], expected);
            assert_eq!(events[1]["span_attributes"]["name"], "first");
            assert_eq!(events[2]["span_attributes"]["name"], "second");
            assert_eq!(events[1]["output"], events[2]["output"]);
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
        let model = crate::dsl::compile(source).unwrap();
        assert!(
            experiment_result(
                &model,
                model.experiments[0].task,
                &json!({"input":"question"}),
                0,
                42,
                SystemTime::now()
            )
            .is_ok()
        );
        let model =
            crate::dsl::compile(&source.replace("output = \"ok\"", "output = trace.bad.repeat.steps[0].tool.lookup.output"))
                .unwrap();
        let error = experiment_result(
            &model,
            model.experiments[0].task,
            &json!({"input":"question"}),
            0,
            42,
            SystemTime::now(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("repeat count is negative"));
    }

    #[test]
    fn dataset_fields_drive_selected_subtree_and_preserve_original_scopes() {
        let model = crate::dsl::compile(
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
        let batch = experiment_result(&model, model.experiments[0].task, &row, 0, 42, SystemTime::now()).unwrap();
        let payload = serde_json::to_value(batch).unwrap();
        let events = payload["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["input"], "dataset input");
        assert_eq!(
            events[0]["output"],
            "dataset input/dataset expectation/enterprise/root variable"
        );
        assert_eq!(events[0]["metadata"]["channel"], "chat");
        assert_eq!(events[0]["tags"], json!(["regression"]));
        assert_eq!(events[1]["input"], "dataset input");
        assert_eq!(events[1]["output"], events[0]["output"]);
        assert_eq!(events[0]["span_parents"], json!([]));
        assert_eq!(events[1]["span_parents"], json!([events[0]["span_id"]]));
        assert!(events.iter().all(|event| event.get("scores").is_none()));
    }

    #[test]
    fn whole_trace_generation_uses_dataset_input_and_dynamic_children() {
        let model = crate::dsl::compile(r#"
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
        )
        .unwrap();
        let payload = serde_json::to_value(batch).unwrap();
        assert_eq!(payload["events"].as_array().unwrap().len(), 3);
        assert_eq!(payload["events"][0]["output"], "question");
        assert_eq!(payload["events"][1]["input"], "question");
        assert_eq!(payload["events"][2]["input"], "question");
        assert_eq!(payload["events"][0]["expected"], Value::Null);
    }
}
