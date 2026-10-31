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
            contents: package_json(context)?,
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
        files.push(CodebaseFile {
            path: "pyproject.toml".into(),
            contents: pyproject_toml(context)?,
            slugs: Vec::new(),
        });
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

fn package_json(context: &CodebaseContext<'_>) -> Result<String, ManifestError> {
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
    for (field, dependencies) in [
        ("dependencies", [("braintrust", "^3.30.0"), ("zod", "^4.0.0")].as_slice()),
        ("devDependencies", [("tsx", "^4.0.0"), ("typescript", "^5.0.0")].as_slice()),
    ] {
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

fn pyproject_toml(context: &CodebaseContext<'_>) -> Result<String, ManifestError> {
    let relative = Path::new("pyproject.toml");
    let path = context.root.join(relative);
    let root = context.root;
    let existing = read_optional(relative, context)?;
    let Some(contents) = existing else {
        return Ok(format!(
            "[project]\nname = {:?}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [\"braintrust>=0.39,<1\", \"pydantic>=2,<3\"]\n\n[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[tool.setuptools]\npy-modules = []\n",
            project_name(root)
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
    let missing: Vec<&str> = [("braintrust", "braintrust>=0.39,<1"), ("pydantic", "pydantic>=2,<3")]
        .into_iter()
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
