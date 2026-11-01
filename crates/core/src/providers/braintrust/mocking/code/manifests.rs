use crate::dsl::ScorerLang;
use crate::mocking::code::{Assembly, CodebaseContext, CodebaseFile};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::fmt;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;

pub(super) fn plan(assembly: &Assembly, context: &CodebaseContext<'_>) -> Result<Vec<CodebaseFile>, ManifestError> {
    let has_python = assembly.files.iter().any(|file| file.lang == ScorerLang::Python);
    let has_typescript = assembly.files.iter().any(|file| file.lang == ScorerLang::Typescript);
    let mut files = Vec::new();
    if has_typescript {
        files.push(CodebaseFile {
            path: "package.json".into(),
            contents: package_json(context, false)?,
            slugs: Vec::new(),
        });
        let path = Path::new("tsconfig.json");
        if !context.files.exists(path).map_err(|source| ManifestError::Read {
            path: context.root.join(path),
            source,
        })? {
            files.push(CodebaseFile {
                path: path.to_owned(),
                contents: TSCONFIG.to_owned(),
                slugs: Vec::new(),
            });
        }
    }
    if has_python {
        let modules = assembly
            .files
            .iter()
            .filter(|file| file.lang == ScorerLang::Python)
            .map(|file| format!("src/scorers/{}", file.name))
            .collect::<Vec<_>>();
        files.push(CodebaseFile {
            path: "pyproject.toml".into(),
            contents: scorer_python_modules(pyproject_toml(context, SCORER_PYTHON_DEPS)?, &modules, context)?,
            slugs: Vec::new(),
        });
        files.extend(python_packages(&modules, context)?);
    }
    Ok(files)
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .and_then(crate::providers::braintrust::naming::slugify)
        .unwrap_or_else(|| "bts-scorers".to_owned())
}

fn read_optional(path: &Path, context: &CodebaseContext<'_>) -> Result<Option<String>, ManifestError> {
    context.files.read(path).map_err(|source| ManifestError::Read {
        path: context.root.join(path),
        source,
    })
}

