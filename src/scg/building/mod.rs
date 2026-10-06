mod builder;

pub(crate) use builder::{Action, Built, Error, build};

use super::Assembly;
use crate::dsl::ScorerLang;
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};
use std::{fmt, fs};
use toml::Value as TomlValue;

struct ManifestArtifact {
    path: PathBuf,
    contents: String,
}

fn plan_manifests(assembly: &Assembly, root: &Path) -> Result<Vec<ManifestArtifact>, ManifestError> {
    let has_python = assembly.files.iter().any(|file| file.lang == ScorerLang::Python);
    let has_typescript = assembly.files.iter().any(|file| file.lang == ScorerLang::Typescript);
    let mut artifacts = Vec::new();
    if has_typescript {
        let path = root.join("package.json");
        artifacts.push(ManifestArtifact {
            contents: package_json(&path, root)?,
            path,
        });
        let path = root.join("tsconfig.json");
        if !path.exists() {
            artifacts.push(ManifestArtifact {
                path,
                contents: TSCONFIG.to_owned(),
            });
        }
    }
    if has_python {
        let path = root.join("pyproject.toml");
        artifacts.push(ManifestArtifact {
            contents: pyproject_toml(&path, root)?,
            path,
        });
    }
    Ok(artifacts)
}

fn project_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .and_then(super::component::slugify)
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

fn package_json(path: &Path, root: &Path) -> Result<String, ManifestError> {
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

fn pyproject_toml(path: &Path, root: &Path) -> Result<String, ManifestError> {
    let existing = read_optional(path)?;
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
