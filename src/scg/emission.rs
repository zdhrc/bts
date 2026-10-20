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

pub(crate) use builder::{Action, Error, build};

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
