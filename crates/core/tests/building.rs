use bts_core::mocking::code::{
    self, AgentTarget, CodebaseContext,
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
    assert_eq!(assembly.files.len(), 3);
    let out = std::env::temp_dir().join(format!("bts-building-test-{}", uuid::Uuid::new_v4()));

    let built = build(&assembly, &out).unwrap();
    assert!(built.iter().all(|file| file.action == Action::Created));
    let code = built
        .iter()
        .find(|file| file.slugs == ["answer-quality", "answer-coverage"])
        .unwrap();
    assert_eq!(code.path.file_name().unwrap(), "answer_quality_scorer.py");
    let source = fs::read_to_string(&code.path).unwrap();
    assert!(!source.contains("def scorer_answer_quality("));
    assert!(source.contains("scorer_answer_quality"));
    assert!(source.contains("scorer_answer_coverage"));
    let implementation = fs::read_to_string(out.join("src/scorers/answer_quality.py")).unwrap();
    assert!(implementation.contains("def scorer_answer_quality("));
    assert!(implementation.contains("def scorer_answer_coverage("));
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
    assert!(merged.contains("[tool.example]\nactive = true\n"));
    assert_eq!(fs::read_to_string(out.join("package.json")).unwrap(), package);
    assert_eq!(fs::read(out.join("tsconfig.json")).unwrap(), [0xff]);
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn invalid_removal_paths_fail_before_writes_or_removals() {
    for path in ["", "../outside.py", "/outside.py"] {
        let out = std::env::temp_dir().join(format!("bts-removal-test-{}", uuid::Uuid::new_v4()));
        let plan = code::CodebasePlan {
            files: vec![code::CodebaseFile {
                path: "new.py".into(),
                contents: "pass\n".into(),
                slugs: Vec::new(),
            }],
            removals: vec![code::CodebaseRemoval {
                path: path.into(),
                expected: "old source".into(),
            }],
        };
        assert!(matches!(
            code::writing::build(&plan, &out, |_, _| panic!("preflight must precede writes")),
            Err(code::writing::Error::InvalidPath { .. })
        ));
        assert!(!out.exists());
    }
}

#[test]
fn removes_unchanged_snapshots_and_preserves_files_edited_after_planning() {
    let out = std::env::temp_dir().join(format!("bts-removal-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("old.py"), "generated source\n").unwrap();
    fs::write(out.join("edited.py"), "generated source\n").unwrap();
    let plan = code::CodebasePlan {
        files: vec![code::CodebaseFile {
            path: "src/agents/current.py".into(),
            contents: "replacement source\n".into(),
            slugs: Vec::new(),
        }],
        removals: ["old.py", "edited.py", "already_removed.py"]
            .into_iter()
            .map(|path| code::CodebaseRemoval {
                path: path.into(),
                expected: "generated source\n".into(),
            })
            .collect(),
    };
    fs::write(out.join("edited.py"), "user changes\n").unwrap();
    code::writing::build(&plan, &out, |_, _| {}).unwrap();
    assert!(!out.join("old.py").exists());
    assert!(!out.join("already_removed.py").exists());
    assert_eq!(fs::read_to_string(out.join("edited.py")).unwrap(), "user changes\n");
    assert_eq!(
        fs::read_to_string(out.join("src/agents/current.py")).unwrap(),
        "replacement source\n"
    );
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn python_packages_include_agents_and_scorers_in_either_build_order() {
    let model = compile(
        r#"
        agent "support" { entrypoint = trace.support }
        trace "support" { output = "hello" }
        scorer "quality" { code { score = 0.5 } }
    "#,
    )
    .unwrap();
    let target = CodeTarget::new("test-project");
    let agents = code::assemble_agents(&model, ScorerLang::Python, &target).unwrap();
    let scorers = code::assemble(&model.scorers, Some(ScorerLang::Python), &Languages, &target).unwrap();

    for agents_first in [true, false] {
        let out = std::env::temp_dir().join(format!("bts-package-order-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(out.join("src/custom")).unwrap();
        fs::create_dir_all(out.join("src/scorers")).unwrap();
        fs::write(out.join("src/custom/__init__.py"), "CUSTOM = True\n").unwrap();
        fs::write(out.join("src/scorers/__init__.py"), "# user scorer exports\n").unwrap();
        fs::write(
            out.join("pyproject.toml"),
            r#"# user settings
[project]
name = "kept"
version = "0.1.0"
dependencies = ["braintrust==0.40.0"]

[tool.example]
active = true

[tool.setuptools]
packages = ["custom"]

[tool.setuptools.package-dir]
custom = "src/custom"
"#,
        )
        .unwrap();
        let build_agents = || {
            let directory = Directory::new(&out);
            let plan = target
                .agent_codebase(
                    &agents.files,
                    ScorerLang::Python,
                    &CodebaseContext {
                        root: &out,
                        files: &directory,
                    },
                )
                .unwrap();
            code::writing::build(&plan, &out, |_, _| {}).unwrap()
        };
        if agents_first {
            build_agents();
            build(&scorers, &out).unwrap();
        } else {
            build(&scorers, &out).unwrap();
            build_agents();
        }
        let contents = fs::read_to_string(out.join("pyproject.toml")).unwrap();
        let manifest: toml::Value = toml::from_str(&contents).unwrap();
        let packages = manifest["tool"]["setuptools"]["packages"].as_array().unwrap();
        for package in ["agents", "scorers", "custom"] {
            assert!(packages.contains(&toml::Value::String(package.into())), "{contents}");
            assert_eq!(
                manifest["tool"]["setuptools"]["package-dir"][package].as_str(),
                Some(format!("src/{package}").as_str())
            );
            assert!(out.join("src").join(package).join("__init__.py").exists());
        }
        assert!(contents.starts_with("# user settings\n"));
        assert_eq!(manifest["project"]["name"].as_str(), Some("kept"));
        assert_eq!(manifest["project"]["dependencies"][0].as_str(), Some("braintrust==0.40.0"));
        assert_eq!(manifest["tool"]["example"]["active"].as_bool(), Some(true));
        assert_eq!(
            fs::read_to_string(out.join("src/scorers/__init__.py")).unwrap(),
            "# user scorer exports\n"
        );
        assert!(build_agents().iter().all(|file| file.action == Action::Unchanged));
        assert!(
            build(&scorers, &out)
                .unwrap()
                .iter()
                .all(|file| file.action == Action::Unchanged)
        );
        fs::remove_dir_all(out).unwrap();
    }
}

#[test]
fn scorer_build_preserves_an_existing_python_backend() {
    assert_custom_python_packaging_preserved(
        r#"
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["src/custom"]
"#,
    );
}

#[test]
fn scorer_build_preserves_setuptools_discovery_and_custom_roots() {
    for packaging in [
        r#"
[tool.setuptools.packages.find]
where = ["src"]
include = ["custom*"]
"#,
        r#"
[tool.setuptools]
packages = ["custom"]
package-dir = { "" = "src" }
"#,
    ] {
        assert_custom_python_packaging_preserved(&format!(
            r#"
[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"
{packaging}"#
        ));
    }
}

fn assert_custom_python_packaging_preserved(packaging: &str) {
    let out = std::env::temp_dir().join(format!("bts-custom-packaging-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&out).unwrap();
    let original = format!(
        r#"# project settings
[project]
name = "kept"
version = "0.1.0"
dependencies = ["braintrust==0.40.0"] # pinned
{packaging}"#
    );
    fs::write(out.join("pyproject.toml"), &original).unwrap();
    let model = compile(r#"scorer "quality" { code { score = 0.5 } }"#).unwrap();
    let assembly = code::assemble(
        &model.scorers,
        Some(ScorerLang::Python),
        &Languages,
        &CodeTarget::new("test-project"),
    )
    .unwrap();
    build(&assembly, &out).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("pyproject.toml")).unwrap(),
        original.replace("[\"braintrust==0.40.0\"]", "[\"braintrust==0.40.0\", \"pydantic>=2,<3\"]"),
    );
    assert!(out.join("src/scorers/__init__.py").exists());
    assert!(out.join("src/scorers/quality.py").exists());
    assert!(out.join("src/scorers/quality_scorer.py").exists());
    assert!(
        build(&assembly, &out)
            .unwrap()
            .iter()
            .all(|file| file.action == Action::Unchanged)
    );
    fs::remove_dir_all(out).unwrap();
}