fn package_json(context: &CodebaseContext<'_>, agent: bool) -> Result<String, ManifestError> {
    let relative = Path::new("package.json");
    let path = context.root.join(relative);
    let root = context.root;
    let existing = read_optional(relative, context)?;
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
        &[("tsx", "^4.0.0"), ("typescript", "^5.0.0")]
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

fn pyproject_toml(context: &CodebaseContext<'_>, requirements: &[(&str, &str)]) -> Result<String, ManifestError> {
    let relative = Path::new("pyproject.toml");
    let path = context.root.join(relative);
    let root = context.root;
    let existing = read_optional(relative, context)?;
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
pub enum ManifestError {
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

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Toml { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(super) fn plan_agents(
    modules: &[String],
    lang: ScorerLang,
    context: &CodebaseContext<'_>,
    tokens: bool,
) -> Result<Vec<CodebaseFile>, ManifestError> {
    match lang {
        ScorerLang::Typescript => {
            let path = PathBuf::from("package.json");
            let mut artifacts = vec![CodebaseFile {
                contents: {
                    let contents = package_json(context, true)?;
                    let mut value: JsonValue = serde_json::from_str(&contents).expect("validated manifest");
                    let original = value.clone();
                    let dependencies = value["dependencies"].as_object_mut().expect("validated dependencies");
                    if modules.iter().any(|module| module.ends_with(".scorer.ts")) {
                        dependencies.entry("zod").or_insert_with(|| json!("^4.0.0"));
                    }
                    if tokens {
                        dependencies.entry("js-tiktoken").or_insert_with(|| json!("1.0.21"));
                    }
                    if original == value {
                        contents
                    } else {
                        format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
                    }
                },
                path,
                slugs: Vec::new(),
            }];
            if let Some(config) = agent_typescript_config(context)? {
                artifacts.push(config);
            }
            Ok(artifacts)
        }
        ScorerLang::Python => {
            let path = PathBuf::from("pyproject.toml");
            let mut dependencies = vec![("braintrust", "braintrust>=0.39,<1")];
            if modules.iter().any(|p| p.ends_with("_scorer.py")) {
                dependencies.push(("pydantic", "pydantic>=2,<3"));
            }
            if tokens {
                dependencies.push(("tiktoken", "tiktoken>=0.9,<1"));
            }
            let contents = pyproject_toml(context, &dependencies)?;
            let contents = python_modules(contents, modules, context)?;
            let mut artifacts = vec![CodebaseFile {
                path,
                contents,
                slugs: Vec::new(),
            }];
            artifacts.extend(python_packages(modules, context)?);
            Ok(artifacts)
        }
    }
}

fn scorer_python_modules(contents: String, modules: &[String], context: &CodebaseContext<'_>) -> Result<String, ManifestError> {
    let value: TomlValue = toml::from_str(&contents).map_err(|source| ManifestError::Toml {
        path: context.root.join("pyproject.toml"),
        source,
    })?;
    // Existing scorer projects may own their packaging through another backend
    // or discovery rules. Keep those settings; generated layouts use explicit mappings.
    let custom_backend = value
        .get("build-system")
        .is_some_and(|system| system.get("build-backend").and_then(TomlValue::as_str) != Some("setuptools.build_meta"));
    let setuptools = value.get("tool").and_then(|tool| tool.get("setuptools"));
    let discovery = setuptools
        .and_then(|tool| tool.get("packages"))
        .is_some_and(TomlValue::is_table);
    let custom_root = setuptools
        .and_then(|tool| tool.get("package-dir"))
        .and_then(|directories| directories.get(""))
        .is_some_and(|root| root.as_str() != Some("."));
    if custom_backend || discovery || custom_root {
        return Ok(contents);
    }
    python_modules(contents, modules, context)
}

fn python_modules(mut contents: String, modules: &[String], context: &CodebaseContext<'_>) -> Result<String, ManifestError> {
    let path = context.root.join("pyproject.toml");
    let root = context.root;
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
                "setuptools.build_meta for generated modules",
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
                && context.files.read(Path::new(&filename)).is_ok_and(|source| {
                    source.is_some_and(|source| source.to_ascii_lowercase().starts_with("# generated by bts;"))
                }))
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

fn python_packages(modules: &[String], context: &CodebaseContext<'_>) -> Result<Vec<CodebaseFile>, ManifestError> {
    let mut packages = std::collections::BTreeSet::new();
    for module in modules {
        if let Some(package) = module.strip_prefix("src/").and_then(|v| v.split('/').next()) {
            packages.insert(package);
        }
    }
    let mut files = Vec::new();
    for package in packages {
        let path = Path::new("src").join(package).join("__init__.py");
        if !context.files.exists(&path).map_err(|source| ManifestError::Read {
            path: context.root.join(&path),
            source,
        })? {
            files.push(CodebaseFile {
                path,
                contents: String::new(),
                slugs: Vec::new(),
            });
        }
    }
    Ok(files)
}

fn agent_typescript_config(context: &CodebaseContext<'_>) -> Result<Option<CodebaseFile>, ManifestError> {
    let path = Path::new("tsconfig.json");
    if context.files.exists(path).map_err(|source| ManifestError::Read {
        path: context.root.join(path),
        source,
    })? {
        let previous = match context.files.read(path) {
            Ok(Some(contents)) => contents,
            Ok(None) => String::new(),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => return Ok(None),
            Err(source) => {
                return Err(ManifestError::Read {
                    path: context.root.join(path),
                    source,
                });
            }
        };
        let recognized = serde_json::from_str::<JsonValue>(&previous)
            .ok()
            .is_some_and(|value| value == serde_json::from_str::<JsonValue>(TSCONFIG).unwrap());
        if !recognized {
            return Ok(None);
        }
    }
    Ok(Some(CodebaseFile {
        path: path.to_owned(),
        contents: AGENT_TSCONFIG.to_owned(),
        slugs: Vec::new(),
    }))
}

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
