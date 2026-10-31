use bts_lang::{
    Accessor, AutomationKind, Child, DatasetSource, DiagPhase, Field, ScorerKind, ScorerLang, SpanKind, Step, Value,
    semantics::compile, syntax::parse_traversal,
};

#[test]
fn compiles_a_trace_with_dynamic_and_resolved_references() {
    let model = compile(include_str!("fixtures/compilation.bt")).unwrap();
    let [trace] = model.traces.as_slice() else {
        panic!("expected one trace");
    };
    assert_eq!(trace.name, "support-sessions");
    assert_eq!(trace.bindings.len(), 1);
    assert!(matches!(&trace.bindings[0].value, Value::Func { .. }));
    let [Child::Repeat(repeat), Child::Choice(choice), _] = trace.children.as_slice() else {
        panic!("expected the repeated turns and route choice");
    };
    let Child::Span(turn) = &repeat.children[0] else {
        panic!("expected turn task");
    };
    let Child::Span(summary) = &choice.children[1] else {
        panic!("expected summary function");
    };
    let Some(Value::Object(input)) = &summary.fields.input else {
        panic!("expected summary input");
    };
    let last_answer = &input.elem.iter().find(|field| field.key == "last_answer").unwrap().value;
    let Value::BlockRef { ref_id, range } = last_answer else {
        panic!("expected a resolved reference");
    };
    let resolved = &model.refs[ref_id.0 as usize];
    assert_eq!(resolved.module, None);
    assert_eq!(resolved.up, 2);
    assert!(matches!(resolved.accessor, Accessor::Field(Field::Output)));
    assert!(resolved.path.is_empty());
    let [
        Step::Child {
            candidates: repeats,
            position: None,
        },
        Step::Iteration(index),
        Step::Child {
            candidates: turns,
            position: None,
        },
    ] = resolved.steps.as_slice()
    else {
        panic!("expected a repeat, one iteration, and its task");
    };
    assert_eq!(repeats, &[repeat.node]);
    assert_eq!(turns, &[turn.node]);
    assert!(matches!(index, Value::Binary { op: bts_lang::BinOp::Sub, lhs, rhs, .. }
        if matches!(lhs.as_ref(), Value::VarRef(name) if name == "turns")
        && matches!(rhs.as_ref(), Value::Num(bts_lang::Number::Int(1)))));
    assert_eq!(
        &include_str!("fixtures/compilation.bt")[range.start..range.end],
        "repeat.turns[var.turns - 1].task.turn.output"
    );
}

#[test]
fn distinguishes_parent_and_self_reference_scopes() {
    let model = compile(
        r#"trace "run" {
        input = "question"
        task "answer" { input = trace.input output = self.input }
    }"#,
    )
    .unwrap();
    let Child::Span(task) = &model.traces[0].children[0] else {
        panic!("expected task");
    };
    for (field, up) in [(&task.fields.input, 1), (&task.fields.output, 0)] {
        let Some(Value::BlockRef { ref_id, .. }) = field else {
            panic!("expected reference");
        };
        let reference = &model.refs[ref_id.0 as usize];
        assert_eq!(reference.up, up);
        assert_eq!(reference.module, None);
        assert!(reference.steps.is_empty());
        assert!(matches!(reference.accessor, Accessor::Field(Field::Input)));
    }
}

#[test]
fn separates_syntax_checks_from_semantic_checks() {
    for source in [
        r#"trace "run" { output = task.missing.output }"#,
        r#"trace "run" { metadata = true }"#,
        r#"unknown "block" {}"#,
    ] {
        assert!(bts_lang::syntax::parse(source).is_ok(), "{source}");
        let errors = compile(source).unwrap_err();
        assert!(errors.iter().all(|error| error.when == DiagPhase::Validation), "{errors:?}");
    }
    let errors = bts_lang::syntax::parse(r#"trace "run" { input = }"#).unwrap_err();
    assert!(errors.iter().any(|error| error.when == DiagPhase::Parsing));
    assert!(bts_lang::syntax::parse("").unwrap().decls.is_empty());
    assert!(compile("").is_err());
    assert!(bts_lang::semantics::compile_module("", true).unwrap().traces.is_empty());
}

#[test]
fn keeps_dynamic_dataset_source_restrictions_in_validation() {
    let source = r#"
        trace "run" { repeat "turns" { count = 2 task "answer" {} } }
        dataset "cases" { case "answer" { span = trace.run.repeat.turns[0].task.answer } }
    "#;
    assert!(bts_lang::syntax::parse(source).is_ok());
    let errors = compile(source).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.when == DiagPhase::Validation && error.what.contains("static path")),
        "{errors:?}"
    );
}

