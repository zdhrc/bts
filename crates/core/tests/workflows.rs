use bts_core::{
    mocking::data::request::{self, Volume, Window},
    providers::braintrust::features::experiments,
};
use std::{
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

#[test]
fn previews_experiments_through_the_public_api() {
    let model = bts_lang::semantics::compile(
        r#"
        trace "answer" { input = "fallback" output = trace.input }
        dataset "cases" { case "question" { input = "hello" } }
        scorer "quality" { code { score = input == output ? 1 : 0 } }
        experiment "trial" { dataset = dataset.cases task = trace.answer scorers = [scorer.quality] }
    "#,
    )
    .unwrap();
    let preview = experiments::preview(&model, &[], Path::new("shape.bt"), 42, UNIX_EPOCH + Duration::from_secs(3600)).unwrap();
    assert_eq!(preview["scorers_executed"], false);
    assert_eq!(preview["experiments"][0]["name"], "trial");
    assert_eq!(preview["experiments"][0]["results"][0]["events"][0]["output"], "hello");
}

#[test]
fn resolves_generation_volume_without_cli_arguments() {
    let now = UNIX_EPOCH + Duration::from_secs(7200);
    let (count, over, until) = request::resolve(
        Window::Relative {
            over: Duration::from_secs(3600),
            offset: Duration::from_secs(600),
        },
        Volume::Rate(0.5),
        now,
    )
    .unwrap();
    assert_eq!(count, 1800);
    assert_eq!(over, Duration::from_secs(3600));
    assert_eq!(until, now - Duration::from_secs(600));
    assert!(
        request::resolve(
            Window::Relative {
                over,
                offset: Duration::ZERO
            },
            Volume::Rate(f64::NAN),
            now
        )
        .is_err()
    );
}

#[test]
fn prepares_preview_attachments_without_reading_or_uploading_files() {
    use bts_core::{mocking::data, selection::filter::WriteFilter};
    let root = std::env::temp_dir().join(format!("bts-preview-{}", uuid::Uuid::new_v4()));
    let source = root.join("shape.bt");
    let model = bts_lang::semantics::compile(
        r#"
        trace "review" {
            input = {
                first = attachment("report.pdf", "application/pdf")
                second = attachment("report.pdf", "application/pdf")
            }
        }
    "#,
    )
    .unwrap();
    let batch = data::prepare(
        data::Request {
            model,
            count: 2,
            over: Duration::from_secs(60),
            distribution: data::Distribution::Linear,
            until: UNIX_EPOCH + Duration::from_secs(3600),
            seed: 42,
            filter: &WriteFilter::default(),
            source: &source,
        },
        &(),
    )
    .unwrap();
    assert_eq!(batch.trace_count(), 2);
    assert_eq!(batch.attachments.len(), 1);
    assert_eq!(Path::new(&batch.attachments[0].path), root.join("report.pdf"));
    assert!(!root.exists());
}

#[test]
fn renders_a_language_skill_without_an_interface_spec() {
    let spec = &bts_lang::spec::SPEC;
    let skill = bts_core::skill::render(spec);
    assert!(skill.starts_with("---\nname: bts\n"));
    assert!(skill.contains(spec.surface.grammar.trim()));
    assert!(skill.contains(&bts_core::skill::reference(spec)));
    assert!(!skill.contains("## CLI commands"));
    assert!(!skill.contains("bts setup"));
}

#[test]
fn client_rejects_invalid_configuration_before_sending_requests() {
    use bts_core::{
        providers::braintrust::client::{Client, Error},
        providers::braintrust::config::Config as Braintrust,
    };
    type InvalidConfigCase = (&'static str, fn(&mut Braintrust));
    let cases: [InvalidConfigCase; 7] = [
        ("write_concurrency", |config| config.write_concurrency = 0),
        ("retry_attempts", |config| config.retry_attempts = 0),
        ("request_timeout", |config| config.request_timeout = Duration::ZERO),
        ("retry_max_elapsed", |config| config.retry_max_elapsed = Duration::ZERO),
        ("max_attachment_uploads", |config| config.max_attachment_uploads = 0),
        ("max_attachment_file_bytes", |config| config.max_attachment_file_bytes = 0),
        ("max_attachment_total_bytes", |config| config.max_attachment_total_bytes = 0),
    ];
    for (expected, invalidate) in cases {
        let mut config = Braintrust::new("test".to_owned(), uuid::Uuid::nil());
        invalidate(&mut config);
        assert!(matches!(Client::new(&config), Err(Error::InvalidConfig { field }) if field == expected));
    }
}

#[test]
fn client_keeps_its_own_configuration() {
    use bts_core::providers::braintrust::{client::Client, config::Config};
    let mut config = Config::new("test".to_owned(), uuid::Uuid::nil());
    let client = Client::new(&config).unwrap();
    config.retry_attempts = 0;
    let mut copy = client.config().clone();
    copy.write_concurrency = 0;
    assert!(client.config().retry_attempts > 0);
    assert!(client.clone().config().write_concurrency > 0);
}

#[test]
fn generation_errors_keep_their_kind_range_and_diagnostic_conversion() {
    use bts_core::mocking::data::{self, Distribution, generation};
    use std::error::Error as _;
    let source = r#"trace "run" { output = 1 / trace.index }"#;
    let error = data::generate(
        bts_lang::semantics::compile(source).unwrap(),
        1,
        Duration::from_secs(60),
        Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap_err();
    let evaluation = error.source().unwrap().downcast_ref::<generation::EvaluationError>().unwrap();
    assert_eq!(evaluation.kind(), generation::EvaluationErrorKind::DivisionByZero);
    assert!(source[evaluation.range.start..evaluation.range.end].contains('/'));
    let diag: bts_lang::Diag = (*evaluation).into();
    assert_eq!(diag.when, bts_lang::DiagPhase::Generation);
    assert_eq!(diag.r#where, evaluation.range);
    assert!(
        diag.render("shape.bt", source)
            .contains("generation error: expression divides by zero")
    );
}

#[test]
fn materialization_errors_expose_the_failure_and_event() {
    use bts_core::mocking::data::{self, Distribution, generation};
    let error = data::generate(
        bts_lang::semantics::compile(r#"trace "run" {}"#).unwrap(),
        1,
        Duration::from_nanos(1),
        Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap_err();
    let data::Error::Materialize(error) = error else {
        panic!("expected a materialization failure")
    };
    assert_eq!(error.kind(), generation::MaterializationErrorKind::WindowTooShort);
    assert_eq!(error.event_index(), 0);
}

#[test]
fn attachment_failures_keep_the_io_cause_through_the_writer() {
    use bts_core::{
        mocking::data::Attachment,
        providers::braintrust::client::{Client, writer},
        providers::braintrust::config::Config as Braintrust,
    };
    use std::error::Error as _;
    let client = Client::new(&Braintrust::new("test".to_owned(), uuid::Uuid::nil())).unwrap();
    let missing = std::env::temp_dir().join(format!("bts-missing-{}", uuid::Uuid::new_v4()));
    let error = writer::validate_attachments(
        &client,
        &[Attachment {
            path: missing.display().to_string(),
            filename: "missing.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            key: "missing".to_owned(),
        }],
    )
    .unwrap_err();
    assert!(matches!(error.kind(), writer::ErrorKind::Attachment(_)));
    let io = error
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<std::io::Error>()
        .unwrap();
    assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains(&missing.display().to_string()));
}

#[test]
fn experiment_preview_keeps_the_underlying_generation_failure() {
    use bts_core::mocking::data::generation;
    let model = bts_lang::semantics::compile(
        r#"
        trace "run" { repeat "steps" { count = range(-2, -1) tool "step" {} } }
        dataset "cases" { case "one" { input = "hello" } }
        scorer "quality" { code { score = 1 } }
        experiment "trial" { dataset = dataset.cases task = trace.run scorers = [scorer.quality] }
    "#,
    )
    .unwrap();
    let error =
        experiments::preview(&model, &[], Path::new("shape.bt"), 42, UNIX_EPOCH + Duration::from_secs(3600)).unwrap_err();
    assert!(error.to_string().contains("experiment \"trial\", row"));
    let mut cause: &dyn std::error::Error = &error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    assert_eq!(
        cause.downcast_ref::<generation::EvaluationError>().unwrap().kind(),
        generation::EvaluationErrorKind::NegativeRepeatCount
    );
}
