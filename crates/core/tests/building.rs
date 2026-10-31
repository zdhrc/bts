use bts_core::mocking::code::{
    self, CodebaseContext,
    languages::Languages,
    writing::{Action, Directory},
};
use bts_core::providers::braintrust::mocking::code::CodeTarget;
use bts_lang::{ScorerLang, semantics::compile};
use std::fs;

const SOURCE: &str = include_str!("fixtures/building.bt");

#[test]
fn builds_packed_python_scorers_and_a_judge() {
    let model = compile(SOURCE).unwrap();
    let assembly = code::assemble(&model.scorers, None, &Languages, &CodeTarget::new("test-project")).unwrap();
    assert_eq!(assembly.files.len(), 2);
    let out = std::env::temp_dir().join(format!("bts-building-test-{}", uuid::Uuid::new_v4()));

    let built = build(&assembly, &out).unwrap();
    assert!(built.iter().all(|file| file.action == Action::Created));
    let code = built
        .iter()
        .find(|file| file.slugs == ["answer-quality", "answer-coverage"])
        .unwrap();
    assert_eq!(code.path.file_name().unwrap(), "answer_quality_scorer.py");
    let source = fs::read_to_string(&code.path).unwrap();
    assert!(source.contains("def scorer_answer_quality("));
    assert!(source.contains("def scorer_answer_coverage("));
    assert!(source.contains("project = braintrust.projects.create(name=\"test-project\")"));
    let judge = built.iter().find(|file| file.slugs == ["helpfulness"]).unwrap();
    assert_eq!(judge.path.file_name().unwrap(), "helpfulness_scorer.py");
    let source = fs::read_to_string(&judge.path).unwrap();
    assert!(source.contains("choice_scores="));
    assert!(source.contains("gpt-4o-mini"));
    let manifest = fs::read_to_string(out.join("pyproject.toml")).unwrap();
    assert!(manifest.contains("braintrust>=0.39,<1"));

    let rebuilt = build(&assembly, &out).unwrap();
    assert!(rebuilt.iter().all(|file| file.action == Action::Unchanged));
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn builds_typescript_scorers_and_their_codebase_files() {
    let model = compile(SOURCE).unwrap();
    let assembly = code::assemble(
        &model.scorers,
        Some(ScorerLang::Typescript),
        &Languages,
        &CodeTarget::new("test-project"),
    )
    .unwrap();
    let out = std::env::temp_dir().join(format!("bts-building-test-{}", uuid::Uuid::new_v4()));

    let built = build(&assembly, &out).unwrap();
    let code = built
        .iter()
        .find(|file| file.slugs == ["answer-quality", "answer-coverage"])
        .unwrap();
    assert_eq!(code.path.file_name().unwrap(), "answer-quality.scorer.ts");
    let source = fs::read_to_string(&code.path).unwrap();
    assert!(source.contains("scorer_answer_quality"));
    assert!(source.contains("scorer_answer_coverage"));
    let judge = built.iter().find(|file| file.slugs == ["helpfulness"]).unwrap();
    assert!(fs::read_to_string(&judge.path).unwrap().contains("choiceScores:"));
    let package: serde_json::Value = serde_json::from_str(&fs::read_to_string(out.join("package.json")).unwrap()).unwrap();
    assert!(package["dependencies"].get("braintrust").is_some());
    assert!(package["dependencies"].get("zod").is_some());
    assert!(out.join("tsconfig.json").exists());
    fs::remove_dir_all(out).unwrap();
}

fn build(assembly: &code::Assembly, out: &std::path::Path) -> Result<Vec<code::writing::Built>, Box<dyn std::error::Error>> {
    let files = Directory::new(out);
    let plan = code::plan_codebase(
        assembly,
        &CodeTarget::new("test-project"),
        &CodebaseContext {
            root: out,
            files: &files,
        },
    )?;
    Ok(code::writing::build(&plan, out, |_, _| {})?)
}

#[test]
fn preserves_existing_manifest_settings_and_unread_tsconfig() {
    let model = compile(r#"scorer "quality" { code { score = 0.5 } }"#).unwrap();
    let out = std::env::temp_dir().join(format!("bts-building-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&out).unwrap();
    let pyproject = "# project notes\n[project] # settings\nname = \"kept\"\ndependencies = [\"braintrust==0.40.0\", \"requests>=2\"] # dependencies\n\n[tool.example]\nactive = true\n";
    fs::write(out.join("pyproject.toml"), pyproject).unwrap();
    let package = r#"{"name":"kept","scripts":{"build":"custom"},"dependencies":{"braintrust":"workspace:*","zod":"^3","extra":"1"},"devDependencies":{"tsx":"old","typescript":"~4"}}"#;
    fs::write(out.join("package.json"), package).unwrap();
    fs::write(out.join("tsconfig.json"), [0xff]).unwrap();

    let target = CodeTarget::new("test-project");
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble(&model.scorers, Some(lang), &Languages, &target).unwrap();
        build(&assembly, &out).unwrap();
    }
    let merged = fs::read_to_string(out.join("pyproject.toml")).unwrap();
    assert!(merged.starts_with("# project notes\n[project] # settings\nname = \"kept\"\n"));
    assert!(merged.contains("\"braintrust==0.40.0\", \"requests>=2\", \"pydantic>=2,<3\"] # dependencies"));
    assert!(merged.ends_with("[tool.example]\nactive = true\n"));
    assert_eq!(fs::read_to_string(out.join("package.json")).unwrap(), package);
    assert_eq!(fs::read(out.join("tsconfig.json")).unwrap(), [0xff]);
    fs::remove_dir_all(out).unwrap();
}