#[test]
fn compiles_dynamic_blocks_for_a_consumer_to_inspect() {
    let model = compile(include_str!("fixtures/compilation.bt")).unwrap();
    let [Child::Repeat(repeat), Child::Choice(choice), Child::Maybe(maybe)] = model.traces[0].children.as_slice() else {
        panic!("expected repeat, choice, and maybe blocks");
    };
    assert_eq!(repeat.name.as_deref(), Some("turns"));
    assert!(matches!(&repeat.count, Value::VarRef(name) if name == "turns"));
    assert!(matches!(repeat.children.as_slice(), [Child::Span(task)] if matches!(task.kind, SpanKind::Task)));
    assert_eq!(choice.children.len(), 2);
    assert_eq!(maybe.name.as_deref(), Some("escalation"));
    assert!(matches!(maybe.children.as_slice(), [Child::Span(task)] if matches!(task.kind, SpanKind::Task)));
}

#[test]
fn compiles_related_dataset_scorer_and_experiment_definitions() {
    let model = compile(include_str!("fixtures/compilation.bt")).unwrap();

    assert_eq!(model.datasets[0].name, "support-cases");
    assert!(matches!(model.datasets[0].cases[0].source, DatasetSource::Inline(_)));
    assert!(matches!(model.datasets[0].cases[1].source, DatasetSource::Trace(_)));
    assert_eq!(model.scorers[0].name, "status-match");
    assert_eq!(model.scorers[0].lang, Some(ScorerLang::Python));
    assert!(matches!(model.scorers[0].kind, ScorerKind::Code { .. }));
    let experiment = &model.experiments[0];
    assert_eq!(experiment.name, "support-regression");
    assert_eq!(experiment.dataset, "support-cases");
    assert_eq!(experiment.scorers, ["status-match"]);
    let resolved = &model.refs[experiment.task.0 as usize];
    assert_eq!(resolved.module, Some(model.traces[0].node));
    assert!(resolved.steps.is_empty());
    assert_eq!(model.facets[0].name, "Escalation");
    assert_eq!(model.automations[0].name, "escalation-topics");
    assert!(matches!(&model.automations[0].kind, AutomationKind::Topics { facets } if facets == &["Escalation"]));
    assert_eq!(model.automations[0].sampling_rate, 0.5);
}

#[test]
fn reports_source_locations_for_invalid_source() {
    let source = "trace \"broken\" {\n    input = }\n}";
    let errors = compile(source).unwrap_err();
    let error = errors.iter().find(|error| error.when == DiagPhase::Parsing).unwrap();
    let rendered = error.render("broken.bt", source);

    assert_eq!(&source[error.r#where.start..error.r#where.end], "}");
    assert!(rendered.contains("broken.bt:2:13: parsing error:"));
    assert!(rendered.contains("2 |     input = }"));
    assert!(rendered.contains("^ (byte 29)"));
}

#[test]
fn rejects_a_reference_to_a_missing_span() {
    let source = "trace \"run\" { output = task.missing.output }";
    let errors = compile(source).unwrap_err();

    assert!(errors.iter().any(|error| error.when == DiagPhase::Validation));
    assert!(errors.iter().any(|error| error.what.contains("missing")));
}

#[test]
fn parses_public_traversal_paths() {
    assert_eq!(
        parse_traversal("trace[\"run\"].task[\"answer\"].output").unwrap(),
        ["trace", "run", "task", "answer", "output"]
    );
    assert!(parse_traversal("trace[0].output").is_err());
}

#[test]
fn consumers_can_name_nested_model_types() {
    use bts_lang::{Facet, Func, JudgeOption, ScorerSpan, ScorerStep, Span, WeightedOption, When};
    let model = compile(
        r#"
        trace "run" {
            input = weighted(["question", 2], ["followup", 1])
            task "answer" {}
            scorer "quality" { score = 1 }
        }
        scorer "quality" { code { score = 1 when { cond = output == null score = 0 } } }
        scorer "judge" { judge { model = "gpt-4o-mini" prompt = "Evaluate ${output}." options = { good = 1, bad = 0 } } }
        facet "Topic" { prompt = "Identify the topic." }
    "#,
    )
    .unwrap();
    let [Child::Span(span), Child::Scorer(score)] = model.traces[0].children.as_slice() else {
        panic!("expected span and score");
    };
    let span: &Span = span;
    let score: &ScorerSpan = score;
    assert_eq!(span.name, "answer");
    assert_eq!(score.name, "quality");
    let facet: &Facet = &model.facets[0];
    assert_eq!(facet.prompt, "Identify the topic.");
    let Some(Value::Func {
        func: Func::Weighted(options),
        ..
    }) = &model.traces[0].fields.input
    else {
        panic!("expected weighted input");
    };
    let option: &WeightedOption = &options[0];
    assert_eq!(option.weight, 2.0);
    let ScorerKind::Code { steps, .. } = &model.scorers[0].kind else {
        panic!("expected code scorer");
    };
    let ScorerStep::When(when) = &steps[0] else {
        panic!("expected condition");
    };
    let when: &When = when;
    assert!(when.score.is_some());
    let ScorerKind::Judge { options, .. } = &model.scorers[1].kind else {
        panic!("expected judge");
    };
    let option: &JudgeOption = &options[0];
    assert_eq!(option.label, "good");
}
