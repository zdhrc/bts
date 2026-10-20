//! Assemble generated functions into modules and a buildable project.

use super::emission::registry;
use super::lowering::scoring as emit;
use super::planning::{callable_name, resolve_slugs, source_name};
use super::{lowering, planning};
use crate::dsl::{Scorer, ScorerKind, ScorerLang};
// SDK registrations are the source files that `bt functions push` discovers.
pub(crate) fn assemble(scorers: &[Scorer], lang: Option<ScorerLang>, project: &str) -> Result<Assembly, Error> {
    let slugs = resolve_slugs(scorers)?;
    let groups = planning::scorer_groups(scorers, &slugs, lang)?;
    let mut files = Vec::new();
    for planning::ScorerGroup {
        stem,
        members,
        lang: file_lang,
    } in groups
    {
        let items: Vec<emit::Item> = members
            .iter()
            .map(|&member| {
                let ScorerKind::Code { score, steps } = &scorers[member].kind else {
                    unreachable!("judges were filtered out")
                };
                emit::Item {
                    name: &scorers[member].name,
                    slug: &slugs[member],
                    score,
                    steps,
                }
            })
            .collect();
        let mut source = emit::module(file_lang, &items).map_err(Error::Emit)?;
        if file_lang == ScorerLang::Typescript {
            for item in &items {
                let name = emit::fn_name(item.slug);
                source = source.replace(&format!("function {name}("), &format!("export function {name}("));
            }
        }
        let implementation = callable_name(&stem, file_lang);
        files.push(SourceFile {
            name: implementation.clone(),
            contents: source,
            slugs: Vec::new(),
            lang: file_lang,
        });
        files.push(SourceFile {
            name: source_name(&stem, file_lang),
            contents: registry::code(file_lang, project, &items, &implementation),
            slugs: members.iter().map(|&member| slugs[member].clone()).collect(),
            lang: file_lang,
        });
    }

    for (scorer, slug) in scorers.iter().zip(&slugs) {
        if let ScorerKind::Judge { model, prompt, options } = &scorer.kind {
            let judge_lang = lang.unwrap_or(ScorerLang::Python);
            files.push(SourceFile {
                name: source_name(slug, judge_lang),
                contents: registry::judge(
                    judge_lang,
                    project,
                    &scorer.name,
                    slug,
                    model,
                    &lowering::mustache(prompt),
                    options.iter().map(|option| (option.label.as_str(), option.score)).collect(),
                ),
                slugs: vec![slug.clone()],
                lang: judge_lang,
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    for file in &files {
        if !seen.insert(&file.name) {
            return Err(Error::DuplicateFile { file: file.name.clone() });
        }
    }
    Ok(Assembly { files })
}

pub(crate) struct Assembly {
    pub(crate) files: Vec<SourceFile>,
}

pub(crate) struct SourceFile {
    pub(crate) name: String,
    pub(crate) contents: String,
    pub(crate) slugs: Vec<String>,
    pub(crate) lang: ScorerLang,
}

#[derive(Debug)]
pub(crate) enum Error {
    UnsluggableName { name: String },
    DuplicateSlug { slug: String, first: String, second: String },
    PackedLangConflict { file: String, first: String, second: String },
    DuplicateFile { file: String },
    Emit(emit::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsluggableName { name } => {
                write!(formatter, "scorer name \"{name}\" has no alphanumeric characters to slug")
            }
            Self::DuplicateSlug { slug, first, second } => write!(
                formatter,
                "scorers \"{first}\" and \"{second}\" both use slug `{slug}`; rename one"
            ),
            Self::PackedLangConflict { file, first, second } => write!(
                formatter,
                "scorers \"{first}\" and \"{second}\" pack into file `{file}` but resolve to different languages"
            ),
            Self::DuplicateFile { file } => write!(
                formatter,
                "multiple scorers would build into `{file}`; change a code scorer's file stem"
            ),
            Self::Emit(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};
use std::{fmt, fs};
use toml::Value as TomlValue;

pub(crate) struct ManifestArtifact {
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
}

pub(super) fn plan_manifests(assembly: &Assembly, root: &Path) -> Result<Vec<ManifestArtifact>, ManifestError> {
    let has_python = assembly.files.iter().any(|file| file.lang == ScorerLang::Python);
    let has_typescript = assembly.files.iter().any(|file| file.lang == ScorerLang::Typescript);
    let mut artifacts = Vec::new();
    if has_typescript {
        let path = root.join("package.json");
        artifacts.push(ManifestArtifact {
            contents: package_json(&path, root, false)?,
            path,
        });
        if let Some(config) = typescript_config(root)? {
            artifacts.push(config);
        }
    }
    if has_python {
        let path = root.join("pyproject.toml");
        artifacts.push(ManifestArtifact {
            contents: agent_python_modules(
                pyproject_toml(&path, root, SCORER_PYTHON_DEPS)?,
                &assembly
                    .files
                    .iter()
                    .filter(|f| f.lang == ScorerLang::Python)
                    .map(|f| format!("src/scorers/{}", f.name))
                    .collect::<Vec<_>>(),
                &path,
                root,
            )?,
            path,
        });
    }
    if has_python {
        artifacts.extend(python_packages(&["src/scorers/placeholder.py".into()], root));
    }
    Ok(artifacts)
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .and_then(planning::slugify)
        .unwrap_or_else(|| "bts-scorers".to_owned())
}

fn read_optional(path: &Path) -> Result<Option<String>, ManifestError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ManifestError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn package_json(path: &Path, root: &Path, agent: bool) -> Result<String, ManifestError> {
    let existing = read_optional(path)?;
    let mut value: JsonValue = match existing {
        Some(ref contents) => serde_json::from_str(contents).map_err(|source| ManifestError::Json {
            path: path.to_owned(),
            source,
        })?,
        None => json!({
            "name": project_name(root),
            "private": true,
            "type": "module",
            "scripts": { "build": "tsc -p tsconfig.json" },
            "dependencies": {},
            "devDependencies": {},
        }),
    };
    let object = value.as_object_mut().ok_or_else(|| ManifestError::Field {
        path: path.to_owned(),
        field: "root",
        expected: "an object",
    })?;
    let runtime: &[(&str, &str)] = if agent {
        &[("braintrust", "^3.30.0")]
    } else {
        &[("braintrust", "^3.30.0"), ("zod", "^4.0.0")]
    };
    let dev: &[(&str, &str)] = if agent {
        &[("typescript", "^5.7.0"), ("@types/node", "^24.2.0")]
    } else {
        &[("tsx", "^4.0.0"), ("typescript", "^5.7.0"), ("@types/node", "^24.2.0")]
    };
    if agent {
        let scripts = object.entry("scripts").or_insert_with(|| json!({}));
        let scripts = scripts.as_object_mut().ok_or_else(|| ManifestError::Field {
            path: path.to_owned(),
            field: "scripts",
            expected: "an object",
        })?;
        scripts.entry("build").or_insert_with(|| json!("tsc -p tsconfig.json"));
        object.entry("engines").or_insert_with(|| json!({"node": ">=24.2.0"}));
    }
    for (field, dependencies) in [("dependencies", runtime), ("devDependencies", dev)] {
        let section = object.entry(field).or_insert_with(|| json!({}));
        let section = section.as_object_mut().ok_or_else(|| ManifestError::Field {
            path: path.to_owned(),
            field,
            expected: "an object",
        })?;
        for (name, version) in dependencies {
            section.entry(*name).or_insert_with(|| json!(version));
        }
    }
    if let Some(contents) = existing {
        let original: JsonValue = serde_json::from_str(&contents).expect("parsed above");
        if original == value {
            return Ok(contents);
        }
    }
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("JSON values serialize")
    ))
}

const SCORER_PYTHON_DEPS: &[(&str, &str)] = &[("braintrust", "braintrust>=0.39,<1"), ("pydantic", "pydantic>=2,<3")];

fn pyproject_toml(path: &Path, root: &Path, requirements: &[(&str, &str)]) -> Result<String, ManifestError> {
    let existing = read_optional(path)?;
    let Some(contents) = existing else {
        return Ok(format!(
            "[project]\nname = {:?}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [{}]\n\n[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[tool.setuptools]\npy-modules = []\n",
            project_name(root),
            requirements
                .iter()
                .map(|(_, value)| format!("{value:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let value: TomlValue = toml::from_str(&contents).map_err(|source| ManifestError::Toml {
        path: path.to_owned(),
        source,
    })?;
    let project = value
        .get("project")
        .map(|value| {
            value.as_table().ok_or_else(|| ManifestError::Field {
                path: path.to_owned(),
                field: "project",
                expected: "a table",
            })
        })
        .transpose()?;
    let dependencies = project.and_then(|project| project.get("dependencies"));
    let dependencies = dependencies
        .map(|value| {
            value.as_array().ok_or_else(|| ManifestError::Field {
                path: path.to_owned(),
                field: "project.dependencies",
                expected: "an array",
            })
        })
        .transpose()?;
    let missing: Vec<&str> = requirements
        .iter()
        .copied()
        .filter_map(|(name, requirement)| {
            (!dependencies.is_some_and(|dependencies| {
                dependencies.iter().any(|entry| {
                    entry
                        .as_str()
                        .is_some_and(|entry| requirement_name(entry).eq_ignore_ascii_case(name))
                })
            }))
            .then_some(requirement)
        })
        .collect();
    if missing.is_empty() {
        return Ok(contents);
    }
    let quoted = missing
        .iter()
        .map(|requirement| format!("{requirement:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(dependencies) = dependencies {
        let parsed: PyprojectSpans = toml::from_str(&contents).map_err(|source| ManifestError::Toml {
            path: path.to_owned(),
            source,
        })?;
        let span = parsed
            .project
            .and_then(|project| project.dependencies)
            .expect("validated above")
            .span();
        let mut all = dependencies
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(|value| format!("{value:?}"))
                    .ok_or_else(|| ManifestError::Field {
                        path: path.to_owned(),
                        field: "project.dependencies",
                        expected: "an array of strings",
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        all.extend(missing.iter().map(|value| format!("{value:?}")));
        let mut output = contents;
        output.replace_range(span, &format!("[{}]", all.join(", ")));
        return Ok(output);
    }
    let mut output = contents;
    if project.is_some() {
        let header = output
            .lines()
            .position(|line| line.trim().split('#').next().is_some_and(|head| head.trim() == "[project]"))
            .ok_or_else(|| ManifestError::Field {
                path: path.to_owned(),
                field: "project",
                expected: "a [project] table header",
            })?;
        let offset = output.split_inclusive('\n').take(header + 1).map(str::len).sum::<usize>();
        let prefix = if offset == output.len() && !output.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        output.insert_str(offset, &format!("{prefix}dependencies = [{quoted}]\n"));
    } else {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&format!(
            "\n[project]\nname = {:?}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [{quoted}]\n",
            project_name(root)
        ));
    }
    Ok(output)
}

#[derive(Deserialize)]
struct PyprojectSpans {
    project: Option<ProjectSpans>,
}

#[derive(Deserialize)]
struct ProjectSpans {
    dependencies: Option<toml::Spanned<Vec<String>>>,
}

fn requirement_name(requirement: &str) -> &str {
    requirement
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '-' || character == '_'))
        .next()
        .unwrap_or_default()
}

const TSCONFIG: &str = "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"ESNext\",\n    \"moduleResolution\": \"Bundler\",\n    \"rootDir\": \"src\",\n    \"outDir\": \"dist\",\n    \"strict\": true,\n    \"esModuleInterop\": true,\n    \"skipLibCheck\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n";

#[derive(Debug)]
pub(crate) enum ManifestError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    Field {
        path: PathBuf,
        field: &'static str,
        expected: &'static str,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(formatter, "failed to read {}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "invalid {}: {source}", path.display()),
            Self::Toml { path, source } => write!(formatter, "invalid {}: {source}", path.display()),
            Self::Field { path, field, expected } => {
                write!(formatter, "{} field `{field}` must be {expected}", path.display())
            }
        }
    }
}

impl std::error::Error for ManifestError {}

fn agent_python_modules(mut contents: String, modules: &[String], path: &Path, root: &Path) -> Result<String, ManifestError> {
    let value: TomlValue = toml::from_str(&contents).map_err(|source| ManifestError::Toml {
        path: path.to_owned(),
        source,
    })?;
    let invalid = |field, expected| ManifestError::Field {
        path: path.to_owned(),
        field,
        expected,
    };
    if value.get("project").is_none() {
        contents.push_str(&format!(
            "\n[project]\nname = {:?}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = []\n",
            project_name(root)
        ));
    }
    if let Some(system) = value.get("build-system") {
        if system.get("build-backend").and_then(TomlValue::as_str) != Some("setuptools.build_meta") {
            return Err(invalid(
                "build-system.build-backend",
                "setuptools.build_meta for generated agent modules",
            ));
        }
        if !system
            .get("requires")
            .and_then(TomlValue::as_array)
            .is_some_and(|requirements| {
                requirements.iter().any(|requirement| {
                    requirement
                        .as_str()
                        .is_some_and(|requirement| requirement_name(requirement).eq_ignore_ascii_case("setuptools"))
                })
            })
        {
            return Err(invalid("build-system.requires", "an array containing setuptools"));
        }
    } else {
        contents.push_str("\n[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n");
    }
    let mut setuptools = value
        .get("tool")
        .and_then(|tool| tool.get("setuptools"))
        .cloned()
        .unwrap_or_else(|| TomlValue::Table(Default::default()));
    let table = setuptools
        .as_table_mut()
        .ok_or_else(|| invalid("tool.setuptools", "a table"))?;
    if table.get("py-modules").is_some_and(|v| v.as_array().is_none()) {
        return Err(invalid("tool.setuptools.py-modules", "an array"));
    }
    if let Some(TomlValue::Array(existing)) = table.get_mut("py-modules") {
        existing.retain(|module| {
            let Some(module) = module.as_str() else {
                return true;
            };
            let filename = format!("{module}.py");
            let generated = modules.iter().any(|path| {
                path.strip_prefix("src/agents/")
                    .or_else(|| path.strip_prefix("src/evals/"))
                    .is_some_and(|name| {
                        name == filename
                            || name
                                .strip_suffix(".py")
                                .is_some_and(|stem| format!("{stem}_agent.py") == filename)
                    })
            });
            !(generated
                && fs::read_to_string(root.join(&filename))
                    .is_ok_and(|source| source.to_ascii_lowercase().starts_with("# generated by bts;")))
        });
    }
    let mut packages = table
        .get("packages")
        .map(|v| {
            v.as_array()
                .cloned()
                .ok_or_else(|| invalid("tool.setuptools.packages", "an explicit package array"))
        })
        .transpose()?
        .unwrap_or_default();
    let mut directories = table
        .get("package-dir")
        .map(|v| {
            v.as_table()
                .cloned()
                .ok_or_else(|| invalid("tool.setuptools.package-dir", "a table"))
        })
        .transpose()?
        .unwrap_or_default();
    if directories.get("").is_some_and(|v| v.as_str() != Some(".")) {
        return Err(invalid(
            "tool.setuptools.package-dir",
            "explicit package mappings without a different root directory",
        ));
    }
    for module in modules {
        let Some(package) = module.strip_prefix("src/").and_then(|v| v.split('/').next()) else {
            continue;
        };
        let directory = format!("src/{package}");
        if directories.get(package).is_some_and(|v| v.as_str() != Some(&directory)) {
            return Err(invalid("tool.setuptools.package-dir", "generated packages mapped under src/"));
        }
        directories.insert(package.to_owned(), TomlValue::String(directory));
        let package = TomlValue::String(package.to_owned());
        if !packages.contains(&package) {
            packages.push(package);
        }
    }
    table.insert("packages".into(), TomlValue::Array(packages));
    table.insert("package-dir".into(), TomlValue::Table(directories));
    if value.get("tool").and_then(|tool| tool.get("setuptools")) == Some(&setuptools) {
        return Ok(contents);
    }
    // Replace only setuptools configuration; preserve project settings and other tools.
    let mut kept = String::new();
    let mut inside = false;
    for line in contents.split_inclusive('\n') {
        let head = line.split('#').next().unwrap_or("").trim();
        if head.starts_with('[') {
            inside = head == "[tool.setuptools]" || head.starts_with("[tool.setuptools.");
        }
        if !inside {
            kept.push_str(line);
        }
    }
    let mut tool = toml::map::Map::new();
    tool.insert("setuptools".into(), setuptools);
    let mut document = toml::map::Map::new();
    document.insert("tool".into(), TomlValue::Table(tool));
    kept.push('\n');
    kept.push_str(&toml::to_string(&TomlValue::Table(document)).expect("valid setuptools table"));
    Ok(kept)
}

fn python_packages(modules: &[String], root: &Path) -> Vec<ManifestArtifact> {
    let mut packages = std::collections::BTreeSet::new();
    for module in modules {
        if let Some(package) = module.strip_prefix("src/").and_then(|v| v.split('/').next()) {
            packages.insert(package);
        }
    }
    packages
        .into_iter()
        .filter_map(|package| {
            let path = root.join("src").join(package).join("__init__.py");
            (!path.exists()).then_some(ManifestArtifact {
                path,
                contents: String::new(),
            })
        })
        .collect()
}

fn typescript_config(root: &Path) -> Result<Option<ManifestArtifact>, ManifestError> {
    let path = root.join("tsconfig.json");
    let previous = read_optional(&path)?;
    // Upgrade our previous defaults, while leaving custom configurations alone.
    let legacy = previous.as_ref().is_some_and(|text| {
        serde_json::from_str::<JsonValue>(text).ok().is_some_and(|value| {
            let mut previous_default = serde_json::from_str::<JsonValue>(AGENT_TSCONFIG).unwrap();
            previous_default["compilerOptions"]
                .as_object_mut()
                .unwrap()
                .remove("rewriteRelativeImportExtensions");
            value == previous_default
                || value == serde_json::from_str::<JsonValue>(LEGACY_AGENT_TSCONFIG).unwrap()
                || value == serde_json::from_str::<JsonValue>(TSCONFIG).unwrap()
        })
    });
    Ok((previous.is_none() || legacy).then_some(ManifestArtifact {
        path,
        contents: AGENT_TSCONFIG.to_owned(),
    }))
}

const LEGACY_AGENT_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "rootDir": ".",
    "outDir": "dist",
    "strict": true,
    "declaration": true,
    "types": ["node"],
    "skipLibCheck": true
  },
  "include": ["*.mts"],
  "exclude": ["dist", "node_modules"]
}
"#;

const AGENT_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "rewriteRelativeImportExtensions": true,
    "rootDir": "src",
    "outDir": "dist",
    "strict": true,
    "declaration": true,
    "types": ["node"],
    "skipLibCheck": true
  },
  "include": ["src/**/*.mts", "src/**/*.ts"],
  "exclude": ["dist", "node_modules"]
}
"#;
