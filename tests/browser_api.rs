use bts::browser::{GenerationOptions, check, generate, language_reference, version};

#[test]
fn browser_api_validates_and_generates() {
    let source = include_str!("fixtures/dynamic.bt");
    assert!(check(source).is_empty());
    let options = GenerationOptions {
        count: 3,
        seed: "42".into(),
        start: "2026-09-01T00:00:00Z".into(),
        end: "2026-09-01T01:00:00Z".into(),
        distribution: Default::default(),
    };
    let events = generate(source, options).unwrap();
    assert!(!events["events"].as_array().unwrap().is_empty());
    assert!(language_reference().contains(version()));
    let errors = check("trace \"t\" {\n input = }\n}");
    assert_eq!(errors[0].location.as_ref().unwrap().line, 2);
}

fn options() -> GenerationOptions {
    GenerationOptions {
        count: 2,
        seed: "18446744073709551615".into(),
        start: "2026-09-01T00:00:00Z".into(),
        end: "2026-09-01T01:00:00Z".into(),
        distribution: Default::default(),
    }
}

#[test]
fn rejects_invalid_options_without_panicking() {
    let mut invalid = options();
    invalid.count = 0;
    assert!(generate("trace \"t\" {}", invalid).unwrap_err().message.contains("count"));
    for seed in ["-1", "", "+1", "18446744073709551616"] {
        let mut invalid = options();
        invalid.seed = seed.into();
        assert!(generate("trace \"t\" {}", invalid).is_err());
    }
    for end in ["not-a-timestamp", "2026-09-01T00:00:00Z", "1960-01-01T00:00:00Z"] {
        let mut invalid = options();
        invalid.end = end.into();
        assert!(generate("trace \"t\" {}", invalid).is_err());
    }
}

#[test]
fn reports_compile_and_evaluation_locations() {
    let syntax = "trace \"é😀\" {\n input = }\n}";
    let checked = check(syntax);
    let generated = generate(syntax, options()).unwrap_err();
    assert_eq!(
        serde_json::to_value(&checked).unwrap(),
        serde_json::to_value(generated.diagnostics).unwrap()
    );
    let location = checked[0].location.as_ref().unwrap();
    assert_eq!((location.line, location.column), (2, 10));
    assert_eq!(location.start, "trace \"é😀\" {\n input = ".len());
    let error = generate("trace \"t\" { input = 1 / range(0, 0) }", options()).unwrap_err();
    assert_eq!(error.diagnostics[0].phase, "generation");
    assert!(error.diagnostics[0].location.is_some());
}

#[test]
fn rejects_filesystem_dependent_attachments_and_reports_materialization_errors() {
    let source = "trace \"t\" { input = attachment(\"report.pdf\", \"application/pdf\") }";
    assert!(check(source).is_empty());
    assert!(generate(source, options()).unwrap_err().message.contains("attachments"));
    let source = "trace \"t\" { duration = 7200 }";
    let error = generate(source, options()).unwrap_err();
    assert!(error.message.contains("window"));
    assert!(error.diagnostics.is_empty());
}

#[test]
fn validates_existing_fixtures_and_embeds_the_release_version() {
    assert!(language_reference().contains(concat!("BTS release ", env!("CARGO_PKG_VERSION"))));
    for source in [
        include_str!("fixtures/simple.bt"),
        include_str!("fixtures/dynamic.bt"),
        include_str!("fixtures/scorers.bt"),
    ] {
        assert!(check(source).is_empty());
    }
}
