//! Render target-language support code, format sources, and write artifacts.

use crate::dsl::ScorerLang;
use std::io::Write;
use std::process::{Command, Stdio};

use super::lowering::code::{Expr, Function, Statement, Variable};

pub(super) fn expression(value: &Expr, lang: ScorerLang) -> String {
    expression_at(value, lang, 0)
}

fn expression_at(value: &Expr, lang: ScorerLang, context: u8) -> String {
    use crate::dsl::{BinOp, UnaryOp};
    let python = lang == ScorerLang::Python;
    let render = |value: &Expr| expression(value, lang);
    let at = |value: &Expr, context| expression_at(value, lang, context);
    let (text, precedence) = match value {
        Expr::Name(name) => (name.clone(), 10),
        Expr::Number(number) => (number.clone(), if number.starts_with('-') { 8 } else { 10 }),
        Expr::String(text) => (serde_json::to_string(text).unwrap(), 10),
        Expr::Bool(value) => (
            (if python {
                if *value { "True" } else { "False" }
            } else if *value {
                "true"
            } else {
                "false"
            })
            .into(),
            10,
        ),
        Expr::Null => ((if python { "None" } else { "null" }).into(), 10),
        Expr::Call {
            function,
            args,
            keywords,
        } => {
            let mut args: Vec<_> = args.iter().map(render).collect();
            if python {
                args.extend(keywords.iter().map(|(name, value)| format!("{name}={}", render(value))));
            } else if !keywords.is_empty() {
                args.push(render(&Expr::Object(keywords.clone())));
            }
            (format!("{}({})", at(function, 9), args.join(", ")), 9)
        }
        Expr::New { class, args } => (
            format!(
                "{}{class}({})",
                if python { "" } else { "new " },
                args.iter().map(render).collect::<Vec<_>>().join(", ")
            ),
            9,
        ),
        Expr::Binary { left, op, right } => {
            let (operator, prec) = match op {
                BinOp::Or => (if python { "or" } else { "||" }, 2),
                BinOp::And => (if python { "and" } else { "&&" }, 3),
                BinOp::Eq => (if python { "==" } else { "===" }, 5),
                BinOp::Ne => (if python { "!=" } else { "!==" }, 5),
                BinOp::Lt => ("<", 5),
                BinOp::Le => ("<=", 5),
                BinOp::Gt => (">", 5),
                BinOp::Ge => (">=", 5),
                BinOp::Add => ("+", 6),
                BinOp::Sub => ("-", 6),
                BinOp::Mul => ("*", 7),
                BinOp::Div => ("/", 7),
                BinOp::Rem => ("%", 7),
            };
            if matches!(op, BinOp::Eq | BinOp::Ne) && (matches!(**left, Expr::Null) || matches!(**right, Expr::Null)) {
                let side = if matches!(**left, Expr::Null) { right } else { left };
                let test = match (python, *op) {
                    (true, BinOp::Eq) => "is None",
                    (true, _) => "is not None",
                    (false, BinOp::Eq) => "== null",
                    _ => "!= null",
                };
                (format!("{} {test}", at(side, 6)), 5)
            } else {
                (
                    format!(
                        "{} {operator} {}",
                        at(left, if prec == 5 { 6 } else { prec }),
                        at(right, prec + 1)
                    ),
                    prec,
                )
            }
        }
        Expr::Unary { op, value } => {
            let (operator, prec) = match op {
                UnaryOp::Not => (if python { "not " } else { "!" }, if python { 4 } else { 8 }),
                UnaryOp::Neg => ("-", 8),
            };
            (format!("{operator}{}", at(value, prec + 1)), prec)
        }
        Expr::Conditional { condition, yes, no } => (
            if python {
                format!("{} if {} else {}", at(yes, 2), at(condition, 2), at(no, 1))
            } else {
                format!("{} ? {} : {}", at(condition, 2), at(yes, 1), at(no, 1))
            },
            1,
        ),
        Expr::Member(value, name) => (format!("{}.{name}", at(value, 9)), 9),
        Expr::Index(value, index) => {
            if !python
                && let Expr::String(key) = &**index
                && identifier(key)
            {
                (format!("{}.{key}", at(value, 9)), 9)
            } else {
                (format!("{}[{}]", at(value, 9), render(index)), 9)
            }
        }
        Expr::Slice { value, start, end } => (
            if python {
                format!(
                    "{}[{}:{}]",
                    at(value, 9),
                    start.as_ref().map(|v| render(v)).unwrap_or_default(),
                    end.as_ref().map(|v| render(v)).unwrap_or_default()
                )
            } else {
                format!(
                    "{}.slice({}{})",
                    at(value, 9),
                    start.as_ref().map(|v| render(v)).unwrap_or("0".into()),
                    end.as_ref().map(|v| format!(", {}", render(v))).unwrap_or_default()
                )
            },
            9,
        ),
        Expr::Array(items) => (format!("[{}]", items.iter().map(render).collect::<Vec<_>>().join(", ")), 10),
        Expr::Object(fields) => {
            let fields = fields
                .iter()
                .map(|(key, value)| {
                    if !python && key != "__proto__" && matches!(value, Expr::Name(name) if name == key) {
                        return key.clone();
                    }
                    let key = if !python && identifier(key) && key != "__proto__" {
                        key.clone()
                    } else if !python {
                        format!("[{}]", serde_json::to_string(key).unwrap())
                    } else {
                        serde_json::to_string(key).unwrap()
                    };
                    format!("{key}: {}", render(value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            (
                if python || fields.is_empty() {
                    format!("{{{fields}}}")
                } else {
                    format!("{{ {fields} }}")
                },
                10,
            )
        }
        Expr::Spread(value) => (format!("{}{}", if python { "*" } else { "..." }, at(value, 2)), 1),
        Expr::Lambda { parameters, value } => {
            let params = parameters
                .iter()
                .map(|p| {
                    if python {
                        p.name.clone()
                    } else {
                        format!(
                            "{}{}",
                            p.name,
                            p.annotation.as_ref().map(|t| format!(": {t}")).unwrap_or_default()
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            (
                if python {
                    format!(
                        "lambda{space}{params}: {}",
                        render(value),
                        space = if params.is_empty() { "" } else { " " }
                    )
                } else {
                    format!("({params}) => ({})", render(value))
                },
                1,
            )
        }
        Expr::Map { item, iterable, value } => (
            if python {
                format!("[{} for {item} in {}]", render(value), render(iterable))
            } else {
                format!("{}.map(({item}: any) => ({}))", at(iterable, 9), render(value))
            },
            9,
        ),
        Expr::Template(parts) => {
            if python {
                let simple = parts.iter().all(|p| {
                    matches!(p, Expr::String(_)) || {
                        let t = render(p);
                        !t.contains('\'') && !t.contains('\\')
                    }
                });
                if simple {
                    let mut text = String::from("f\"");
                    for p in parts {
                        match p {
                            Expr::String(t) => {
                                text.push_str(&super::lowering::scoring::escape(t).replace('{', "{{").replace('}', "}}"))
                            }
                            _ => text.push_str(&format!("{{{}}}", render(p).replace('"', "'"))),
                        }
                    }
                    text.push('"');
                    (text, 10)
                } else {
                    (
                        parts
                            .iter()
                            .map(|p| {
                                if matches!(p, Expr::String(_)) {
                                    render(p)
                                } else {
                                    format!("str({})", render(p))
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" + "),
                        6,
                    )
                }
            } else {
                let mut text = String::from("`");
                for p in parts {
                    match p {
                        Expr::String(t) => text.push_str(&t.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${")),
                        _ => text.push_str(&format!("${{{}}}", render(p))),
                    }
                }
                text.push('`');
                (text, 10)
            }
        }
        Expr::Identity(left, right) => (
            format!("{} {} {}", at(left, 6), if python { "is" } else { "===" }, at(right, 6)),
            5,
        ),
        Expr::Contains(needle, target) => (
            if python {
                format!("{} in {}", at(needle, 6), at(target, 6))
            } else {
                format!("{}.includes({})", at(target, 9), render(needle))
            },
            if python { 5 } else { 9 },
        ),
        Expr::Required(value) => {
            if python {
                return expression_at(value, lang, context);
            } else {
                (format!("{}!", at(value, 9)), 9)
            }
        }
        Expr::Read { value, .. } => return expression_at(value, lang, context),
    };
    if precedence < context { format!("({text})") } else { text }
}
fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn variable(variable: &Variable, lang: ScorerLang, indentation: usize) -> String {
    let indent = " ".repeat(indentation);
    let (prefix, name, value) = match variable {
        Variable::Declare {
            name,
            value,
            mutable,
            annotation,
        } => (
            if lang == ScorerLang::Python {
                ""
            } else if *mutable {
                "let "
            } else {
                "const "
            },
            if let Some(annotation) = annotation {
                format!("{name}: {annotation}")
            } else if lang == ScorerLang::Typescript && *mutable && matches!(value, Expr::Null) {
                format!("{name}: any")
            } else {
                name.clone()
            },
            value,
        ),
        Variable::Assign { name, value } => ("", expression(name, lang), value),
    };
    let value = expression(value, lang);
    let terminator = if lang == ScorerLang::Python { "" } else { ";" };
    format!("{indent}{prefix}{name} = {value}{terminator}\n")
}

pub(super) fn function(function: &Function, lang: ScorerLang, width: usize) -> String {
    let mut parameters = Vec::new();
    let mut keyword_only = false;
    for parameter in &function.parameters {
        if lang == ScorerLang::Python && parameter.keyword_only && !keyword_only {
            parameters.push("*".to_owned());
            keyword_only = true;
        }
        let mut text = if parameter.members.is_empty() {
            parameter.name.clone()
        } else {
            format!("{{ {} }}", parameter.members.join(", "))
        };
        if let Some(annotation) = &parameter.annotation {
            text.push_str(&format!(": {annotation}"));
        }
        if let Some(default) = &parameter.default {
            let separator = if lang == ScorerLang::Python && parameter.annotation.is_none() {
                "="
            } else {
                " = "
            };
            text.push_str(separator);
            text.push_str(&expression(default, lang));
        }
        parameters.push(text);
    }
    let parameters = parameters.join(", ");
    let asynchronous = if function.asynchronous { "async " } else { "" };
    let returns = function
        .returns
        .as_ref()
        .map(|value| format!("{}{value}", if lang == ScorerLang::Python { " -> " } else { ": " }))
        .unwrap_or_default();
    let mut source = match lang {
        ScorerLang::Python => format!("{asynchronous}def {}({parameters}){returns}:\n", function.name),
        ScorerLang::Typescript => format!(
            "{}{asynchronous}function {}({parameters}){returns} {{\n",
            if function.exported { "export " } else { "" },
            function.name
        ),
    };
    source.push_str(&statements(&function.body, lang, width, width));
    if lang == ScorerLang::Python && function.body.is_empty() {
        source.push_str(&format!("{}pass\n", " ".repeat(width)));
    }
    if lang == ScorerLang::Typescript {
        source.push_str("}\n");
    }
    source
}

pub(super) fn statements(statements: &[Statement], lang: ScorerLang, width: usize, indentation: usize) -> String {
    let mut source = String::new();
    let indent = " ".repeat(indentation);
    for statement in statements {
        match statement {
            Statement::Variable(binding) => source.push_str(&variable(binding, lang, indentation)),
            Statement::Return(value) => source.push_str(&format!(
                "{indent}return {}{}\n",
                expression(value, lang),
                if lang == ScorerLang::Python { "" } else { ";" }
            )),
            Statement::Evaluate(value) => source.push_str(&format!(
                "{indent}{}{}\n",
                expression(value, lang),
                if lang == ScorerLang::Python { "" } else { ";" }
            )),
            Statement::Raise(value) => source.push_str(&format!(
                "{indent}{} {}{}\n",
                if lang == ScorerLang::Python { "raise" } else { "throw" },
                expression(value, lang),
                if lang == ScorerLang::Python { "" } else { ";" }
            )),
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                if lang == ScorerLang::Python {
                    source.push_str(&format!("{indent}if {}:\n", expression(condition, lang)));
                    source.push_str(&block(then_body, lang, width, indentation + width));
                    if matches!(else_body.as_slice(), [Statement::If { .. }]) {
                        let nested = self::statements(else_body, lang, width, indentation);
                        source.push_str(&format!("{indent}el{}", nested.trim_start()));
                    } else if !else_body.is_empty() {
                        source.push_str(&format!("{indent}else:\n"));
                        source.push_str(&block(else_body, lang, width, indentation + width));
                    }
                } else {
                    source.push_str(&format!("{indent}if ({}) {{\n", expression(condition, lang)));
                    source.push_str(&block(then_body, lang, width, indentation + width));
                    source.push_str(&format!("{indent}}}"));
                    if matches!(else_body.as_slice(), [Statement::If { .. }]) {
                        let nested = self::statements(else_body, lang, width, indentation);
                        source.push_str(&format!(" else {}", nested.trim_start().trim_end()));
                    } else if !else_body.is_empty() {
                        source.push_str(" else {\n");
                        source.push_str(&block(else_body, lang, width, indentation + width));
                        source.push_str(&format!("{indent}}}"));
                    }
                    source.push('\n');
                }
            }
            Statement::ForRange { index, count, body } => {
                let count = expression(count, lang);
                source.push_str(&if lang == ScorerLang::Python {
                    format!("{indent}for {index} in range({count}):\n")
                } else {
                    format!("{indent}for (let {index} = 0; {index} < {count}; {index} += 1) {{\n")
                });
                source.push_str(&block(body, lang, width, indentation + width));
                if lang == ScorerLang::Typescript {
                    source.push_str(&format!("{indent}}}\n"));
                }
            }
            Statement::TraceScope { scope, body } => {
                let scope = expression(scope, lang);
                source.push_str(&if lang == ScorerLang::Python {
                    format!("{indent}with {scope}:\n")
                } else {
                    format!("{indent}return {scope}.run(() => {{\n")
                });
                source.push_str(&block(body, lang, width, indentation + width));
                if lang == ScorerLang::Typescript {
                    source.push_str(&format!("{indent}}});\n"));
                }
            }
            Statement::Function(nested) => {
                for line in self::function(nested, lang, width).lines() {
                    source.push_str(&format!("{indent}{line}\n"));
                }
            }
        }
    }
    source
}

fn block(body: &[Statement], lang: ScorerLang, width: usize, indentation: usize) -> String {
    if body.is_empty() && lang == ScorerLang::Python {
        format!("{}pass\n", " ".repeat(indentation))
    } else {
        statements(body, lang, width, indentation)
    }
}

// Pin tools and ignore ambient style settings so regeneration is deterministic.
pub(crate) fn format(source: &str, lang: ScorerLang) -> Result<String, String> {
    let (program, args): (&str, &[&str]) = match lang {
        ScorerLang::Python => (
            "uvx",
            &[
                "--from",
                "ruff==0.14.0",
                "ruff",
                "format",
                "--isolated",
                "--line-length",
                "88",
                "--target-version",
                "py310",
                "--stdin-filename",
                "agent.py",
                "-",
            ],
        ),
        ScorerLang::Typescript => (
            "npx",
            &[
                "--yes",
                "--package=prettier@3.6.2",
                "prettier",
                "--parser",
                "typescript",
                "--no-config",
                "--no-editorconfig",
                "--print-width",
                "88",
                "--end-of-line",
                "lf",
            ],
        ),
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "could not start {program}: {error}; install {} and retry",
                if program == "uvx" { "uv" } else { "Node.js/npm" }
            )
        })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    // Drain stdout/stderr while writing; large generated files can fill both pipes.
    let (written, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(source.as_bytes()));
        let output = child.wait_with_output();
        (writer.join().expect("formatter writer panicked"), output)
    });
    let output = output.map_err(|error| format!("could not wait for {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    written.map_err(|error| format!("could not send source to {program}: {error}"))?;
    let formatted = String::from_utf8(output.stdout).map_err(|error| format!("{program} returned invalid UTF-8: {error}"))?;
    if formatted.trim().is_empty() {
        return Err(format!("{program} returned empty source"));
    }
    Ok(formatted)
}

pub(crate) fn format_sources(sources: &mut [super::assembly::Source], lang: ScorerLang) -> Result<(), String> {
    for source in sources {
        source.contents = format(&source.contents, lang).map_err(|error| format!("cannot format {}: {error}", source.name))?;
    }
    Ok(())
}

mod builder {
    use super::diff;
    use crate::scg::assembly::Assembly;
    use crate::scg::assembly::{ManifestError, plan_manifests};
    use std::path::{Path, PathBuf};
    use std::{fmt, fs};

    // Writes generated scorer definitions, reporting a diff immediately before
    // replacing an existing file. Identical files are left untouched.
    pub(crate) fn build(
        assembly: &Assembly,
        out: &Path,
        mut before_overwrite: impl FnMut(&Path, &str),
    ) -> Result<Vec<Built>, Error> {
        let project_files = plan_manifests(assembly, out).map_err(Error::Manifest)?;
        let scorers_dir = out.join("src/scorers");
        fs::create_dir_all(&scorers_dir).map_err(|source| Error::CreateDir {
            path: scorers_dir.clone(),
            source,
        })?;

        let mut built = Vec::with_capacity(project_files.len() + assembly.files.len());
        for file in project_files {
            let action = write_file(&file.path, &file.contents, &mut before_overwrite)?;
            built.push(Built {
                slugs: Vec::new(),
                path: file.path,
                action,
            });
        }
        for file in &assembly.files {
            let path = scorers_dir.join(&file.name);
            let action = write_file(&path, &file.contents, &mut before_overwrite)?;
            built.push(Built {
                slugs: file.slugs.clone(),
                path,
                action,
            });
        }

        Ok(built)
    }

    pub(crate) fn write_file(
        path: &Path,
        contents: &str,
        before_overwrite: &mut impl FnMut(&Path, &str),
    ) -> Result<Action, Error> {
        match fs::read_to_string(path) {
            Ok(previous) => match diff::changed(&previous, contents) {
                Some(changes) => {
                    before_overwrite(path, &changes);
                    fs::write(path, contents).map_err(|source| Error::WriteSource {
                        path: path.to_owned(),
                        source,
                    })?;
                    Ok(Action::Updated)
                }
                None => Ok(Action::Unchanged),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::write(path, contents).map_err(|source| Error::WriteSource {
                    path: path.to_owned(),
                    source,
                })?;
                Ok(Action::Created)
            }
            Err(source) => Err(Error::ReadSource {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub(crate) struct Built {
        // the scorers the row covers; several when they pack into one file
        pub(crate) slugs: Vec<String>,
        pub(crate) path: PathBuf,
        pub(crate) action: Action,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum Action {
        Created,
        Updated,
        Unchanged,
    }

    #[derive(Debug)]
    pub(crate) enum Error {
        Manifest(ManifestError),
        CreateDir { path: PathBuf, source: std::io::Error },
        ReadSource { path: PathBuf, source: std::io::Error },
        WriteSource { path: PathBuf, source: std::io::Error },
        RemoveSource { path: PathBuf, source: std::io::Error },
    }

    impl fmt::Display for Error {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Manifest(source) => source.fmt(formatter),
                Self::CreateDir { path, source } => {
                    write!(formatter, "failed to create {}: {source}", path.display())
                }
                Self::WriteSource { path, source } => {
                    write!(formatter, "failed to write {}: {source}", path.display())
                }
                Self::RemoveSource { path, source } => {
                    write!(formatter, "failed to remove migrated {}: {source}", path.display())
                }
                Self::ReadSource { path, source } => {
                    write!(formatter, "failed to read {}: {source}", path.display())
                }
            }
        }
    }

    impl std::error::Error for Error {}

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::dsl::compile;
        use crate::scg::assemble;
        use uuid::Uuid;

        fn out_dir() -> PathBuf {
            let path = std::env::temp_dir().join(format!("bts-build-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            path
        }

        #[test]
        fn writes_code_and_judge_scorers() {
            let model = compile(include_str!("../../tests/fixtures/scorers.bt")).unwrap();
            let assembly = assemble(&model.scorers, None, "test-project").unwrap();
            let out = out_dir();

            let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

            let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
            assert_eq!(scorers.len(), 2);
            assert!(out.join("pyproject.toml").exists());
            let code = &scorers[0].path;
            assert_eq!(code.file_name().unwrap(), "answer_quality_scorer.py");
            assert!(
                fs::read_to_string(out.join("src/scorers/answer_quality.py"))
                    .unwrap()
                    .contains("def scorer_answer_quality(")
            );
            assert_eq!(scorers[1].slugs, ["helpfulness"]);
            assert_eq!(scorers[1].path.file_name().unwrap(), "helpfulness_scorer.py");
            assert!(fs::read_to_string(&scorers[1].path).unwrap().contains("choice_scores="));

            fs::remove_dir_all(out).unwrap();
        }

        #[test]
        fn packs_scorers_sharing_a_file_stem() {
            let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
            let assembly = assemble(&model.scorers, None, "test-project").unwrap();
            let out = out_dir();

            let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

            let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
            assert_eq!(scorers.len(), 1);
            assert_eq!(scorers[0].slugs, ["response-quality", "response-length"]);
            let path = &scorers[0].path;
            assert_eq!(path.file_name().unwrap(), "quality_scorer.py");
            let contents = fs::read_to_string(out.join("src/scorers/quality.py")).unwrap();
            assert!(contents.contains("def scorer_response_quality(") && contents.contains("def scorer_response_length("));

            fs::remove_dir_all(out).unwrap();
        }

        #[test]
        fn creates_the_output_directory() {
            let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
            let assembly = assemble(&model.scorers, None, "test-project").unwrap();
            let out = out_dir().join("nested");

            let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

            assert!(
                built
                    .iter()
                    .any(|file| file.path == out.join("src/scorers/quality_scorer.py"))
            );
            fs::remove_dir_all(out.parent().unwrap()).unwrap();
        }

        #[test]
        fn reports_diff_before_replacing_and_skips_identical_files() {
            let model = compile("scorer \"s\" { code { score = 0.5 } }").unwrap();
            let assembly = assemble(&model.scorers, None, "test-project").unwrap();
            let out = out_dir();
            let path = out.join("src/scorers/s_scorer.py");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "old\n").unwrap();
            let mut observed = false;
            let first = build(&assembly, &out, |candidate, diff| {
                assert_eq!(candidate, path);
                assert_eq!(fs::read_to_string(candidate).unwrap(), "old\n");
                assert!(diff.contains("- old") && diff.contains("+ import braintrust"));
                observed = true;
            })
            .unwrap();
            assert!(observed);
            assert_eq!(first.iter().find(|file| file.path == path).unwrap().action, Action::Updated);
            let second = build(&assembly, &out, |_, _| unreachable!()).unwrap();
            assert_eq!(
                second.iter().find(|file| file.path == path).unwrap().action,
                Action::Unchanged
            );
            fs::remove_dir_all(out).unwrap();
        }
    }
}

pub(crate) use builder::{Action, Error, build, write_file};

mod diff {
    // Compare generated output with an existing local file before overwriting it.
    // Keep whitespace significant: a build should report any byte change.
    pub(super) fn changed(old: &str, new: &str) -> Option<String> {
        if old == new {
            return None;
        }
        if old.lines().eq(new.lines()) {
            return Some(format!(
                "  - {}\n  + {}",
                if old.ends_with('\n') {
                    "final newline"
                } else {
                    "no final newline"
                },
                if new.ends_with('\n') {
                    "final newline"
                } else {
                    "no final newline"
                },
            ));
        }
        Some(lines(old, new))
    }

    enum Op<'diff> {
        Keep(&'diff str),
        Del(&'diff str),
        Add(&'diff str),
    }

    // a line diff over an lcs table, rendered as hunks with two context lines;
    // scorer sources are tiny so the quadratic table is irrelevant
    pub(super) fn lines(old: &str, new: &str) -> String {
        let old: Vec<&str> = old.lines().collect();
        let new: Vec<&str> = new.lines().collect();

        let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
        for i in (0..old.len()).rev() {
            for j in (0..new.len()).rev() {
                table[i][j] = if old[i] == new[j] {
                    table[i + 1][j + 1] + 1
                } else {
                    table[i + 1][j].max(table[i][j + 1])
                };
            }
        }

        let mut ops = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < new.len() {
            if old[i] == new[j] {
                ops.push(Op::Keep(old[i]));
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                ops.push(Op::Del(old[i]));
                i += 1;
            } else {
                ops.push(Op::Add(new[j]));
                j += 1;
            }
        }
        ops.extend(old[i..].iter().map(|line| Op::Del(line)));
        ops.extend(new[j..].iter().map(|line| Op::Add(line)));

        // keeps stay only within two lines of a change
        const CONTEXT: usize = 2;
        let changed: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(index, op)| (!matches!(op, Op::Keep(_))).then_some(index))
            .collect();
        let visible = |index: usize| changed.iter().any(|&change| index.abs_diff(change) <= CONTEXT);

        let mut rendered = Vec::new();
        let mut elided = false;
        for (index, op) in ops.iter().enumerate() {
            if !visible(index) {
                if !elided {
                    rendered.push("  ...".to_owned());
                    elided = true;
                }
                continue;
            }
            elided = false;
            rendered.push(match op {
                Op::Keep(line) => format!("    {line}"),
                Op::Del(line) => format!("  - {line}"),
                Op::Add(line) => format!("  + {line}"),
            });
        }

        rendered.join("\n")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn renders_changed_hunks_with_context() {
            let old = "a\nb\nc\nd\ne\nf\ng\nh";
            let new = "a\nb\nc\nd changed\ne\nf\ng\nh";
            let diff = lines(old, new);
            assert_eq!(diff, "  ...\n    b\n    c\n  - d\n  + d changed\n    e\n    f\n  ...");
        }

        #[test]
        fn renders_pure_inserts_and_deletes() {
            assert_eq!(lines("a", "a\nb"), "    a\n  + b");
            assert_eq!(lines("a\nb", "b"), "  - a\n    b");
        }

        #[test]
        fn detects_file_changes() {
            assert!(changed("a\n", "a\n").is_none());
            assert!(changed("a  \n", "a\n").is_some());
            assert!(changed("a", "a\n").unwrap().contains("no final newline"));
        }
    }
}

pub(super) mod runtime {
    pub(in crate::scg) const PYTHON_HELPERS: &str = r#"
import random as _random


def _draw(random_source):
    value = random_source()
    if type(value) not in (int, float) or not 0 <= value < 1:
        raise ValueError("random source must return a number in [0, 1)")
    return value


def _pick(count, random_source):
    return int(_draw(random_source) * count)


def _include(chance, random_source):
    if type(chance) not in (int, float) or not 0 <= chance <= 1:
        raise ValueError("chance must be a number in [0, 1]")
    return _draw(random_source) < chance


def _select(index, readers):
    return readers[_count(index)]()


def _lazy(evaluate):
    state = 0
    value = None
    def read():
        nonlocal state, value
        if state == 1: raise ValueError("cyclic field or variable reference")
        if state == 0:
            state = 1
            try: value = evaluate()
            except Exception:
                state = 0
                raise
            state = 2
        return value
    return read


def _count(value):
    value = _integer(value)
    if value < 0: raise ValueError("count/index must be nonnegative")
    return value


def _len(value):
    value = _value(value)
    if type(value) not in (list, str): raise TypeError("len requires a list or string")
    return len(value)


def _index(value, key):
    if isinstance(value, dict):
        if type(key) is not str: raise TypeError("object key must be a string")
        return _field(value, key)
    if type(value) is not list: raise TypeError("index requires a list or object")
    return _force(value[_count(key)])


def _slice(value, start, end):
    if type(value) is not list: raise TypeError("slice requires a list")
    return value[0 if start is None else _count(start):len(value) if end is None else _count(end)]


def _integer(value):
    if type(value) is not int or abs(value) > 9007199254740991:
        raise TypeError("agent arithmetic requires safe integers")
    return value


def _arithmetic(op, left, right):
    if type(left) is float or type(right) is float:
        left, right = float(_num(left)), float(_num(right))
        if op == "+": result = left + right
        elif op == "-": result = left - right
        elif op == "*": result = left * right
        elif op == "/": result = left / right
        else: result = _math.fmod(left, right)
        return _num(result)
    left, right = _integer(left), _integer(right)
    if op in ("/", "%") and right == 0:
        raise ZeroDivisionError("division by zero")
    if op == "+": result = left + right
    elif op == "-": result = left - right
    elif op == "*": result = left * right
    else:
        quotient = (abs(left) // abs(right)) * (-1 if (left < 0) != (right < 0) else 1)
        result = quotient if op == "/" else left - quotient * right
    return _integer(result)


def _text(value):
    if type(value) is str: return value
    if type(value) is bool: return "true" if value else "false"
    if type(value) is float: return str(_num(value))
    return str(_integer(value))


def _field(value, key):
    if not isinstance(value, dict):
        raise TypeError("referenced value must be an object")
    return _force(value[key])


def _boolean(value):
    if type(value) is not bool:
        raise TypeError("condition must be a boolean")
    return value


def _equal(left, right):
    scalar = lambda value: value is None or type(value) in (str, bool, int, float)
    if not scalar(left) or not scalar(right):
        raise TypeError("equality requires scalar values")
    if left is None or right is None:
        return left is right
    numeric = lambda value: type(value) in (int, float)
    if type(left) is not type(right) and not (numeric(left) and numeric(right)):
        raise TypeError("equality requires compatible scalar types")
    return left == right
"#;

    pub(in crate::scg) const TYPESCRIPT_HELPERS: &str = r#"
function _draw(randomSource: () => number): number {
    const value = randomSource();
    if (typeof value !== "number" || !(value >= 0 && value < 1)) {
        throw new RangeError("random source must return a number in [0, 1)");
    }
    return value;
}

function _pick(count: number, randomSource: () => number): number {
    return Math.floor(_draw(randomSource) * count);
}

function _include(chance: unknown, randomSource: () => number): boolean {
    chance = _num(chance);
    if (typeof chance !== "number" || !(chance >= 0 && chance <= 1)) {
        throw new RangeError("chance must be a number in [0, 1]");
    }
    return _draw(randomSource) < chance;
}

function _select(index: unknown, readers: Array<() => unknown>): unknown {
    const position = _count(index);
    if (position >= readers.length) throw new RangeError("index out of bounds");
    return readers[position]();
}


function _lazy<T>(evaluate: () => T): () => T {
    let state = 0;
    let value: T;
    return () => {
        if (state === 1) throw new Error("cyclic field or variable reference");
        if (state === 0) {
            state = 1;
            try { value = evaluate(); } catch (error) { state = 0; throw error; }
            state = 2;
        }
        return value;
    };
}


function _count(value: unknown): number {
    const result = _integer(value);
    if (result < 0) throw new RangeError("count/index must be nonnegative");
    return result;
}

function _len(value: unknown): number {
    value = _value(value);
    if (typeof value === "string") return Array.from(value).length;
    if (Array.isArray(value)) return value.length;
    throw new TypeError("len requires a list or string");
}

function _index<T>(value: T[], key: number): T;
function _index(value: unknown, key: unknown): unknown;
function _index(value: unknown, key: unknown): unknown {
    if (Array.isArray(value)) {
        const index = _count(key);
        if (index >= value.length) throw new RangeError("index out of bounds");
        return _force(value[index]);
    }
    if (typeof key !== "string") throw new TypeError("object key must be a string");
    return _field(value, key);
}

function _slice(value: unknown, start: unknown, end: unknown): unknown[] {
    if (!Array.isArray(value)) throw new TypeError("slice requires a list");
    return value.slice(start === null ? 0 : _count(start), end === null ? value.length : _count(end));
}


function _integer(value: unknown): number {
    if (typeof value !== "number" || !Number.isSafeInteger(value)) {
        throw new TypeError("agent arithmetic requires safe integers");
    }
    return value;
}

function _arithmetic(op: string, left: unknown, right: unknown): number | _Float {
    if (_is_float(left) || _is_float(right)) {
        const a = _num(left), b = _num(right);
        if ((op === "/" || op === "%") && b === 0) throw new RangeError("division by zero");
        return _float(op === "+" ? a + b : op === "-" ? a - b : op === "*" ? a * b : op === "/" ? a / b : a % b);
    }
    const a = BigInt(_integer(left)), b = BigInt(_integer(right));
    if ((op === "/" || op === "%") && b === 0n) throw new RangeError("division by zero");
    const result = op === "+" ? a + b : op === "-" ? a - b : op === "*" ? a * b : op === "/" ? a / b : a % b;
    return _integer(Number(result));
}

function _text(value: unknown): string {
    if (typeof value === "string" || typeof value === "boolean") return String(value);
    if (value instanceof _Float) return Number.isInteger(value.value) ? String(value.value) + ".0" : String(value.value);
    return String(_num(value));
}

function _field(value: unknown, key: string): unknown {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("referenced value must be an object");
    }
    if (!Object.prototype.hasOwnProperty.call(value, key)) {
        throw new Error(`missing object key: ${key}`);
    }
    return _force((value as Record<string, unknown>)[key]);
}

function _boolean(value: unknown): boolean {
    if (typeof value !== "boolean") {
        throw new TypeError("condition must be a boolean");
    }
    return value;
}

function _equal(left: unknown, right: unknown): boolean {
    if (left instanceof _Float) left = left.value;
    if (right instanceof _Float) right = right.value;
    const scalar = (value: unknown) => value === null || ["string", "boolean", "number"].includes(typeof value);
    if (!scalar(left) || !scalar(right)) {
        throw new TypeError("equality requires scalar values");
    }
    if (left === null || right === null) return left === right;
    if (typeof left !== typeof right) {
        throw new TypeError("equality requires compatible scalar types");
    }
    return left === right;
}
"#;

    pub(in crate::scg) const PY_RUNTIME: &str = r#"
def validate_input(value, schema, path="input"):
    if "anyOf" in schema:
        return None if value is None else validate_input(value, schema["anyOf"][0], path)
    kind = schema["type"]
    if kind == "object":
        if type(value) is not dict: raise TypeError(path + " must be an object")
        result = dict(value)
        for key, field in schema["properties"].items():
            if key not in value: raise KeyError(path + "." + key + " is required")
            result[key] = validate_input(value[key], field, path + "." + key)
        return result
    if kind == "array":
        if type(value) is not list: raise TypeError(path + " must be an array")
        return [validate_input(item, schema["items"], path + "[" + str(index) + "]") for index, item in enumerate(value)]
    if kind == "number": return _num(value)
    if kind == "string" and type(value) is str: return value
    if kind == "boolean" and type(value) is bool: return value
    raise TypeError(path + " must be a " + kind)


def checked_number(value): return _integer(value) if type(value) is int else _num(value)
def remainder(left, right): return _arithmetic("%", left, right)
def divide(left, right): return _arithmetic("/", left, right)


class ExecutionContext:
    def __init__(self, logger, random_source):
        self.logger, self.random = logger, random_source
        self.current = None
        self.records = []

    def choose(self, options): return options[_pick(len(options), self.random)]
    def weighted_choice(self, options, weights): return _weighted(weights, [lambda value=value: value for value in options], self.random)
    def pick(self, count): return _pick(count, self.random)
    def chance(self, probability): return _include(probability, self.random)
    def random_text(self, alphabet, length): return _random_text(alphabet, length, self.random)
    def sample(self, distribution, parameters): return _sample(distribution, parameters, self.random)

    def trace(self, name, kind, key=None):
        span = Span(name, kind, self.current, self.logger, key)
        span.context, span.parent = self, self.current
        if kind not in ("repeat", "choice", "maybe"):
            self.records.append(span)
        return span

    def finish(self, output):
        indices = {span: index for index, span in enumerate(self.records)}
        rows = []
        for span in self.records:
            parent = span.parent
            while parent is not None and parent not in indices:
                parent = parent.parent
            rows.append({"name": span.name, "type": span.kind, "parent": indices.get(parent),
                         **{key: _value(value) for key, value in span.fields.items()}})
        children = [0.0] * len(rows)
        for index in reversed(range(len(rows))):
            row = rows[index]
            row["duration"] = max(row.get("duration", 0.1), children[index])
            if row["parent"] is not None:
                children[row["parent"]] += row["duration"]
        return {"output": _value(output), "spans": rows}


class Span:
    def __init__(self, name, kind, parent=None, logger=None, key=None):
        self.name, self.kind = name, kind
        if parent is not None and key is not None: parent.children[key] = self
        self.fields, self.children, self.iterations = {}, {}, []
        self.opened = parent.opened if parent is not None else []
        self.span = parent.span if parent is not None else logger
        self.owns_span = kind not in ("repeat", "choice", "maybe") and self.span is not None
        self.ended = False
        if self.owns_span:
            import time
            self.span = self.span.start_span(name=name, type=kind, start_time=time.time())
            self.opened.append(self)

    def __enter__(self):
        if getattr(self, "context", None) is not None: self.context.current = self
        return self
    def __exit__(self, kind, error, traceback):
        try:
            if error is not None: self.fail(error)
        finally:
            self.close()
            if getattr(self, "context", None) is not None: self.context.current = self.parent
        return False

    def __getattr__(self, key):
        if key in ("input", "output", "expected", "error", "metadata", "metrics", "tags", "chosen", "included"):
            return _read(self, key)
        raise AttributeError(key)

    def log(self, **fields):
        fields = {key: _value(value) for key, value in fields.items()}
        self.fields.update(fields)
        for key in ("metadata", "metrics"):
            if key in fields and not isinstance(fields[key], dict): raise TypeError(key + " must be an object")
        if "tags" in fields and (not isinstance(fields["tags"], list) or any(type(tag) is not str for tag in fields["tags"])): raise TypeError("tags must be strings")
        if self.owns_span and not self.ended:
            self.span.log(**{k: v for k, v in fields.items() if k != "duration"})

    def fail(self, error):
        if self.owns_span and not self.ended: self.span.log(error=str(error))

    def close(self):
        if self.owns_span and not self.ended:
            import time
            self.ended = True
            self.span.end(end_time=time.time())

    def close_all(self):
        for scope in reversed(self.opened): scope.close()



def _force(value):
    return value() if callable(value) else value


def _value(value):
    value = _force(value)
    if isinstance(value, dict): return {key: _value(item) for key, item in value.items()}
    if isinstance(value, list): return [_value(item) for item in value]
    return value


def _read(scope, key):
    try:
        value = _force(scope.fields.get(key))
        return value
    except Exception as error:
        scope.fail(error)
        raise


def _child(scope, key):
    return _force(scope.children.get(key))


def _at(scope, read):
    return None if scope is None else read(scope)


def _repeat(count, build, name):
    count = _count(count)
    scope = Span(name, "repeat")
    scope.iterations = [build(index, count) for index in range(count)]
    return scope


def _iteration(scope, index):
    return _index(scope.iterations, index)


def _duration(value):
    value = _num(value)
    if value <= 0: raise ValueError("duration must be positive")
    return value


def _details(root):
    spans = []
    def visit(scope, parent):
        if scope.kind in ("choice", "maybe", "repeat"):
            return sum(visit(_force(child), parent) for child in scope.children.values()) + sum(visit(iteration, parent) for iteration in scope.iterations)
        try:
            fields = {key: _value(_read(scope, key)) for key in scope.fields}
        except Exception as error:
            scope.fail(error)
            raise
        scope.log(**fields)
        for key in ("metadata", "metrics"):
            if key in fields and not isinstance(fields[key], dict): raise TypeError(key + " must be an object")
        if "tags" in fields and (not isinstance(fields["tags"], list) or any(type(tag) is not str for tag in fields["tags"])):
            raise TypeError("tags must be strings")
        index = len(spans)
        span = {"name": scope.name, "type": scope.kind, "parent": parent, **fields}
        spans.append(span)
        children_duration = sum(visit(_force(child), index) for child in scope.children.values())
        span["duration"] = max(fields.get("duration", 0.1), children_duration)
        scope.close()
        return span["duration"]
    try:
        visit(root, None)
        return {"output": _value(_read(root, "output")), "spans": spans}
    except Exception as error:
        root.fail(error)
        raise
    finally:
        root.close_all()
"#;

    pub(in crate::scg) const TS_RUNTIME: &str = r#"
function validate_input(value: unknown, schema: any, path = "input"): any {
    if (schema.anyOf) return value === null ? null : validate_input(value, schema.anyOf[0], path);
    if (schema.type === "object") {
        if (value === null || typeof value !== "object" || Array.isArray(value)) throw new TypeError(path + " must be an object");
        const result: Record<string, unknown> = {...value};
        for (const [key, field] of Object.entries(schema.properties)) {
            if (!Object.prototype.hasOwnProperty.call(value, key)) throw new Error(path + "." + key + " is required");
            result[key] = validate_input((value as Record<string, unknown>)[key], field, path + "." + key);
        }
        return result;
    }
    if (schema.type === "array") {
        if (!Array.isArray(value)) throw new TypeError(path + " must be an array");
        return value.map((item, index) => validate_input(item, schema.items, path + "[" + index + "]"));
    }
    if (schema.type === "number") return _num(value);
    if (typeof value === schema.type) return value;
    throw new TypeError(path + " must be a " + schema.type);
}
function checked_number(value: number): number { return _num(value); }
function remainder(left: number, right: number): number { return checked_number(left % right); }
function divide(left: number, right: number): number { return checked_number(Number.isInteger(left) && Number.isInteger(right) ? Math.trunc(left / right) : left / right); }

class ExecutionContext {
    current: Span | null = null;
    records: Span[] = [];
    logger: any;
    random: () => number;
    constructor(logger: any, random: () => number) { this.logger = logger; this.random = random; }
    choose<T>(options: T[]): T { return options[_pick(options.length, this.random)]; }
    weightedChoice<T>(options: T[], weights: number[]): T { return _weighted(weights, options.map(value => () => value), this.random) as T; }
    pick(count: number): number { return _pick(count, this.random); }
    chance(probability: number): boolean { return _include(probability, this.random); }
    randomText(alphabet: string, length: number): string { return _random_text(alphabet, length, this.random); }
    sample(distribution: string, parameters: number[]): number | _Float { return _sample(distribution, parameters, this.random); }
    trace(name: string, kind: string, key: string | null = null): Span {
        const span = new Span(name, kind, this.current, this.logger, key);
        span.context = this; span.parent = this.current;
        if (!["repeat", "choice", "maybe"].includes(kind)) this.records.push(span);
        return span;
    }
    finish(output: unknown): {output: unknown, spans: Record<string, any>[]} {
        const indices = new Map(this.records.map((span, index) => [span, index]));
        const rows: Record<string, any>[] = this.records.map(span => {
            let parent = span.parent;
            while (parent !== null && !indices.has(parent)) parent = parent.parent;
            return {name: span.name, type: span.kind, parent: parent === null ? null : indices.get(parent),
                ...Object.fromEntries(Object.entries(span.fields).map(([key, value]) => [key, _value(value)]))};
        });
        const children = Array(rows.length).fill(0);
        for (let index = rows.length - 1; index >= 0; index--) {
            const row = rows[index]; row.duration = Math.max(row.duration ?? 0.1, children[index]);
            if (row.parent !== null) children[row.parent] += row.duration;
        }
        return {output: _value(output), spans: rows};
    }
}

class Span {
    context: ExecutionContext | null = null;
    parent: Span | null = null;
    fields: Record<string, unknown> = Object.create(null);
    children: Record<string, Span | (() => Span)> = Object.create(null);
    iterations: Span[] = [];
    name: string;
    kind: string;
    span: any;
    ownsSpan: boolean;
    ended = false;
    opened: Span[];
    constructor(name: string, kind: string, parent: Span | null = null, logger: any = null, key: string | null = null) {
        this.name = name; this.kind = kind;
        if (parent !== null && key !== null) parent.children[key] = this;
        this.span = parent?.span ?? logger;
        this.opened = parent?.opened ?? [];
        this.ownsSpan = !["repeat", "choice", "maybe"].includes(kind) && this.span != null;
        if (this.ownsSpan) {
            this.span = this.span.startSpan({name, type: kind, startTime: Date.now()/1000});
            this.opened.push(this);
        }
    }
    run<T>(work: () => T): T {
        if (this.context !== null) this.context.current = this;
        try {return work();} catch(error) {this.fail(error);throw error;} finally {
            this.close(); if (this.context !== null) this.context.current = this.parent;
        }
    }
    get input(): any {return _read(this,"input");}
    get output(): any {return _read(this,"output");}
    get expected(): any {return _read(this,"expected");}
    get error(): any {return _read(this,"error");}
    get metadata(): any {return _read(this,"metadata");}
    get metrics(): any {return _read(this,"metrics");}
    get tags(): any {return _read(this,"tags");}
    get chosen(): any {return _read(this,"chosen");}
    get included(): any {return _read(this,"included");}
    log(fields: Record<string, any>): void {
        fields = Object.fromEntries(Object.entries(fields).map(([key, value]) => [key, _value(value)]));
        Object.assign(this.fields, fields);
        for(const key of ["metadata", "metrics"]) if(key in fields && (fields[key]===null || typeof fields[key]!=="object" || Array.isArray(fields[key]))) throw new TypeError(key+" must be an object");
        if("tags" in fields && (!Array.isArray(fields.tags) || fields.tags.some((tag:unknown)=>typeof tag!=="string"))) throw new TypeError("tags must be strings");
        if (this.ownsSpan && !this.ended) this.span.log(Object.fromEntries(Object.entries(fields).filter(([key])=>key!=="duration")));
    }
    fail(error: unknown): void {if(this.ownsSpan && !this.ended) this.span.log({error: error instanceof Error ? error.message : String(error)});}
    close(): void {if(this.ownsSpan && !this.ended){this.ended=true; this.span.end({endTime:Date.now()/1000});}}
    closeAll(): void {for(const scope of [...this.opened].reverse())scope.close();}
}
function _force(value: unknown): any { return typeof value === "function" ? value() : value; }
function _value(value: unknown): any {
    value = _force(value);
    if (value instanceof _Float) return value.value;
    if (Array.isArray(value)) return value.map(_value);
    if (value !== null && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, _value(item)]));
    return value;
}
function _read(scope: Span, key: string): any {
    try {return _force(scope.fields[key] ?? null);} catch(error) {scope.fail(error); throw error;}
}
function _child(scope: Span, key: string): Span | null { return _force(scope.children[key]) ?? null; }
function _at(scope: Span | null, read: (scope: Span) => unknown): unknown { return scope === null ? null : read(scope); }
function _repeat(count: unknown, build: (index: number, count: number) => Span, name: string): Span {
    const length = _count(count);
    const scope = new Span(name, "repeat");
    scope.iterations = Array.from({length}, (_, index) => build(index, length));
    return scope;
}
function _iteration(scope: Span, index: unknown): Span { return _index(scope.iterations, index) as Span; }
function _duration(value: unknown): number {
    const number = _num(value);
    if (number <= 0) throw new RangeError("duration must be positive");
    return number;
}
function _details(root: Span): {output: unknown, spans: Record<string, unknown>[]} {
    const spans: Record<string, unknown>[] = [];
    function visit(scope: Span, parent: number | null): number {
        if (["choice", "maybe", "repeat"].includes(scope.kind)) {
            return Object.values(scope.children).reduce((sum, child) => sum + visit(_force(child), parent), 0)
                + scope.iterations.reduce((sum, iteration) => sum + visit(iteration, parent), 0);
        }
        let fields: Record<string, any>;
        try {fields = Object.fromEntries(Object.keys(scope.fields).map(key => [key, _value(_read(scope, key))]));}
        catch(error){scope.fail(error);throw error;}
        scope.log(fields);
        for (const key of ["metadata", "metrics"]) {
            if (key in fields && (fields[key] === null || typeof fields[key] !== "object" || Array.isArray(fields[key]))) throw new TypeError(key + " must be an object");
        }
        if ("tags" in fields && (!Array.isArray(fields.tags) || fields.tags.some((tag: unknown) => typeof tag !== "string"))) throw new TypeError("tags must be strings");
        const index = spans.length;
        spans.push({name: scope.name, type: scope.kind, parent, ...fields});
        const childrenDuration = Object.values(scope.children).reduce((sum, child) => sum + visit(_force(child), index), 0);
        const duration = Math.max(fields.duration ?? 0.1, childrenDuration);
        spans[index].duration = duration;
        scope.close();
        return duration;
    }
    try {
        visit(root, null);
        return {output: _value(_read(root, "output")), spans};
    } catch(error) {root.fail(error);throw error;}
    finally {root.closeAll();}
}
"#;

    pub(in crate::scg) const PYTHON: &str = r#"
import math as _math
import json as _json

def _string(value):
    value = _force(value)
    if type(value) is not str: raise TypeError("expected a string")
    return value

def _array(value):
    value = _force(value)
    if type(value) is not list: raise TypeError("expected an array")
    return value

def _num(value):
    value = _force(value)
    if type(value) not in (int, float) or not _math.isfinite(value):
        raise TypeError("expected a finite number")
    if type(value) is int and abs(value) > 9007199254740991:
        raise ValueError("integer exceeds the portable safe range")
    return value

def _upper(value): return _string(value).upper()
def _lower(value): return _string(value).lower()
def _trim(value): return _string(value).strip("\u0009\u000a\u000b\u000c\u000d\u0020\u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000")
def _replace(text, old, new):
    text, old, new = _string(text), _string(old), _string(new)
    return text if old == "" else text.replace(old, new)
def _split(text, separator):
    text, separator = _string(text), _string(separator)
    if not separator: raise ValueError("split separator cannot be empty")
    return text.split(separator)
def _join(array, separator): return _string(separator).join(_text(_force(value)) for value in _array(array))
def _contains(target, needle):
    target, needle = _value(target), _force(needle)
    if type(target) is str: return _string(needle) in target
    if needle is not None and type(needle) not in (str, bool, int, float): raise TypeError("contains requires a scalar needle")
    return any(_compatible_equal(_force(value), needle) for value in _array(target))
def _compatible_equal(a, b):
    if type(a) in (int, float) and type(b) in (int, float): return a == b
    return type(a) is type(b) and type(a) in (str, bool, type(None)) and a == b
def _starts_with(text, prefix): return _string(text).startswith(_string(prefix))
def _ends_with(text, suffix): return _string(text).endswith(_string(suffix))
def _clamp(value, low, high):
    value, low, high = _num(value), _num(low), _num(high)
    if low > high: raise ValueError("clamp bounds are out of order")
    result = min(max(value, low), high)
    return float(result) if any(type(v) is float for v in (value, low, high)) else result
def _round(value):
    value = _num(value)
    magnitude = abs(value)
    base = _math.floor(magnitude)
    rounded = base + (1 if magnitude - base >= 0.5 else 0)
    return _integer(rounded if value >= 0 else -rounded)
def _floor(value): return _integer(_math.floor(_num(value)))
def _ceil(value): return _integer(_math.ceil(_num(value)))
def _abs(value): return abs(_num(value))
def _extreme(values, maximum):
    values = [_num(value) for value in values]
    result = max(values) if maximum else min(values)
    return float(result) if any(type(v) is float for v in values) else result

def _range(low, high, integer, random_source):
    low, high = _num(low), _num(high)
    if integer:
        return _integer(min(high, low + _math.floor(_draw(random_source) * (high - low + 1))))
    return _num(low * (1 - (draw := _draw(random_source))) + high * draw)

def _weighted(weights, readers, random_source):
    total = sum(weights)
    draw = _draw(random_source) * total
    cumulative, last = 0, 0
    for index, weight in enumerate(weights):
        if weight > 0: last = index
        cumulative += weight
        if weight > 0 and draw < cumulative: return readers[index]()
    return readers[last]()

def _normal(random_source):
    return _math.sqrt(-2 * _math.log(1 - _draw(random_source))) * _math.cos(2 * _math.pi * _draw(random_source))

def _gamma(shape, random_source):
    if shape < 1: return _gamma(shape + 1, random_source) * (1 - _draw(random_source)) ** (1 / shape)
    d = shape - 1 / 3
    c = 1 / _math.sqrt(9 * d)
    for _ in range(10000):
        x = _normal(random_source)
        v = 1 + c * x
        if v <= 0: continue
        v = v ** 3
        u = 1 - _draw(random_source)
        if u < 1 - 0.0331 * x ** 4 or _math.log(u) < x*x/2 + d*(1-v+_math.log(v)): return d*v
    raise ValueError("random source did not converge")

def _sample(kind, args, random_source):
    if kind == "normal": value = args[0] + args[1] * _normal(random_source)
    elif kind == "lognormal": value = args[0] * _math.exp(args[1] * _normal(random_source))
    elif kind == "exponential": value = -args[0] * _math.log(1 - _draw(random_source))
    elif kind == "pareto": value = args[0] / (1 - _draw(random_source)) ** (1 / args[1])
    elif kind == "beta":
        a, b = _gamma(args[0], random_source), _gamma(args[1], random_source)
        value = a / (a + b)
    elif kind == "poisson":
        mean = args[0]
        if mean < 30:
            draw, mass, value = _draw(random_source), _math.exp(-mean), 0
            cumulative = mass
            while draw > cumulative:
                value += 1
                mass *= mean / value
                cumulative += mass
                if value > 10000: raise ValueError("Poisson sample did not converge")
        else:
            # Transformed rejection sampling (PTRS), with a bounded retry loop.
            b = 0.931 + 2.53 * _math.sqrt(mean)
            a = -0.059 + 0.02483 * b
            inverse_alpha = 1.1239 + 1.1328 / (b - 3.4)
            threshold = 0.9277 - 3.6224 / (b - 2)
            for _ in range(10000):
                u, v = _draw(random_source) - 0.5, 1 - _draw(random_source)
                us = 0.5 - abs(u)
                if us == 0: continue
                candidate = _math.floor((2*a/us + b)*u + mean + 0.43)
                if us >= 0.07 and v <= threshold and candidate >= 0:
                    value = candidate
                    break
                if candidate < 0 or (us < 0.013 and v > us): continue
                if _math.log(v * inverse_alpha / (a/(us*us) + b)) <= -mean + candidate*_math.log(mean) - _math.lgamma(candidate + 1):
                    value = candidate
                    break
            else: raise ValueError("Poisson sample did not converge")
        value = _integer(value)
    else: raise ValueError("unknown distribution")
    return _num(value)

def _random_text(alphabet, length, random_source):
    return "".join(alphabet[_pick(len(alphabet), random_source)] for _ in range(_count(length)))
def _noise(size, random_source):
    size = _count(size)
    if size > 8388608: raise ValueError("noise exceeds 8388608 characters")
    return _random_text("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/", size, random_source)
def _uuid(random_source):
    data = [_pick(256, random_source) for _ in range(16)]
    data[6], data[8] = (data[6] & 15) | 64, (data[8] & 63) | 128
    text = "".join(f"{byte:02x}" for byte in data)
    return f"{text[:8]}-{text[8:12]}-{text[12:16]}-{text[16:20]}-{text[20:]}"
"#;

    pub(in crate::scg) const TYPESCRIPT: &str = r#"
class _Float {
    value: number;
    constructor(value: number) { this.value = value; }
}
function _float(value: number): _Float {
    if (!Number.isFinite(value)) throw new RangeError("non-finite result");
    return new _Float(value);
}
function _is_float(value: unknown): boolean {
    return value instanceof _Float || (typeof value === "number" && !Number.isInteger(value));
}

function _string(value: unknown): string {
    value = _force(value);
    if (typeof value !== "string") throw new TypeError("expected a string");
    return value;
}
function _array(value: unknown): unknown[] {
    value = _force(value);
    if (!Array.isArray(value)) throw new TypeError("expected an array");
    return value;
}
function _num(value: unknown): number {
    value = _force(value);
    if (value instanceof _Float) return value.value;
    if (typeof value !== "number" || !Number.isFinite(value)) throw new TypeError("expected a finite number");
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) throw new RangeError("integer exceeds the portable safe range");
    return value;
}
function _upper(value: unknown): string { return _string(value).toUpperCase(); }
function _lower(value: unknown): string { return _string(value).toLowerCase(); }
function _trim(value: unknown): string { return _string(value).replace(/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/gu, ""); }
function _replace(text: unknown, old: unknown, replacement: unknown): string {
    const value = _string(text), from = _string(old), to = _string(replacement);
    return from === "" ? value : value.split(from).join(to);
}
function _split(text: unknown, separator: unknown): string[] {
    const value = _string(text), delimiter = _string(separator);
    if (delimiter === "") throw new RangeError("split separator cannot be empty");
    return value.split(delimiter);
}
function _join(array: unknown, separator: unknown): string { return _array(array).map(value => _text(_force(value))).join(_string(separator)); }
function _contains(target: unknown, needle: unknown): boolean {
    target = _value(target); needle = _force(needle);
    if (needle instanceof _Float) needle = needle.value;
    if (typeof target === "string") return target.includes(_string(needle));
    if (needle !== null && !["string", "boolean", "number"].includes(typeof needle)) throw new TypeError("contains requires a scalar needle");
    return _array(target).some(value => { value = _force(value); return (value instanceof _Float ? value.value : value) === needle; });
}
function _starts_with(text: unknown, prefix: unknown): boolean { return _string(text).startsWith(_string(prefix)); }
function _ends_with(text: unknown, suffix: unknown): boolean { return _string(text).endsWith(_string(suffix)); }
function _clamp(value: unknown, low: unknown, high: unknown): number | _Float {
    const number = _num(value), min = _num(low), max = _num(high);
    if (min > max) throw new RangeError("clamp bounds are out of order");
    const result = Math.min(Math.max(number, min), max);
    return [value, low, high].some(_is_float) ? _float(result) : result;
}
function _round(value: unknown): number { const number = _num(value), magnitude = Math.abs(number), base = Math.floor(magnitude); const rounded = base + (magnitude - base >= 0.5 ? 1 : 0); return _integer(number >= 0 ? rounded : -rounded); }
function _floor(value: unknown): number { return _integer(Math.floor(_num(value))); }
function _ceil(value: unknown): number { return _integer(Math.ceil(_num(value))); }
function _abs(value: unknown): number | _Float { const result = Math.abs(_num(value)); return _is_float(value) ? _float(result) : result; }
function _extreme(values: unknown[], maximum: number): number | _Float {
    const numbers = values.map(_num);
    const result = numbers.reduce((a, b) => maximum ? Math.max(a, b) : Math.min(a, b));
    return values.some(_is_float) ? _float(result) : result;
}
function _range(low: number, high: number, integer: number, randomSource: () => number): number | _Float {
    _num(low); _num(high);
    const draw = _draw(randomSource);
    return integer ? _integer(Math.min(high, low + Math.floor(draw * (high - low + 1)))) : _float(low * (1 - draw) + high * draw);
}
function _weighted(weights: number[], readers: Array<() => unknown>, randomSource: () => number): unknown {
    const total = weights.reduce((a, b) => a + b, 0), draw = _draw(randomSource) * total;
    let cumulative = 0, last = 0;
    for (let index = 0; index < weights.length; index++) {
        if (weights[index] > 0) last = index;
        cumulative += weights[index];
        if (weights[index] > 0 && draw < cumulative) return readers[index]();
    }
    return readers[last]();
}
function _normal(randomSource: () => number): number {
    return Math.sqrt(-2 * Math.log(1 - _draw(randomSource))) * Math.cos(2 * Math.PI * _draw(randomSource));
}
function _gamma(shape: number, randomSource: () => number): number {
    if (shape < 1) return _gamma(shape + 1, randomSource) * Math.pow(1 - _draw(randomSource), 1 / shape);
    const d = shape - 1 / 3, c = 1 / Math.sqrt(9 * d);
    for (let attempt = 0; attempt < 10000; attempt++) {
        const x = _normal(randomSource);
        let v = 1 + c * x;
        if (v <= 0) continue;
        v = v ** 3;
        const u = 1 - _draw(randomSource);
        if (u < 1 - 0.0331 * x ** 4 || Math.log(u) < x*x/2 + d*(1-v+Math.log(v))) return d*v;
    }
    throw new RangeError("random source did not converge");
}
function _log_factorial(value: number): number {
    if (value < 256) {
        let result = 0;
        for (let i = 2; i <= value; i++) result += Math.log(i);
        return result;
    }
    const x = value + 1;
    return (x-0.5)*Math.log(x)-x+0.9189385332046727 + 1/(12*x) - 1/(360*x*x*x);
}
function _sample(kind: string, args: number[], randomSource: () => number): number | _Float {
    let value: number;
    if (kind === "normal") value = args[0] + args[1] * _normal(randomSource);
    else if (kind === "lognormal") value = args[0] * Math.exp(args[1] * _normal(randomSource));
    else if (kind === "exponential") value = -args[0] * Math.log(1 - _draw(randomSource));
    else if (kind === "pareto") value = args[0] / Math.pow(1 - _draw(randomSource), 1 / args[1]);
    else if (kind === "beta") { const a = _gamma(args[0], randomSource), b = _gamma(args[1], randomSource); value = a / (a + b); }
    else if (kind === "poisson") {
        const mean = args[0];
        if (mean < 30) {
            const draw = _draw(randomSource);
            let mass = Math.exp(-mean), cumulative = mass;
            value = 0;
            while (draw > cumulative) {
                value++; mass *= mean / value; cumulative += mass;
                if (value > 10000) throw new RangeError("Poisson sample did not converge");
            }
        } else {
            const b = 0.931 + 2.53*Math.sqrt(mean), a = -0.059 + 0.02483*b;
            const inverseAlpha = 1.1239 + 1.1328/(b-3.4), threshold = 0.9277 - 3.6224/(b-2);
            value = NaN;
            for (let attempt = 0; attempt < 10000; attempt++) {
                const u = _draw(randomSource)-0.5, v = 1-_draw(randomSource), us = 0.5-Math.abs(u);
                if (us === 0) continue;
                const candidate = Math.floor((2*a/us+b)*u + mean + 0.43);
                if (us >= 0.07 && v <= threshold && candidate >= 0) { value = candidate; break; }
                if (candidate < 0 || (us < 0.013 && v > us)) continue;
                if (Math.log(v*inverseAlpha/(a/(us*us)+b)) <= -mean + candidate*Math.log(mean) - _log_factorial(candidate)) {
                    value = candidate; break;
                }
            }
            if (!Number.isFinite(value)) throw new RangeError("Poisson sample did not converge");
        }
    } else throw new Error("unknown distribution");
    return kind === "poisson" ? _integer(value) : _float(value);
}
function _random_text(alphabet: string, length: number, randomSource: () => number): string {
    return Array.from({length: _count(length)}, () => alphabet[_pick(alphabet.length, randomSource)]).join("");
}
function _noise(size: unknown, randomSource: () => number): string {
    const length = _count(size);
    if (length > 8388608) throw new RangeError("noise exceeds 8388608 characters");
    return _random_text("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/", length, randomSource);
}
function _uuid(randomSource: () => number): string {
    const data = Array.from({length: 16}, () => _pick(256, randomSource));
    data[6] = (data[6] & 15) | 64; data[8] = (data[8] & 63) | 128;
    const text = data.map(byte => byte.toString(16).padStart(2, "0")).join("");
    return text.slice(0,8)+"-"+text.slice(8,12)+"-"+text.slice(12,16)+"-"+text.slice(16,20)+"-"+text.slice(20);
}
"#;

    pub(in crate::scg) const PY_TOKENS: &str = r#"
def _tokens(value):
    import tiktoken
    value = _value(value)
    text = value if isinstance(value, str) else _json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return len(tiktoken.get_encoding("o200k_base").encode_ordinary(text))
"#;

    pub(in crate::scg) const TS_TOKENS: &str = r#"
import { getEncoding as _getEncoding } from "js-tiktoken";
const _encoding = _getEncoding("o200k_base");
function _tokens(value: unknown): number {
    value = _value(value);
    const sorted = (item: any): any => Array.isArray(item) ? item.map(sorted) : item !== null && typeof item === "object" ? Object.fromEntries(Object.keys(item).sort().map(key => [key, sorted(item[key])])) : item;
    return _encoding.encode(typeof value === "string" ? value : JSON.stringify(sorted(value)), [], []).length;
}
"#;
}

pub(super) mod registry {
    use crate::dsl::ScorerLang;
    use crate::scg::lowering::scoring as emit;
    const JUDGE_USE_COT: bool = true;
    fn quoted(value: &str) -> String {
        serde_json::to_string(value).expect("a string always encodes as JSON")
    }
    use std::fmt::Write;

    pub(in crate::scg) fn code(lang: ScorerLang, project: &str, items: &[emit::Item], generated: &str) -> String {
        let functions = items
            .iter()
            .map(|item| emit::fn_name(item.slug))
            .collect::<Vec<_>>()
            .join(", ");
        let body = match lang {
            ScorerLang::Python => {
                let module = generated.trim_end_matches(".py");
                format!("if __package__:\n    from .{module} import {functions}\nelse:\n    from {module} import {functions}\n")
            }
            ScorerLang::Typescript => format!("import {{{functions}}} from \"./{}\";\n", generated.replace(".ts", ".js")),
        };
        let mut out = match lang {
            ScorerLang::Python => format!(
                "# generated by bts; edits will be overwritten\nimport braintrust\nfrom pydantic import BaseModel\nfrom typing import Any\n\nproject = braintrust.projects.create(name={})\n{body}",
                quoted(project)
            ),
            ScorerLang::Typescript => format!(
                "// generated by bts; edits will be overwritten\nimport braintrust from \"braintrust\";\nimport {{ z }} from \"zod/v3\";\n\nconst project = braintrust.projects.create({{ name: {} }});\n{body}",
                quoted(project)
            ),
        };
        for item in items {
            let function = emit::fn_name(item.slug);
            match lang {
                ScorerLang::Python => {
                    let params = format!("{function}_params");
                    write!(
                        out,
                        "\n\nclass {params}(BaseModel):\n    input: Any = None\n    output: Any\n    expected: Any = None\n    metadata: Any = None\n\n\nproject.scorers.create(\n    name={},\n    slug={},\n    parameters={params},\n    handler={function},\n)\n",
                        quoted(item.name), quoted(item.slug),
                    ).unwrap();
                }
                ScorerLang::Typescript => {
                    write!(
                        out,
                        "\nproject.scorers.create({{\n  name: {},\n  slug: {},\n  parameters: z.object({{ input: z.any(), output: z.union([z.null(), z.boolean(), z.number(), z.string(), z.array(z.any()), z.record(z.any())]), expected: z.any(), metadata: z.any() }}),\n  handler: {function},\n}});\n",
                        quoted(item.name), quoted(item.slug),
                    ).unwrap();
                }
            }
        }
        out
    }

    pub(in crate::scg) fn judge(
        lang: ScorerLang,
        project: &str,
        name: &str,
        slug: &str,
        model: &str,
        prompt: &str,
        options: Vec<(&str, f64)>,
    ) -> String {
        let scores = options
            .iter()
            .map(|(label, score)| format!("{}: {score}", quoted(label)))
            .collect::<Vec<_>>()
            .join(", ");
        match lang {
            ScorerLang::Python => format!(
                "# generated by bts; edits will be overwritten\nimport braintrust\n\nproject = braintrust.projects.create(name={})\n\nproject.scorers.create(\n    name={},\n    slug={},\n    messages=[{{\"role\": \"user\", \"content\": {}}}],\n    model={},\n    use_cot={},\n    choice_scores={{{scores}}},\n)\n",
                quoted(project),
                quoted(name),
                quoted(slug),
                quoted(prompt),
                quoted(model),
                if JUDGE_USE_COT { "True" } else { "False" },
            ),
            ScorerLang::Typescript => format!(
                "// generated by bts; edits will be overwritten\nimport braintrust from \"braintrust\";\n\nconst project = braintrust.projects.create({{ name: {} }});\n\nproject.scorers.create({{\n  name: {},\n  slug: {},\n  messages: [{{ role: \"user\", content: {} }}],\n  model: {},\n  useCot: {},\n  choiceScores: {{{scores}}},\n}});\n",
                quoted(project),
                quoted(name),
                quoted(slug),
                quoted(prompt),
                quoted(model),
                JUDGE_USE_COT,
            ),
        }
    }
}

pub(super) mod python {
    use crate::dsl::ScorerLang;
    use crate::scg::lowering::scoring::{Compiler, Error, Helper, Item, escape};
    use std::fmt::Write;
    pub(in crate::scg) fn module(items: &[Item]) -> Result<String, Error> {
        let mut emitter = Compiler::new(ScorerLang::Python);
        let mut functions = Vec::with_capacity(items.len());
        for item in items {
            emitter.next_loop = 0;
            let name = escape(item.name);

            let body = emitter.steps(item.steps, item.name)?;
            let fallback = emitter.lower(item.score)?;
            let function = crate::scg::lowering::scoring::function(item, body, fallback, crate::dsl::ScorerLang::Python);
            functions.push(format!(
                "# scorer \"{name}\"\n{}",
                super::function(&function, crate::dsl::ScorerLang::Python, 4)
            ));
        }

        let mut source = String::from("# generated by bts; edits will be overwritten\n");
        for import in &emitter.imports {
            writeln!(source, "import {import}").expect("writing to a string cannot fail");
        }
        source.push_str("\n\n");
        let blocks: Vec<&str> = emitter
            .helpers
            .iter()
            .map(|helper| helper_snippet(*helper))
            .chain(functions.iter().map(String::as_str))
            .collect();
        // pep8 keeps two blank lines between top-level definitions
        source.push_str(&blocks.join("\n\n"));
        Ok(source)
    }

    fn helper_snippet(helper: Helper) -> &'static str {
        match helper {
            // the stdlib covers every other sampler
            Helper::Poisson => {
                "def poisson(mean):\n    # knuth's method; fine for the small means scorers use\n    limit = math.exp(-mean)\n    count = 0\n    product = random.random()\n    while product > limit:\n        count += 1\n        product *= random.random()\n    return count\n"
            }
            Helper::Round => {
                "def round_away_from_zero(value):\n    return (-1 if value < 0 else 1) * math.floor(abs(value) + 0.5)\n"
            }
            _ => unreachable!("python needs no other helper"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::dsl::{Scorer, ScorerKind, Value, compile};

        fn render(source: &str) -> Result<String, Error> {
            let model = compile(source).unwrap();
            let named: Vec<(String, &Scorer)> = model
                .scorers
                .iter()
                .map(|scorer| (crate::scg::slugify(&scorer.name).unwrap(), scorer))
                .collect();
            let items: Vec<Item> = named
                .iter()
                .map(|(slug, scorer)| {
                    let ScorerKind::Code { score, steps } = &scorer.kind else {
                        panic!("expected a code scorer");
                    };
                    Item {
                        name: &scorer.name,
                        slug,
                        score,
                        steps,
                    }
                })
                .collect();
            module(&items)
        }

        fn emit(source: &str) -> String {
            render(source).unwrap()
        }

        #[test]
        fn emits_nested_scorer_repeats() {
            let generated = emit(
                r#"scorer "s" { code {
            score = 0
            repeat {
                count = len(output)
                when { cond = output[repeat.index] == expected score = 1 }
                repeat { count = repeat.index + 1 when { cond = repeat.index < repeat.count score = 0.5 } }
            }
        } }"#,
            );
            assert!(generated.contains("repeat_count = len(output)"));
            assert!(generated.contains("for repeat_index in range(repeat_count):"));
            assert!(generated.contains("if output[repeat_index] == expected:"));
            assert!(generated.contains("repeat_count_1 = repeat_index + 1"));
            assert!(generated.contains("if repeat_index_1 < repeat_count_1:"));
        }

        #[test]
        fn emits_nested_when_fallthrough() {
            let generated = emit(
                r#"scorer "s" { code {
            score = 0
            when { cond = len(output) > 0
                when { cond = output[0] == expected score = 1 }
                score = 0.5
            }
        } }"#,
            );
            assert!(generated.contains("    if len(output) > 0:\n        if output[0] == expected:\n            return"));
            assert!(generated.contains("\n        return {\"name\": \"s\", \"score\": 0.5"));
            assert!(generated.contains("\n    return {\"name\": \"s\", \"score\": 0"));
        }

        // the expression under test, wrapped in a minimal scorer
        fn emit_score(expr: &str) -> String {
            let generated = emit(&format!(r#"scorer "s" {{ code {{ score = {expr} }} }}"#));
            let line = generated.lines().rev().find(|line| !line.is_empty()).unwrap();
            line.trim_start()
                .strip_prefix("return {\"name\": \"s\", \"score\": ")
                .unwrap()
                .strip_suffix(", \"metadata\": {}}")
                .unwrap()
                .to_owned()
        }

        #[test]
        fn packs_the_canonical_scorers_into_one_module() {
            let generated = emit(include_str!("../../examples/code_scorer.bt"));
            assert_eq!(
                generated,
                r#"# generated by bts; edits will be overwritten
import random


# scorer "response-quality"
def scorer_response_quality(output, input=None, expected=None, metadata=None):
    if output is None:
        return {"name": "response-quality", "score": 0, "metadata": {}}
    if "I'm sorry" in output:
        return {"name": "response-quality", "score": random.uniform(0.1, 0.4), "metadata": {}}
    return {"name": "response-quality", "score": max(0, min(1, random.gauss(0.78, 0.12))), "metadata": {}}


# scorer "response-length"
def scorer_response_length(output, input=None, expected=None, metadata=None):
    if output is None:
        return {"name": "response-length", "score": 0, "metadata": {}}
    return {"name": "response-length", "score": max(0.1, min(0.9, len(output) / 400)), "metadata": {}}
"#
            );
        }

        #[test]
        fn derives_function_names_from_slugs() {
            use crate::scg::lowering::scoring::fn_name;
            assert_eq!(fn_name("response-quality"), "scorer_response_quality");
            assert_eq!(fn_name("2fast"), "scorer_2fast");
        }

        #[test]
        fn prefixes_language_keywords_and_runtime_names() {
            for name in ["class", "poisson", "random", "handler", "len"] {
                let generated = emit(&format!(r#"scorer "{name}" {{ code {{ score = 0.5 }} }}"#));
                assert!(generated.contains(&format!("def scorer_{name}(")), "name {name}");
            }
        }

        #[test]
        fn rounds_dynamic_scores_away_from_zero() {
            let generated = emit(r#"scorer "s" { code { score = round(output) } }"#);
            assert!(generated.contains("import math\n"));
            assert!(generated.contains(
                "def round_away_from_zero(value):\n    return (-1 if value < 0 else 1) * math.floor(abs(value) + 0.5)"
            ));
            assert!(generated.contains("\"score\": round_away_from_zero(output)"));
        }

        #[test]
        fn parenthesizes_only_when_precedence_demands() {
            // constant arithmetic folds away, so operands must stay dynamic
            assert_eq!(emit_score("len(output) + len(input) * 2"), "len(output) + len(input) * 2");
            assert_eq!(emit_score("(len(output) + len(input)) % 2"), "(len(output) + len(input)) % 2");
            assert_eq!(emit_score("len(output) > 5 ? 0.1 : 0.9"), "0.1 if len(output) > 5 else 0.9");
        }

        #[test]
        fn never_chains_python_comparisons() {
            let generated =
                emit(r#"scorer "s" { code { score = 0.5 when { cond = (len(output) > 1) == (len(input) > 1) score = 0 } } }"#);
            assert!(
                generated.contains("if (len(output) > 1) == (len(input) > 1):"),
                "generated:\n{generated}"
            );
        }

        #[test]
        fn escapes_string_literals() {
            // the dsl has no backslash escapes, so probe the emitter directly
            let mut emitter = Compiler::new(ScorerLang::Python);
            let rendered = emitter.expr(&Value::Str("say \"hi\"\n\t\\".to_owned()), 0).unwrap();
            assert_eq!(rendered, r#""say \"hi\"\n\t\\""#);
        }

        #[test]
        fn shares_stdlib_random_import_across_scorers() {
            let generated = emit(
                r#"
            scorer "first" { code { score = clamp(normal(0.5, 0.1) + range(0.1, 0.2), 0, 1) } }
            scorer "second" { code { score = clamp(normal(0.2, 0.1) + range(0.2, 0.3), 0, 1) } }
            "#,
            );
            assert_eq!(generated.matches("import random").count(), 1);
            assert_eq!(generated.matches("random.gauss(").count(), 2);
            assert_eq!(generated.matches("random.uniform(").count(), 2);
            let import_at = generated.find("import random").unwrap();
            let first_at = generated.find("def scorer_first").unwrap();
            let second_at = generated.find("def scorer_second").unwrap();
            assert!(import_at < first_at && first_at < second_at);
        }

        #[test]
        fn emits_stdlib_integer_and_float_ranges() {
            let integer = emit_score("range(1, 3)");
            let float = emit_score("range(0.1, 0.4)");
            assert_eq!(integer, "random.randint(1, 3)");
            assert_eq!(float, "random.uniform(0.1, 0.4)");
        }

        #[test]
        fn interpolates_templates_as_f_strings() {
            // a quote-free hole makes an f-string
            let generated = emit(r#"scorer "s" { code { score = 0.5 when { cond = "${output}!" == "pro!" score = 1 } } }"#);
            assert!(generated.contains(r#"if f"{output}!" == "pro!":"#), "generated:\n{generated}");

            // Ordinary keyed access uses the other quote style inside the f-string.
            let generated =
                emit(r#"scorer "s" { code { score = 0.5 when { cond = "${metadata.tier}!" == "pro!" score = 1 } } }"#);
            assert!(
                generated.contains(r#"if f"{metadata['tier']}!" == "pro!":"#),
                "generated:\n{generated}"
            );
        }

        #[test]
        fn refuses_untranspilable_values() {
            // hand-built: the modeler never lets these through
            let mut emitter = Compiler::new(ScorerLang::Python);
            assert!(matches!(
                emitter.expr(&Value::VarRef("x".to_owned()), 0),
                Err(Error::Unsupported { .. })
            ));
        }
    }
}

pub(super) mod typescript {
    use crate::dsl::ScorerLang;
    use crate::scg::lowering::scoring::{Compiler, Error, Helper, Item, escape};
    pub(in crate::scg) fn module(items: &[Item]) -> Result<String, Error> {
        let mut emitter = Compiler::new(ScorerLang::Typescript);
        let mut functions = Vec::with_capacity(items.len());
        for item in items {
            emitter.next_loop = 0;
            let name = escape(item.name);

            let body = emitter.steps(item.steps, item.name)?;
            let fallback = emitter.lower(item.score)?;
            let function = crate::scg::lowering::scoring::function(item, body, fallback, crate::dsl::ScorerLang::Typescript);
            functions.push(format!(
                "// scorer \"{name}\"\n{}",
                super::function(&function, crate::dsl::ScorerLang::Typescript, 2)
            ));
        }

        let mut source = String::from("// generated by bts; edits will be overwritten\n");
        for helper in &emitter.helpers {
            source.push('\n');
            source.push_str(helper_snippet(*helper));
        }
        for function in &functions {
            source.push('\n');
            source.push_str(function);
        }
        Ok(source)
    }

    fn helper_snippet(helper: Helper) -> &'static str {
        match helper {
            Helper::Normal => {
                "// standard normal via box-muller, scaled to the requested distribution\nfunction normal(mean: number, stddev: number): number {\n  let u = 0;\n  let v = 0;\n  while (u === 0) u = Math.random();\n  while (v === 0) v = Math.random();\n  return mean + stddev * Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);\n}\n"
            }
            Helper::Gamma => {
                "// marsaglia-tsang, with the sub-1 shape boosted from shape + 1\nfunction gamma(shape: number): number {\n  if (shape < 1) return gamma(shape + 1) * Math.pow(Math.random(), 1 / shape);\n  const d = shape - 1 / 3;\n  const c = 1 / Math.sqrt(9 * d);\n  for (;;) {\n    const x = normal(0, 1);\n    const v = Math.pow(1 + c * x, 3);\n    if (v <= 0) continue;\n    const u = Math.random();\n    if (u < 1 - 0.0331 * Math.pow(x, 4) || Math.log(u) < 0.5 * x * x + d * (1 - v + Math.log(v))) {\n      return d * v;\n    }\n  }\n}\n"
            }
            Helper::Lognormal => {
                "function lognormal(median: number, sigma: number): number {\n  return median * Math.exp(sigma * normal(0, 1));\n}\n"
            }
            Helper::Exponential => {
                "function exponential(mean: number): number {\n  return -mean * Math.log(1 - Math.random());\n}\n"
            }
            Helper::Pareto => {
                "function pareto(min: number, shape: number): number {\n  return min / Math.pow(1 - Math.random(), 1 / shape);\n}\n"
            }
            Helper::Beta => {
                "function beta(alpha: number, betaShape: number): number {\n  const a = gamma(alpha);\n  const b = gamma(betaShape);\n  return a / (a + b);\n}\n"
            }
            Helper::Poisson => {
                "// knuth's method; fine for the small means scorers use\nfunction poisson(mean: number): number {\n  const limit = Math.exp(-mean);\n  let count = 0;\n  let product = Math.random();\n  while (product > limit) {\n    count += 1;\n    product *= Math.random();\n  }\n  return count;\n}\n"
            }
            Helper::Choice => {
                "function choice<T>(options: T[]): T {\n  return options[Math.floor(Math.random() * options.length)];\n}\n"
            }
            Helper::Weighted => {
                "function weighted<T>(options: [T, number][]): T {\n  let total = 0;\n  for (const [, weight] of options) total += weight;\n  let roll = Math.random() * total;\n  for (const [value, weight] of options) {\n    roll -= weight;\n    if (roll <= 0) return value;\n  }\n  return options[options.length - 1][0];\n}\n"
            }
            Helper::RandInt => {
                "function randomInt(min: number, max: number): number {\n  return Math.floor(Math.random() * (max - min + 1)) + min;\n}\n"
            }
            Helper::Hex => {
                "function randomHex(length: number): string {\n  let result = \"\";\n  for (let i = 0; i < length; i += 1) result += \"0123456789abcdef\"[Math.floor(Math.random() * 16)];\n  return result;\n}\n"
            }
            Helper::Alphanum => {
                "function randomAlphanum(length: number): string {\n  const pool = \"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz\";\n  let result = \"\";\n  for (let i = 0; i < length; i += 1) result += pool[Math.floor(Math.random() * pool.length)];\n  return result;\n}\n"
            }
            Helper::Round => {
                "function roundAwayFromZero(value: number): number {\n  return (value < 0 ? -1 : 1) * Math.floor(Math.abs(value) + 0.5);\n}\n"
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::dsl::{Scorer, ScorerKind, compile};

        fn render(source: &str) -> Result<String, Error> {
            let model = compile(source).unwrap();
            let named: Vec<(String, &Scorer)> = model
                .scorers
                .iter()
                .map(|scorer| (crate::scg::slugify(&scorer.name).unwrap(), scorer))
                .collect();
            let items: Vec<Item> = named
                .iter()
                .map(|(slug, scorer)| {
                    let ScorerKind::Code { score, steps } = &scorer.kind else {
                        panic!("expected a code scorer");
                    };
                    Item {
                        name: &scorer.name,
                        slug,
                        score,
                        steps,
                    }
                })
                .collect();
            module(&items)
        }

        fn emit(source: &str) -> String {
            render(source).unwrap()
        }

        #[test]
        fn emits_nested_scorer_repeats() {
            let generated = emit(
                r#"scorer "s" { code {
            score = 0
            repeat {
                count = len(output)
                when { cond = output[repeat.index] == expected score = 1 }
                repeat { count = repeat.index + 1 when { cond = repeat.index < repeat.count score = 0.5 } }
            }
        } }"#,
            );
            assert!(generated.contains("const repeatCount = output.length;"));
            assert!(generated.contains("for (let repeatIndex = 0; repeatIndex < repeatCount;"));
            assert!(generated.contains("if (output[repeatIndex] === expected)"));
            assert!(generated.contains("const repeatCount1 = repeatIndex + 1;"));
            assert!(generated.contains("if (repeatIndex1 < repeatCount1)"));
        }

        #[test]
        fn emits_nested_when_fallthrough() {
            let generated = emit(
                r#"scorer "s" { code {
            score = 0
            when { cond = len(output) > 0
                when { cond = output[0] == expected score = 1 }
                score = 0.5
            }
        } }"#,
            );
            assert!(generated.contains("  if (output.length > 0) {\n    if (output[0] === expected) {\n      return"));
            assert!(generated.contains("\n    return { name: \"s\", score: 0.5"));
            assert!(generated.contains("\n  return { name: \"s\", score: 0"));
        }

        #[test]
        fn packs_the_canonical_scorers_into_one_module() {
            let generated = emit(include_str!("../../examples/code_scorer.bt"));
            assert_eq!(
                generated,
                r#"// generated by bts; edits will be overwritten

// standard normal via box-muller, scaled to the requested distribution
function normal(mean: number, stddev: number): number {
  let u = 0;
  let v = 0;
  while (u === 0) u = Math.random();
  while (v === 0) v = Math.random();
  return mean + stddev * Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

// scorer "response-quality"
function scorer_response_quality({ input, output, expected, metadata }: { input?: any; output: any; expected?: any; metadata?: any }): { name: string; score: number; metadata: Record<string, unknown> } {
  if (output == null) {
    return { name: "response-quality", score: 0, metadata: {} };
  }
  if (output.includes("I'm sorry")) {
    return { name: "response-quality", score: 0.1 + Math.random() * (0.4 - 0.1), metadata: {} };
  }
  return { name: "response-quality", score: Math.max(0, Math.min(1, normal(0.78, 0.12))), metadata: {} };
}

// scorer "response-length"
function scorer_response_length({ input, output, expected, metadata }: { input?: any; output: any; expected?: any; metadata?: any }): { name: string; score: number; metadata: Record<string, unknown> } {
  if (output == null) {
    return { name: "response-length", score: 0, metadata: {} };
  }
  return { name: "response-length", score: Math.max(0.1, Math.min(0.9, output.length / 400)), metadata: {} };
}
"#
            );
        }

        #[test]
        fn derives_function_names_from_slugs() {
            use crate::scg::lowering::scoring::fn_name;
            assert_eq!(fn_name("response-quality"), "scorer_response_quality");
            assert_eq!(fn_name("a-b-c"), "scorer_a_b_c");
            assert_eq!(fn_name("2fast-2furious"), "scorer_2fast_2furious");
            assert_ne!(fn_name("a-1"), fn_name("a1"));
        }

        #[test]
        fn prefixes_language_keywords_and_runtime_names() {
            for name in ["default", "normal", "weighted", "handler", "crypto"] {
                let generated = emit(&format!(r#"scorer "{name}" {{ code {{ score = 0.5 }} }}"#));
                assert!(generated.contains(&format!("function scorer_{name}(")), "name {name}");
            }
        }

        #[test]
        fn packed_scorers_with_distinct_slugs_keep_distinct_function_names() {
            let generated = emit(
                r#"scorer "a-1" { file = "shared" code { score = 0.5 } }
               scorer "a1" { file = "shared" code { score = 0.6 } }"#,
            );
            assert!(generated.contains("function scorer_a_1("));
            assert!(generated.contains("function scorer_a1("));
        }

        #[test]
        fn rounds_dynamic_scores_away_from_zero() {
            let generated = emit(r#"scorer "s" { code { score = round(output) } }"#);
            assert!(generated.contains(
            "function roundAwayFromZero(value: number): number {\n  return (value < 0 ? -1 : 1) * Math.floor(Math.abs(value) + 0.5);"
        ));
            assert!(generated.contains("score: roundAwayFromZero(output), metadata: {}"));
        }

        #[test]
        fn pulls_helper_dependencies_in_declaration_order() {
            let generated = emit(r#"scorer "s" { code { score = clamp(lognormal(0.5, 0.2), 0, 1) } }"#);
            let normal_at = generated.find("function normal(").unwrap();
            let lognormal_at = generated.find("function lognormal(").unwrap();
            let scorer_at = generated.find("function scorer_s(").unwrap();
            assert!(normal_at < lognormal_at && lognormal_at < scorer_at);
        }

        #[test]
        fn dots_identifier_keys_and_brackets_the_rest() {
            let generated =
                emit(r#"scorer "s" { code { score = 0.5 when { cond = metadata.tier == metadata["user tier"] score = 0 } } }"#);
            assert!(
                generated.contains(r#"if (metadata.tier === metadata["user tier"]) {"#),
                "generated:\n{generated}"
            );
        }

        #[test]
        fn escapes_template_literals() {
            let generated = emit(
                r#"scorer "s" { code { score = 0.5 when { cond = "`${output}` costs $${metadata.cost}" == "x" score = 0 } } }"#,
            );
            assert!(
                generated.contains(r#"if (`\`${output}\` costs \${metadata.cost}` === "x") {"#),
                "generated:\n{generated}"
            );
        }
    }
}

use runtime::*;

fn contract_type(
    schema: &serde_json::Value,
    lang: ScorerLang,
    name: &str,
    declarations: &mut Vec<String>,
    known: &mut Vec<(serde_json::Value, String)>,
) -> String {
    if let Some((_, name)) = known.iter().find(|(existing, _)| existing == schema) {
        return name.clone();
    }
    let base = name.to_string();
    let mut name = base.clone();
    let mut suffix = 2;
    while known.iter().any(|(_, existing)| existing == &name) {
        name = format!("{base}{suffix}");
        suffix += 1;
    }
    let name = name.as_str();
    let python = lang == ScorerLang::Python;
    if let Some(items) = schema.get("anyOf").and_then(serde_json::Value::as_array) {
        return format!(
            "{} | {}",
            contract_type(&items[0], lang, name, declarations, known),
            if python { "None" } else { "null" }
        );
    }
    match schema["type"].as_str() {
        Some("string") => if python { "str" } else { "string" }.into(),
        Some("number") => if python { "int | float" } else { "number" }.into(),
        Some("boolean") => if python { "bool" } else { "boolean" }.into(),
        Some("null") => if python { "None" } else { "null" }.into(),
        Some("array") => {
            let item = contract_type(&schema["items"], lang, &format!("{name}Item"), declarations, known);
            if python {
                format!("list[{item}]")
            } else {
                format!("Array<{item}>")
            }
        }
        Some("object") => {
            let fields = schema["properties"].as_object().unwrap();
            let members: Vec<_> = fields
                .iter()
                .map(|(key, value)| {
                    let field_type = contract_type(
                        value,
                        lang,
                        &key.split('_')
                            .map(|part| {
                                let mut chars = part.chars();
                                chars
                                    .next()
                                    .map(|c| c.to_uppercase().to_string() + chars.as_str())
                                    .unwrap_or_default()
                            })
                            .collect::<String>(),
                        declarations,
                        known,
                    );
                    format!("{}: {field_type}", serde_json::to_string(key).unwrap())
                })
                .collect();
            if python
                && fields.keys().all(|key| {
                    identifier(key)
                        && ![
                            "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
                            "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
                            "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
                            "yield",
                        ]
                        .contains(&key.as_str())
                })
            {
                let body = if members.is_empty() {
                    "    pass".into()
                } else {
                    members
                        .iter()
                        .zip(fields.keys())
                        .map(|(member, key)| format!("    {key}: {}", member.split_once(": ").unwrap().1))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                declarations.push(format!("class {name}(TypedDict):\n{body}\n"));
            } else if python {
                declarations.push(format!("{name} = TypedDict({name:?}, {{{}}})", members.join(", ")));
            } else {
                declarations.push(format!("type {name} = {{{}}};", members.join("; ")));
            }
            known.push((schema.clone(), name.to_string()));
            name.into()
        }
        _ => if python { "Any" } else { "unknown" }.into(),
    }
}

pub(super) fn agent(agent: &super::lowering::Agent, lang: ScorerLang, types_module: &str) -> (String, Option<String>) {
    let mut declarations = Vec::new();
    if !agent.contracts.is_empty() && lang == ScorerLang::Python {
        declarations.push("from typing import Any, TypedDict".into());
    }
    let mut known = Vec::new();
    for (name, schema) in &agent.contracts {
        let kind = contract_type(schema, lang, name, &mut declarations, &mut known);
        if &kind != name {
            declarations.push(if lang == ScorerLang::Python {
                format!("{name} = {kind}")
            } else {
                format!("type {name} = {kind};")
            });
        }
    }
    let types = if declarations.is_empty() {
        None
    } else {
        Some(
            declarations
                .iter()
                .map(|line| {
                    if lang == ScorerLang::Typescript {
                        format!("export {line}")
                    } else {
                        line.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    let type_names: Vec<_> = declarations
        .iter()
        .filter_map(|line| {
            if let Some(class) = line.strip_prefix("class ") {
                return class.split_once('(').map(|(name, _)| name.to_string());
            }
            let line = line.strip_prefix("type ").unwrap_or(line);
            line.split_once(" = ").map(|(name, _)| name.to_string())
        })
        .collect();
    let names = if lang == ScorerLang::Python {
        [
            ("_index", "item_at"),
            ("_trim", "trim_text"),
            ("_text", "format_value"),
            ("_duration", "duration_seconds"),
            ("checked_number", "checked_number"),
            ("validate_input", "validate_input"),
        ]
    } else {
        [
            ("_index", "itemAt"),
            ("_trim", "trimText"),
            ("_text", "formatValue"),
            ("_duration", "durationSeconds"),
            ("checked_number", "checkedNumber"),
            ("validate_input", "validateInput"),
        ]
    };
    let mut functions = agent.functions.clone();
    for function in &mut functions {
        super::lowering::code::rewrite(&mut function.body, &mut |value| {
            if let Expr::Name(name) = value
                && let Some((_, replacement)) = names.iter().find(|(old, _)| old == name)
            {
                *name = (*replacement).into();
            }
        });
    }
    let functions = functions
        .iter()
        .map(|item| function(item, lang, 4))
        .collect::<Vec<_>>()
        .join("\n");
    let imports = type_names
        .iter()
        .filter(|name| super::planning::mentions(&functions, name))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let declarations = if imports.is_empty() {
        String::new()
    } else if lang == ScorerLang::Python {
        format!("if __package__:\n    from .{types_module} import {imports}\nelse:\n    from {types_module} import {imports}\n")
    } else {
        format!("import type {{{imports}}} from \"./{types_module}.mts\";\n")
    };
    let declarations = if lang == ScorerLang::Python && super::planning::mentions(&functions, "Any") {
        format!("from typing import Any\n{declarations}")
    } else {
        declarations
    };
    let helpers = match lang {
        ScorerLang::Python => format!("{PYTHON_HELPERS}\n{PY_RUNTIME}"),
        ScorerLang::Typescript => format!("{TYPESCRIPT_HELPERS}\n{TS_RUNTIME}"),
    };
    let extra = runtime_helpers(lang, agent.tokens);
    let rename = |source: &str| {
        let mut output = String::new();
        let mut word = String::new();
        for c in source.chars().chain(std::iter::once('\n')) {
            if c.is_ascii_alphanumeric() || c == '_' {
                word.push(c);
            } else {
                output.push_str(
                    names
                        .iter()
                        .find(|(old, _)| old == &word)
                        .map(|(_, new)| *new)
                        .unwrap_or(&word),
                );
                word.clear();
                output.push(c);
            }
        }
        output
    };
    let helpers = rename(&helpers);
    let extra = rename(&extra);
    let source = match lang {
        ScorerLang::Python => format!(
            "# Generated by bts; edits will be overwritten.\n{helpers}\n{extra}\n_MISSING = object()\n\n{declarations}\n\n{functions}\nif __name__ == \"__main__\":\n    print(run())\n"
        ),
        ScorerLang::Typescript => format!(
            "// Generated by bts; edits will be overwritten.\n{helpers}\n{extra}\nconst _MISSING = Symbol(\"omitted input\");\n\n{declarations}\n\n{functions}\nif (import.meta.main) console.log(run());\n"
        ),
    };
    (source, types)
}
fn runtime_helpers(lang: ScorerLang, tokens: bool) -> String {
    let (base, tokenizer) = match lang {
        ScorerLang::Python => (PYTHON, PY_TOKENS),
        ScorerLang::Typescript => (TYPESCRIPT, TS_TOKENS),
    };
    format!("{base}\n{}", if tokens { tokenizer } else { "" })
}

pub(crate) fn write_project(
    project: &super::assembly::Project,
    mut before_overwrite: impl FnMut(&std::path::Path, &str),
    mut after_write: impl FnMut(&std::path::Path, Action),
    mut after_remove: impl FnMut(&std::path::Path),
) -> Result<(), Error> {
    use std::fs;
    for file in &project.files {
        let parent = file.path.parent().expect("generated file has a parent");
        fs::create_dir_all(parent).map_err(|source| Error::CreateDir {
            path: parent.into(),
            source,
        })?;
        let action = write_file(&file.path, &file.contents, &mut before_overwrite)?;
        after_write(&file.path, action);
    }
    for path in &project.obsolete {
        if fs::read_to_string(path).is_ok_and(|source| {
            source.to_ascii_lowercase().starts_with("# generated by bts;")
                || source.to_ascii_lowercase().starts_with("// generated by bts;")
        }) {
            fs::remove_file(path).map_err(|source| Error::RemoveSource {
                path: path.clone(),
                source,
            })?;
            after_remove(path);
        }
    }
    Ok(())
}

pub(super) fn evaluation(
    plan: &super::planning::Evaluation<'_>,
    rows: &[serde_json::Value],
    lang: ScorerLang,
    project: &str,
) -> super::assembly::Source {
    let experiment = plan.experiment;
    let stem = &plan.agent;
    let eval_stem = &plan.module;
    let mut functions = Vec::new();
    let mut imports = String::new();
    for scorer in &plan.scorers {
        let name = &scorer.name;
        let slug = super::slugify(name).expect("valid scorer name");
        let function = format!("scorer_{}", slug.replace('-', "_"));
        let file = super::planning::callable_name(scorer.file.as_deref().unwrap_or(&slug), lang);
        imports.push_str(&match lang {
            ScorerLang::Python => format!("from scorers.{} import {function}\n", file.trim_end_matches(".py")),
            ScorerLang::Typescript => format!("import {{{function}}} from \"../scorers/{}\";\n", file),
        });
        functions.push(function);
    }
    let data = serde_json::to_string_pretty(&rows).unwrap();
    let project = serde_json::to_string(project).unwrap();
    let experiment_name = serde_json::to_string(&experiment.name).unwrap();
    let description = serde_json::to_string(&experiment.description).unwrap();
    let scores = functions.join(", ");
    let ts_description = experiment
        .description
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap())
        .unwrap_or_else(|| "undefined".into());
    let (name, contents) = match lang {
        ScorerLang::Python => (
            format!("src/evals/{eval_stem}_eval.py"),
            format!(
                "# Generated by bts; edits will be overwritten.\nimport json\nfrom agents.{stem} import run_traced\n{imports}\n\nCASES = json.loads({data:?})\nSCORERS = [{scores}]\n\ndef task(input, hooks):\n    return run_traced(hooks.span, input)\n\ndef evaluate(project={project}, *, no_send_logs=False):\n    from braintrust import Eval\n    return Eval(project, experiment_name={experiment_name}, description=json.loads({description:?}), data=CASES, task=task, scores=SCORERS, no_send_logs=no_send_logs)\n\nif __name__ == \"__main__\":\n    print(evaluate().summary)\n"
            ),
        ),
        ScorerLang::Typescript => (
            format!("src/evals/{eval_stem}.eval.mts"),
            format!(
                "// Generated by bts; edits will be overwritten.\nimport {{runTraced}} from \"../agents/{stem}.mts\";\n{imports}\n\nexport const cases = {data};\nexport const scorers = [{scores}];\nexport function task(input: unknown, hooks: {{span: Parameters<typeof runTraced>[0]}}): any {{\n    return runTraced(hooks.span, input);\n}}\nexport async function evaluate(project={project}, noSendLogs=false) {{\n    const {{Eval}} = await import(\"braintrust\");\n    return Eval(project, {{experimentName: {experiment_name}, description: {ts_description}, data: cases, task, scores: scorers}} , {{noSendLogs}});\n}}\n\nif (import.meta.main) console.log((await evaluate()).summary);\n"
            ),
        ),
    };
    super::assembly::Source { name, contents }
}
