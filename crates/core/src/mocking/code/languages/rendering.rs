//! Render functions, expressions, and runtime support into target-language source.

use crate::dsl::ScorerLang;

use crate::mocking::code::lowering::code::{Expr, Function, Statement, Variable};

pub(in crate::mocking::code) fn expression(value: &Expr, lang: ScorerLang) -> String {
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
                            Expr::String(t) => text.push_str(
                                &crate::mocking::code::lowering::scoring::escape(t)
                                    .replace('{', "{{")
                                    .replace('}', "}}"),
                            ),
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

pub(in crate::mocking::code) fn variable(variable: &Variable, lang: ScorerLang, indentation: usize) -> String {
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

pub(in crate::mocking::code) fn function(function: &Function, lang: ScorerLang, width: usize) -> String {
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

pub(in crate::mocking::code) fn statements(
    statements: &[Statement],
    lang: ScorerLang,
    width: usize,
    indentation: usize,
) -> String {
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
pub(in crate::mocking::code) mod runtime {
    pub(in crate::mocking::code) const PYTHON_HELPERS: &str = r#"
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

    pub(in crate::mocking::code) const TYPESCRIPT_HELPERS: &str = r#"
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

    pub(in crate::mocking::code) const PY_RUNTIME: &str = r#"
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
            self.span = start_trace(self.span, name, kind)
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
            log_trace(self.span, {k: v for k, v in fields.items() if k != "duration"})

    def fail(self, error):
        if self.owns_span and not self.ended: log_trace(self.span, {"error": str(error)})

    def close(self):
        if self.owns_span and not self.ended:
            self.ended = True
            end_trace(self.span)

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

    pub(in crate::mocking::code) const TS_RUNTIME: &str = r#"
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
function remainder(left: unknown, right: unknown): number | _Float { return _arithmetic("%", left, right); }
function divide(left: unknown, right: unknown): number | _Float { return _arithmetic("/", left, right); }

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
                ...Object.fromEntries(Object.entries(span.fields).map(([key, value]) => [key, _serialize(value)]))};
        });
        const children = Array(rows.length).fill(0);
        for (let index = rows.length - 1; index >= 0; index--) {
            const row = rows[index]; row.duration = Math.max(row.duration ?? 0.1, children[index]);
            if (row.parent !== null) children[row.parent] += row.duration;
        }
        return {output: _serialize(output), spans: rows};
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
            this.span = startTrace(this.span, name, kind);
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
        if (this.ownsSpan && !this.ended) logTrace(this.span, _serialize(Object.fromEntries(Object.entries(fields).filter(([key])=>key!=="duration"))));
    }
    fail(error: unknown): void {if(this.ownsSpan && !this.ended) logTrace(this.span, {error: error instanceof Error ? error.message : String(error)});}
    close(): void {if(this.ownsSpan && !this.ended){this.ended=true; endTrace(this.span);}}
    closeAll(): void {for(const scope of [...this.opened].reverse())scope.close();}
}
function _force(value: unknown): any { return typeof value === "function" ? value() : value; }
function _value(value: unknown): any {
    value = _force(value);
    if (value instanceof _Float) return value;
    if (Array.isArray(value)) return value.map(_value);
    if (value !== null && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, _value(item)]));
    return value;
}
function _serialize(value: unknown): any {
    value = _force(value);
    if (value instanceof _Float) return value.value;
    if (Array.isArray(value)) return value.map(_serialize);
    if (value !== null && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, _serialize(item)]));
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
        spans.push({name: scope.name, type: scope.kind, parent, ..._serialize(fields)});
        const childrenDuration = Object.values(scope.children).reduce((sum, child) => sum + visit(_force(child), index), 0);
        const duration = Math.max(fields.duration ?? 0.1, childrenDuration);
        spans[index].duration = duration;
        scope.close();
        return duration;
    }
    try {
        visit(root, null);
        return {output: _serialize(_read(root, "output")), spans};
    } catch(error) {root.fail(error);throw error;}
    finally {root.closeAll();}
}
"#;

    pub(in crate::mocking::code) const PYTHON: &str = r#"
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
def _render(template, values):
    return _language_render(_string(template), _force(values), lambda value: _language_text(_force(value)))
def _compose(parts, separator): return _language_compose(_value(parts), _string(separator))
def _sample_items(items, count, random_source):
    return _language_sample(_value(items), _force(count), lambda: _draw(random_source))
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

    pub(in crate::mocking::code) const TYPESCRIPT: &str = r#"
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
function _render(template: unknown, values: unknown): string { return _language_render(_string(template), _force(values), value => { value = _force(value); return _language_text(value instanceof _Float ? _language_float(value.value) : value); }); }
function _compose(parts: unknown, separator: unknown): string { return _language_compose(_value(parts), _string(separator)); }
function _sample_items(items: unknown, count: unknown, randomSource: () => number): unknown[] { const value = _force(count); return _language_sample(_value(items), value instanceof _Float ? NaN : value, () => _draw(randomSource)); }
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

    pub(in crate::mocking::code) const PY_TOKENS: &str = r#"
def _tokens(value):
    import tiktoken
    value = _value(value)
    text = value if isinstance(value, str) else _json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return len(tiktoken.get_encoding("o200k_base").encode_ordinary(text))
"#;

    pub(in crate::mocking::code) const TS_TOKENS: &str = r#"
import { getEncoding as _getEncoding } from "js-tiktoken";
const _encoding = _getEncoding("o200k_base");
function _tokens(value: unknown): number {
    value = _serialize(value);
    const sorted = (item: any): any => Array.isArray(item) ? item.map(sorted) : item !== null && typeof item === "object" ? Object.fromEntries(Object.keys(item).sort().map(key => [key, sorted(item[key])])) : item;
    return _encoding.encode(typeof value === "string" ? value : JSON.stringify(sorted(value)), [], []).length;
}
"#;
}

pub(in crate::mocking::code) const PY_LANGUAGE: &str = r#"
def _language_float(value): return float(value)

def _language_float_text(value):
    if value != value or value in (float("inf"), float("-inf")): raise ValueError("non-finite render number")
    if value == 0: return "-0.0" if str(value).startswith("-") else "0.0"
    sign = "-" if value < 0 else ""
    mantissa, _, power = str(abs(value)).lower().partition("e")
    whole, _, fraction = mantissa.partition(".")
    digits = whole + fraction
    point = len(whole) + int(power or "0")
    leading = len(digits) - len(digits.lstrip("0"))
    digits = digits.lstrip("0").rstrip("0")
    point -= leading
    exponent = point - 1
    if exponent < -5 or exponent >= 16:
        coefficient = digits[0] + ("." + digits[1:] if len(digits) > 1 else "")
        return sign + coefficient + "e" + ("+" if exponent >= 0 else "") + str(exponent)
    if point <= 0: return sign + "0." + "0" * -point + digits
    if point >= len(digits): return sign + digits + "0" * (point - len(digits)) + ".0"
    return sign + digits[:point] + "." + digits[point:]

def _language_math(op, values):
    if any(type(value) not in (int, float) for value in values): raise TypeError("expected numbers")
    floating = any(type(value) is float for value in values)
    a = values[0]
    if op == "neg": result = -a
    elif op == "abs": result = abs(a)
    elif op == "min": result = min(values)
    elif op == "max": result = max(values)
    elif op == "clamp":
        if values[1] > values[2]: raise ValueError("clamp bounds out of order")
        result = max(values[1], min(values[2], a))
    else:
        b = values[1]
        if op == "+": result = a + b
        elif op == "-": result = a - b
        elif op == "*": result = a * b
        elif op == "/": result = a / b if floating else (abs(a) // abs(b)) * (-1 if (a < 0) != (b < 0) else 1)
        else:
            import math
            result = math.fmod(a, b) if floating else a - ((abs(a) // abs(b)) * (-1 if (a < 0) != (b < 0) else 1)) * b
    return float(result) if floating else result

def _language_text(value):
    if type(value) is str: return value
    if type(value) is bool: return "true" if value else "false"
    if type(value) is float: return _language_float_text(value)
    if type(value) is int: return str(value)
    raise TypeError("render values must be strings, numbers, or booleans")

def _language_render(template, values, text=_language_text):
    if type(template) is not str or type(values) is not dict:
        raise TypeError("render expects a string and an object")
    result, i = [], 0
    while i < len(template):
        ch = template[i]
        if ch in "{}":
            if i + 1 < len(template) and template[i + 1] == ch:
                result.append(ch)
                i += 2
                continue
            if ch == "}": raise ValueError("unmatched render brace")
            end = template.find("}", i + 1)
            if end < 0: raise ValueError("unmatched render brace")
            key = template[i + 1:end]
            if not key or "{" in key or key not in values:
                raise ValueError("invalid or missing render name: " + key)
            result.append(text(values[key]))
            i = end + 1
        else:
            result.append(ch)
            i += 1
    return "".join(result)

def _language_compose(parts, separator):
    if type(parts) is not list or type(separator) is not str:
        raise TypeError("compose expects an array and a string")
    result = []
    for part in parts:
        if part is None: continue
        if type(part) is not str: raise TypeError("compose parts must be strings or null")
        if part != "": result.append(part)
    return separator.join(result)

def _language_sample(items, count, random_source):
    if type(items) is not list or type(count) is not int or not 0 <= count <= len(items):
        raise ValueError("sample count must be an integer between zero and the array length")
    result, available = [], len(items)
    for item in items:
        if count > 0 and (count == available or int(random_source() * available) < count):
            result.append(item)
            count -= 1
        available -= 1
    return result
"#;

pub(in crate::mocking::code) const TS_LANGUAGE: &str = r#"
class _LanguageFloat {
    readonly value: number;
    constructor(value: number) { this.value = value; }
    valueOf(): number { return this.value; }
}
function _language_float(value: number): any { return new _LanguageFloat(value); }
function _language_float_text(value: number): string {
    if (!Number.isFinite(value)) throw new Error("non-finite render number");
    if (value === 0) return Object.is(value, -0) ? "-0.0" : "0.0";
    const sign = value < 0 ? "-" : "", [mantissa, power = "0"] = String(Math.abs(value)).toLowerCase().split("e");
    const [whole, fraction = ""] = mantissa.split(".");
    let digits = whole + fraction, point = whole.length + Number(power);
    point -= digits.length - digits.replace(/^0+/, "").length;
    digits = digits.replace(/^0+/, "").replace(/0+$/, "");
    const exponent = point - 1;
    if (exponent < -5 || exponent >= 16) return sign + digits[0] + (digits.length > 1 ? "." + digits.slice(1) : "") + "e" + (exponent >= 0 ? "+" : "") + exponent;
    if (point <= 0) return sign + "0." + "0".repeat(-point) + digits;
    if (point >= digits.length) return sign + digits + "0".repeat(point - digits.length) + ".0";
    return sign + digits.slice(0, point) + "." + digits.slice(point);
}
function _language_math(op: string, values: any[]): any {
    if (values.some(value => !(value instanceof _LanguageFloat) && typeof value !== "number")) throw new TypeError("expected numbers");
    const floating = values.some(value => value instanceof _LanguageFloat || !Number.isInteger(value));
    const numbers = values.map(Number), [a, b] = numbers;
    let result: number;
    if (op === "neg") result = -a;
    else if (op === "abs") result = Math.abs(a);
    else if (op === "min") result = Math.min(...numbers);
    else if (op === "max") result = Math.max(...numbers);
    else if (op === "clamp") {
        if (b > numbers[2]) throw new Error("clamp bounds out of order");
        result = Math.max(b, Math.min(numbers[2], a));
    } else if (op === "+") result = a + b;
    else if (op === "-") result = a - b;
    else if (op === "*") result = a * b;
    else {
        if (b === 0) throw new Error("division by zero");
        result = op === "/" ? (floating ? a / b : Math.trunc(a / b)) : a % b;
    }
    if (!Number.isFinite(result)) throw new Error("non-finite number");
    return floating ? _language_float(result) : (result === 0 ? 0 : result);
}
function _language_text(value: unknown): string {
    if (value instanceof _LanguageFloat) return _language_float_text(value.value);
    if (typeof value === "number") return Number.isInteger(value) && !Object.is(value, -0) ? String(value) : _language_float_text(value);
    if (typeof value === "string" || typeof value === "boolean") return String(value);
    throw new TypeError("render values must be strings, numbers, or booleans");
}
function _language_render(template: unknown, values: unknown, text: (value: unknown) => string = _language_text): string {
    if (typeof template !== "string" || values === null || typeof values !== "object" || Array.isArray(values)) throw new TypeError("render expects a string and an object");
    let result = "";
    for (let i = 0; i < template.length;) {
        const ch = template[i];
        if (ch === "{" || ch === "}") {
            if (template[i + 1] === ch) { result += ch; i += 2; continue; }
            if (ch === "}") throw new Error("unmatched render brace");
            const end = template.indexOf("}", i + 1);
            if (end < 0) throw new Error("unmatched render brace");
            const key = template.slice(i + 1, end);
            if (!key || key.includes("{") || !Object.prototype.hasOwnProperty.call(values, key)) throw new Error("invalid or missing render name: " + key);
            result += text((values as Record<string, unknown>)[key]);
            i = end + 1;
        } else { result += ch; i++; }
    }
    return result;
}
function _language_compose(parts: unknown, separator: unknown): string {
    if (!Array.isArray(parts) || typeof separator !== "string") throw new TypeError("compose expects an array and a string");
    return parts.filter(part => {
        if (part === null) return false;
        if (typeof part !== "string") throw new TypeError("compose parts must be strings or null");
        return part !== "";
    }).join(separator);
}
function _language_sample(items: unknown, count: unknown, randomSource: () => number): unknown[] {
    if (!Array.isArray(items) || typeof count !== "number" || !Number.isSafeInteger(count) || count < 0 || count > items.length) throw new Error("sample count must be an integer between zero and the array length");
    const result: unknown[] = [];
    let remaining = count, available = items.length;
    for (const item of items) {
        if (remaining > 0 && (remaining === available || Math.floor(randomSource() * available) < remaining)) { result.push(item); remaining--; }
        available--;
    }
    return result;
}
"#;

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

pub(in crate::mocking::code) fn agent(
    agent: &crate::mocking::code::lowering::Agent,
    lang: ScorerLang,
    types_module: &str,
    tracing: &str,
) -> (String, Option<String>) {
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
        crate::mocking::code::lowering::code::rewrite(&mut function.body, &mut |value| {
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
        .filter(|name| crate::mocking::code::planning::mentions(&functions, name))
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
    let declarations = if lang == ScorerLang::Python && crate::mocking::code::planning::mentions(&functions, "Any") {
        format!("from typing import Any\n{declarations}")
    } else {
        declarations
    };
    let helpers = match lang {
        ScorerLang::Python => format!("{PYTHON_HELPERS}\n{PY_RUNTIME}\n{tracing}"),
        ScorerLang::Typescript => format!("{TYPESCRIPT_HELPERS}\n{TS_RUNTIME}\n{tracing}"),
    };
    let extra = runtime_helpers(
        lang,
        agent.tokens,
        crate::mocking::code::planning::mentions(&functions, "_english"),
    );
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
fn runtime_helpers(lang: ScorerLang, tokens: bool, english: bool) -> String {
    let (base, tokenizer) = match lang {
        ScorerLang::Python => (PYTHON, PY_TOKENS),
        ScorerLang::Typescript => (TYPESCRIPT, TS_TOKENS),
    };
    let language = if lang == ScorerLang::Python {
        PY_LANGUAGE
    } else {
        TS_LANGUAGE
    };
    format!(
        "{base}\n{language}\n{}\n{}",
        if tokens { tokenizer } else { "" },
        if english { english_source(lang, true) } else { String::new() }
    )
}

const PY_ENGLISH: &str = r#"_english_space = "\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
_english_space_pattern = r"[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]"

def _english_object(value, fields, defaults=None):
    if type(value) is not dict or any(key not in fields for key in value):
        raise ValueError("invalid English object fields")
    result = dict(defaults or {})
    result.update(value)
    return result

def _english_enum(value, choices):
    if type(value) is not str or value not in choices:
        raise ValueError("invalid English option")
    return value

def _english_string(value):
    if type(value) is not str:
        raise ValueError("expected English string")
    return value

def _english_array(value):
    if type(value) is not list:
        raise ValueError("expected English array")
    return value

def _english_fragment(value):
    value = _english_string(value)
    if not value or value.strip(_english_space) != value or any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "?!;,\u2028\u2029" for c in value) or value.endswith(".") or _english_re.search(r"\." + _english_space_pattern, value):
        raise ValueError("invalid English fragment")
    return value

def _english_subject(value):
    value = _english_object(value, ["text", "person", "number"], {"person": "third", "number": "singular"})
    text = _english_fragment(value.get("text"))
    person = _english_enum(value["person"], ["first", "second", "third"])
    number = _english_enum(value["number"], ["singular", "plural"])
    expected = {"I": ("first", "singular"), "we": ("first", "plural"), "We": ("first", "plural"), "he": ("third", "singular"), "He": ("third", "singular"), "she": ("third", "singular"), "She": ("third", "singular"), "it": ("third", "singular"), "It": ("third", "singular"), "they": ("third", "plural"), "They": ("third", "plural")}.get(text)
    if expected is not None and expected != (person, number) or text in ["you", "You"] and person != "second":
        raise ValueError("pronoun agreement mismatch")
    return value

def _english_verb(value):
    if type(value) is str:
        if value not in _EnglishData.data["verbs"]:
            raise ValueError("unknown English verb")
        return _EnglishData.data["verbs"][value]
    fields = ["base", "third_person_singular", "past", "past_participle", "present_participle"]
    value = _english_object(value, fields)
    if any(type(value.get(key)) is not str or _english_re.fullmatch(r"[a-z]+", value[key]) is None for key in fields):
        raise ValueError("invalid English verb forms")
    if value["base"] == "be" and value != _EnglishData.data["verbs"]["be"]:
        raise ValueError("invalid forms of be")
    return value

def _english_clause(value, condition=False):
    fields = ["subject", "predicate", "polarity", "tense", "aspect"] + ([] if condition else ["modal", "form"])
    clause = _english_object(value, fields, {"polarity": "positive", "tense": "present", "aspect": "simple"})
    clause["subject"] = _english_subject(clause.get("subject"))
    predicate = _english_object(clause.get("predicate"), ["verb", "complement"])
    predicate["verb"] = _english_verb(predicate.get("verb"))
    if predicate.get("complement") is not None:
        _english_fragment(predicate["complement"])
    clause["predicate"] = predicate
    _english_enum(clause["polarity"], ["positive", "negative"])
    _english_enum(clause["tense"], ["present", "past", "future"])
    _english_enum(clause["aspect"], ["simple", "progressive", "perfect", "perfect_progressive"])
    clause["form"] = "statement" if condition else _english_enum(clause.get("form", "statement"), ["statement", "question"])
    clause["modal"] = None if condition else clause.get("modal")
    if clause["modal"] is not None:
        _english_enum(clause["modal"], ["can", "could", "may", "might", "must", "should", "will", "would"])
        if clause["tense"] != "present":
            raise ValueError("modal clauses require present tense")
    return clause

def _english_sentence(value):
    sentence = _english_object(value, ["clause", "condition"])
    sentence["clause"] = _english_clause(sentence.get("clause"))
    if sentence.get("condition") is not None:
        sentence["condition"] = _english_clause(sentence["condition"], True)
    return sentence

def _english_render_options(value):
    options = _english_object(value, ["negation", "condition_position"], {"negation": "expanded", "condition_position": "before"})
    _english_enum(options["negation"], ["expanded", "contracted"])
    _english_enum(options["condition_position"], ["before", "after"])
    return options

class _EnglishUnsupported(ValueError):
    pass

def _english_contract(auxiliary):
    return next((row[2] for row in _EnglishData.data["negations"] if row[0] == auxiliary), None)

def _english_start(text):
    words = ["if", "the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "we", "you", "he", "she", "it", "they", "can", "could", "may", "might", "must", "should", "will", "would", "am", "is", "are", "was", "were", "have", "has", "had", "do", "does", "did"] + [row[2] for row in _EnglishData.data["negations"]]
    return text[0].upper() + text[1:] if text.split(" ")[0] in words else text

def _english_render_clause(clause, style):
    subject, predicate = clause["subject"], clause["predicate"]
    verb = predicate["verb"]
    third = subject["person"] == "third" and subject["number"] == "singular"
    past = clause["tense"] == "past"
    be = ("was" if subject["person"] in ["first", "third"] and subject["number"] == "singular" else "were") if past else ("am" if subject["person"] == "first" and subject["number"] == "singular" else "is" if third else "are")
    have = "had" if past else "has" if third else "have"
    do = "did" if past else "does" if third else "do"
    modal = clause["modal"] or ("will" if clause["tense"] == "future" else None)
    aspect = clause["aspect"]
    negative, question = clause["polarity"] == "negative", clause["form"] == "question"
    if modal is not None:
        first = modal
        rest = {"simple": verb["base"], "progressive": "be " + verb["present_participle"], "perfect": "have " + verb["past_participle"], "perfect_progressive": "have been " + verb["present_participle"]}[aspect]
    elif aspect == "simple":
        if verb["base"] == "be": first, rest = be, ""
        elif negative or question: first, rest = do, verb["base"]
        else: first, rest = verb["past"] if past else verb["third_person_singular"] if third else verb["base"], ""
    elif aspect == "progressive": first, rest = be, verb["present_participle"]
    elif aspect == "perfect": first, rest = have, verb["past_participle"]
    else: first, rest = have, "been " + verb["present_participle"]
    if negative and style == "contracted":
        first = _english_contract(first)
        if first is None: raise _EnglishUnsupported("unsupported negation contraction")
        start = first + " " + subject["text"] if question else subject["text"] + " " + first
    elif negative:
        start = first + " " + subject["text"] + " not" if question else subject["text"] + (" cannot" if first == "can" else " " + first + " not")
    else: start = first + " " + subject["text"] if question else subject["text"] + " " + first
    return " ".join(part for part in [start, rest, predicate.get("complement")] if part)

def _english_render(sentence, options, fragment=False):
    main = _english_render_clause(sentence["clause"], options["negation"])
    condition = sentence.get("condition")
    if condition is not None:
        condition = _english_render_clause(condition, options["negation"])
        body = ("if " if fragment else "If ") + condition + ", " + main if options["condition_position"] == "before" else (main if fragment else _english_start(main)) + " if " + condition
    else: body = main if fragment else _english_start(main)
    return body if fragment else body + ("?" if sentence["clause"]["form"] == "question" else ".")

def _english_safe_word(word):
    return bool(word) and all(c.isalnum() or c in "_-" for c in word) and any(c.isalnum() for c in word) and word.lower() not in ["if", "when", "unless", "because", "although", "while", "that", "whether", "who", "whom", "whose", "which", "where", "as", "or", "and", "but", "not", "no", "never", "neither", "nor", "only", "just", "merely", "except", "rather", "instead", "so"]

def _english_determiner(word):
    return word in ["the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "his", "her", "its"]

def _english_auxiliary(word):
    return word in ["can", "could", "may", "might", "must", "should", "will", "would", "do", "does", "did", "am", "is", "are", "was", "were", "have", "has", "had"]

def _english_analyze_subject(words):
    if not words: return False
    if words[0].lower() in ["i", "you", "he", "she", "it", "we", "they"]: return len(words) == 1
    if _english_determiner(words[0].lower()):
        return len(words) >= 2 and all(_english_safe_word(w) and not _english_determiner(w.lower()) and not _english_auxiliary(w.lower()) and w.lower() not in ["i", "you", "he", "she", "it", "we", "they", "to"] for w in words[1:])
    if len(words) == 1 and _english_safe_word(words[0]) and any(c.isalpha() for c in words[0]) and ("_" in words[0] or any(c in "0123456789" for c in words[0])): return True
    return len(words) <= 3 and all(_english_safe_word(w) and w[0].isupper() and not _english_auxiliary(w.lower()) for w in words)

def _english_agrees(auxiliary, words):
    if len(words) != 1: return True
    allowed = {"i": ["am", "was", "do", "have"], "you": ["are", "were", "do", "have"], "we": ["are", "were", "do", "have"], "they": ["are", "were", "do", "have"], "he": ["is", "was", "does", "has"], "she": ["is", "was", "does", "has"], "it": ["is", "was", "does", "has"]}.get(words[0].lower())
    return allowed is None or auxiliary not in ["am", "is", "are", "was", "were", "do", "does", "have", "has"] or auxiliary in allowed

def _english_be_complement(words):
    if not words: return False
    if any(words[0] in [v["present_participle"], v["past_participle"]] for v in _EnglishData.data["verbs"].values()): return True
    if _english_determiner(words[0]) or words[0] in ["in", "on", "at", "near", "under", "above", "outside", "inside", "with", "without"]: return len(words) > 1
    return words[0] in ["ready", "late", "early", "available", "unavailable", "hungry", "thirsty", "open", "closed", "complete", "correct", "incorrect", "certain", "safe", "sure", "possible", "impossible", "necessary", "happy", "sad", "tired", "well", "ill", "here", "there", "alone", "able", "unable", "busy", "free", "valid", "invalid", "empty", "full", "active", "inactive", "online", "offline", "clear", "wrong", "right"]

def _english_perfect(words):
    return bool(words) and (_english_be_complement(words[1:]) if words[0] == "been" else any(words[0] == v["past_participle"] for v in _EnglishData.data["verbs"].values()))

def _english_base(words):
    if not words: return False
    if words[0] == "have" and len(words) > 1 and words[1] == "to": return len(words) > 2 and words[2] in _EnglishData.data["verbs"]
    return words[0] in _EnglishData.data["verbs"]

def _english_predicate(auxiliary, words):
    if not words or not all(_english_safe_word(w) for w in words): return False
    if auxiliary in ["do", "does", "did"]: return _english_base(words)
    if auxiliary in ["am", "is", "are", "was", "were"]: return _english_be_complement(words)
    if auxiliary in ["have", "has", "had"]: return _english_perfect(words)
    if words[0] == "be": return _english_be_complement(words[1:])
    if words[0] == "have" and len(words) > 1 and (words[1] == "been" or any(words[1] == v["past_participle"] for v in _EnglishData.data["verbs"].values())): return _english_perfect(words[1:])
    return _english_base(words)

def _english_analyze(text):
    trimmed = text.strip(_english_space)
    if not trimmed.endswith((".", "?")): return None
    body, question = trimmed[:-1], trimmed[-1] == "?"
    if any((ord(c) < 32 and c != "\t") or 127 <= ord(c) <= 159 or c in "\u2028\u2029.?!" for c in body): return None
    matches = list(_english_re.finditer("[^" + _english_space_pattern[1:-1] + "]+", body))
    words = [m.group() for m in matches]
    if question:
        if not words or not _english_auxiliary(words[0].lower()): return None
        found = sum(_english_analyze_subject(words[1:end]) and _english_agrees(words[0].lower(), words[1:end]) and _english_predicate(words[0].lower(), words[end:]) for end in range(2, min(len(words), 7)))
        return {"edit": None} if found == 1 else None
    found = None
    for index in range(1, min(len(words), 6)):
        if not _english_analyze_subject(words[:index]): continue
        word, edit, start = words[index], None, index + 1
        expansion = next((row for row in _EnglishData.data["negations"] if word in row[2:]), None)
        if expansion is not None:
            auxiliary = expansion[0]
            edit = (matches[index].start(), matches[index].end(), expansion[1])
        elif word == "cannot": auxiliary, edit = "can", (matches[index].start(), matches[index].end(), "can't")
        elif _english_auxiliary(word):
            auxiliary = word
            if start < len(words) and words[start] == "not":
                if word == "can": continue
                contracted = _english_contract(word)
                if contracted is not None: edit = (matches[index].start(), matches[start].end(), contracted)
                start += 1
        else: continue
        if _english_agrees(auxiliary, words[:index]) and _english_predicate(auxiliary, words[start:]):
            if found is not None: return None
            if edit is not None:
                offset = len(text) - len(text.lstrip(_english_space))
                edit = (edit[0] + offset, edit[1] + offset, edit[2])
            found = {"edit": edit}
    return found if found is not None else {"edit": None} if _english_lexical(words) else None

def _english_vary(value, options, random):
    if type(options) is dict and any(k not in ["negation", "condition_order", "rendering"] for k in options): return _english_style(value, options, random)
    options = _english_object(options, ["negation", "condition_order", "rendering"], {"negation": True, "condition_order": True, "rendering": {}})
    if type(options["negation"]) is not bool or type(options["condition_order"]) is not bool: raise ValueError("variation controls must be booleans")
    rendering = _english_render_options(options["rendering"])
    if type(value) is str:
        alternatives = _english_text_alternatives(value, options)
        return ([value] + alternatives)[min(len(alternatives), int(random() * (len(alternatives) + 1)))] if alternatives else value
    sentence = _english_sentence(value)
    original = _english_render(sentence, rendering)
    alternatives = [original]
    negative = sentence["clause"]["polarity"] == "negative" or sentence.get("condition") is not None and sentence["condition"]["polarity"] == "negative"
    negative_question = sentence["clause"]["form"] == "question" and sentence["clause"]["polarity"] == "negative"
    if options["negation"] and negative and not negative_question:
        candidate = dict(rendering, negation="contracted" if rendering["negation"] == "expanded" else "expanded")
        try: alternatives.append(_english_render(sentence, candidate))
        except _EnglishUnsupported: pass
    if options["condition_order"] and sentence.get("condition") is not None:
        alternatives.append(_english_render(sentence, dict(rendering, condition_position="after" if rendering["condition_position"] == "before" else "before")))
    return alternatives[min(len(alternatives) - 1, int(random() * len(alternatives)))] if len(alternatives) > 1 else original

def _english_text(value):
    value = _english_string(value)
    if not value or value.strip(_english_space) != value or any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "\u2028\u2029" for c in value): raise ValueError("invalid context text")
    return value

def _english_entity(value):
    value = _english_object(value, ["id", "label", "person", "number", "pronouns"], {"person": "third", "number": "singular", "pronouns": None})
    _english_text(value.get("id")); _english_text(value.get("label"))
    _english_enum(value["person"], ["first", "second", "third"]); _english_enum(value["number"], ["singular", "plural"])
    if value["pronouns"] is not None:
        fields = ["subject", "object", "possessive_determiner", "possessive"]
        forms = _english_object(value["pronouns"], fields)
        if any(type(forms.get(key)) is not str or _english_re.fullmatch(r"[A-Za-z]+", forms[key]) is None for key in fields): raise ValueError("invalid pronoun forms")
        value["pronouns"] = forms
    return value

def _english_context(value):
    value = _english_object(value, ["entities", "mentioned", "facts"], {"entities": [], "mentioned": [], "facts": []})
    value["entities"] = [_english_entity(entity) for entity in _english_array(value["entities"])]
    value["mentioned"] = [_english_text(item) for item in _english_array(value["mentioned"])]
    value["facts"] = [_english_text(item) for item in _english_array(value["facts"])]
    ids = [entity["id"] for entity in value["entities"]]
    if len(set(ids)) != len(ids) or len(set(value["mentioned"])) != len(value["mentioned"]) or len(set(value["facts"])) != len(value["facts"]) or any(item not in ids for item in value["mentioned"]): raise ValueError("invalid discourse context")
    return value

def _english_refer(entity, context, options):
    entity, context = _english_entity(entity), _english_context(context)
    options = _english_object(options, ["form", "style"], {"form": "subject", "style": "auto"})
    form = _english_enum(options["form"], ["subject", "object", "possessive_determiner", "possessive"])
    style = _english_enum(options["style"], ["auto", "name", "pronoun"])
    if any(other["id"] == entity["id"] and other != entity for other in context["entities"]): raise ValueError("conflicting entity definition")
    candidate = entity["pronouns"][form] if entity["pronouns"] is not None else None
    unambiguous = candidate is not None and not any(other["id"] != entity["id"] and other["id"] in context["mentioned"] and (other["pronouns"] is None or other["pronouns"][form].lower() == candidate.lower()) for other in context["entities"])
    if style != "name" and entity["id"] in context["mentioned"] and unambiguous: return candidate
    if style == "pronoun": raise ValueError("ambiguous or unintroduced pronoun")
    if form in ["subject", "object"]: return entity["label"]
    return entity["label"] + ("'" if entity["label"].endswith(("s", "S")) else "'s")

def _english_remember(context, entities, facts):
    context = _english_context(context)
    for entity in [_english_entity(value) for value in _english_array(entities)]:
        old = next((other for other in context["entities"] if other["id"] == entity["id"]), None)
        if old is not None and old != entity: raise ValueError("conflicting entity definition")
        if old is None: context["entities"].append(entity)
        if entity["id"] not in context["mentioned"]: context["mentioned"].append(entity["id"])
    for fact in [_english_text(value) for value in _english_array(facts)]:
        if fact not in context["facts"]: context["facts"].append(fact)
    return context

def _english_unmentioned(details, context):
    context, seen, selected = _english_context(context), {}, []
    for detail in _english_array(details):
        detail = _english_object(detail, ["id", "text"])
        identifier, text = _english_text(detail.get("id")), _english_text(detail.get("text"))
        if identifier in seen:
            if seen[identifier] != text: raise ValueError("conflicting detail text")
            continue
        seen[identifier] = text
        if identifier not in context["facts"]: selected.append(detail)
    return selected

def _english_locale(value):
    return _english_enum(value, ["en-US", "en-GB", "en-CA", "en-AU", "en-IN"])

def _english_number(value, options):
    options = _english_object(options, ["locale", "grouping", "fraction_digits"], {"locale": "en-US", "grouping": True, "fraction_digits": None})
    locale = _english_locale(options["locale"])
    precision = options["fraction_digits"]
    if type(options["grouping"]) is not bool or precision is not None and (type(precision) not in [int, float] or not _english_math.isfinite(precision) or precision != int(precision) or not 0 <= precision <= 9): raise ValueError("invalid number options")
    if precision is not None: precision = int(precision)
    if type(value) in [int, float]:
        if not _english_math.isfinite(value): raise ValueError("invalid number")
        value = str(value)
    value = _english_string(value)
    match = _english_re.fullmatch(r"([+-]?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?", value)
    if match is None or len(value) > 4096: raise ValueError("invalid decimal")
    sign, integer, fraction, exponent = match.groups()
    fraction, exponent = fraction or "", int(exponent or "0")
    if abs(exponent) > 4096: raise ValueError("decimal expansion too large")
    digits = integer + fraction
    point = len(integer) + exponent
    if point <= 0 and 1 - point + len(digits) > 4096 or point >= len(digits) and point > 4096: raise ValueError("decimal expansion too large")
    if point < 0: digits, point = "0" * -point + digits, 0
    if point > len(digits): digits += "0" * (point - len(digits))
    integer, fraction = (digits[:point].lstrip("0") or "0"), digits[point:]
    if len(integer) + len(fraction) > 4096: raise ValueError("decimal expansion too large")
    if precision is None: fraction = fraction.rstrip("0")
    else:
        round_up = len(fraction) > precision and fraction[precision] >= "5"
        fraction = (fraction + "0" * precision)[:precision]
        if round_up:
            combined = list(integer + fraction)
            index = len(combined) - 1
            while index >= 0 and combined[index] == "9": combined[index], index = "0", index - 1
            if index >= 0: combined[index] = str(int(combined[index]) + 1)
            else: combined.insert(0, "1")
            combined = "".join(combined)
            integer = combined[:-precision] if precision else combined
            fraction = combined[-precision:] if precision else ""
    negative = sign == "-" and any(c != "0" for c in integer + fraction)
    if options["grouping"]:
        parts, width = [], 3
        while integer:
            parts.append(integer[-width:]); integer = integer[:-width]
            if locale == "en-IN": width = 2
        integer = ",".join(reversed(parts))
    return ("-" if negative else "") + integer + ("." + fraction if fraction else "")

def _english_date(value, options):
    options = _english_object(options, ["locale", "style"], {"locale": "en-US", "style": "long"})
    locale, style = _english_locale(options["locale"]), _english_enum(options["style"], ["short", "long"])
    value = _english_string(value)
    if _english_re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", value) is None: raise ValueError("invalid date")
    year, month, day = map(int, value.split("-"))
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    if not 1 <= year <= 9999 or not 1 <= month <= 12 or not 1 <= day <= days[month - 1]: raise ValueError("invalid date")
    if style == "short":
        if locale == "en-CA": return value
        return f"{month:02d}/{day:02d}/{year:04d}" if locale == "en-US" else f"{day:02d}/{month:02d}/{year:04d}"
    name = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][month - 1]
    return f"{name} {day}, {year}" if locale in ["en-US", "en-CA"] else f"{day} {name} {year}"

def _english_time(value, options):
    options = _english_object(options, ["locale", "hour_cycle", "seconds"], {"locale": "en-US", "hour_cycle": None, "seconds": False})
    locale = _english_locale(options["locale"])
    cycle = ("h24" if locale == "en-GB" else "h12") if options["hour_cycle"] is None else _english_enum(options["hour_cycle"], ["h12", "h24"])
    if type(options["seconds"]) is not bool: raise ValueError("seconds must be boolean")
    value = _english_string(value)
    if _english_re.fullmatch(r"[0-9]{2}:[0-9]{2}(?::[0-9]{2})?", value) is None: raise ValueError("invalid time")
    parts = list(map(int, value.split(":")))
    hour, minute, second = parts[0], parts[1], parts[2] if len(parts) == 3 else 0
    if hour > 23 or minute > 59 or second > 59: raise ValueError("invalid time")
    result = f"{hour:02d}:{minute:02d}" if cycle == "h24" else f"{hour % 12 or 12}:{minute:02d}"
    if options["seconds"]: result += f":{second:02d}"
    if cycle == "h12": result += " AM" if hour < 12 else " PM"
    return result

def _language_english(operation, args, random):
    arities = {"realize": (1, 2), "vary": (1, 2), "combine": (2, 3), "refer": (2, 3), "remember": (2, 3), "unmentioned": (2, 2), "format_number": (1, 2), "format_date": (1, 2), "format_time": (1, 2)}
    if operation not in arities or type(args) is not list or not arities[operation][0] <= len(args) <= arities[operation][1]: raise ValueError("invalid English call")
    if operation == "realize": return _english_render(_english_sentence(args[0]), _english_render_options(args[1] if len(args) > 1 else {}))
    if operation == "vary": return _english_vary(args[0], args[1] if len(args) > 1 else {}, random)
    if operation == "combine":
        sentences = [_english_sentence(value) for value in _english_array(args[0])]
        relation = _english_enum(args[1], ["and", "but", "or", "because"])
        options = _english_render_options(args[2] if len(args) > 2 else {})
        if len(sentences) < 2 or relation == "because" and len(sentences) != 2 or any(sentence["clause"]["form"] != "statement" or sentence.get("condition") is not None for sentence in sentences): raise ValueError("invalid proposition combination")
        if relation == "because": return "Because " + _english_render(sentences[1], options, True) + ", " + _english_render(sentences[0], options, True) + "."
        separator = ", " + relation + " "
        return _english_render(sentences[0], options)[:-1] + separator + separator.join(_english_render(sentence, options, True) for sentence in sentences[1:]) + "."
    if operation == "refer": return _english_refer(args[0], args[1], args[2] if len(args) > 2 else {})
    if operation == "remember": return _english_remember(args[0], args[1], args[2] if len(args) > 2 else [])
    if operation == "unmentioned": return _english_unmentioned(args[0], args[1])
    if operation == "format_number": return _english_number(args[0], args[1] if len(args) > 1 else {})
    if operation == "format_date": return _english_date(args[0], args[1] if len(args) > 1 else {})
    return _english_time(args[0], args[1] if len(args) > 1 else {})

def _english_lexical(words):
    found = 0
    for index in range(1, min(len(words), 6)):
        if not _english_analyze_subject(words[:index]) or not all(_english_safe_word(word) for word in words[index:]): continue
        pronoun = words[0].lower() if index == 1 else None
        valid = False
        for verb in _EnglishData.data["verbs"].values():
            if verb["base"] == "be": continue
            forms = [verb["past"]]
            forms += [verb["third_person_singular"]] if pronoun in ["he", "she", "it"] else [verb["base"]] if pronoun in ["i", "you", "we", "they"] else [verb["base"], verb["third_person_singular"]]
            if words[index] in forms: valid = True
        found += int(valid)
    return found == 1

def _english_initial_case(text, question, upper):
    first, _, tail = text.partition(" ")
    lower = first.lower()
    pronoun = lower in ["you", "we", "they", "he", "she", "it"]
    determiner = _english_determiner(lower) and bool(tail) and tail[0].islower()
    auxiliary = question and _english_auxiliary(lower)
    if not (pronoun or determiner or auxiliary) or not first.isascii() or not first: return text
    return (text[0].upper() if upper else text[0].lower()) + text[1:]

def _english_text_alternatives(text, options):
    if not options["negation"] and not options["condition_order"]: return []
    edits, reordered = [], None
    trimmed, leading, trailing = text.strip(_english_space), len(text) - len(text.lstrip(_english_space)), len(text.rstrip(_english_space))
    conditional = None
    if trimmed.endswith((".", "?")):
        body, punctuation = trimmed[:-1], trimmed[-1]
        if not any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "\u2028\u2029.?!" for c in body):
            if body.startswith(("If ", "if ")) and ", " in body[3:]:
                condition, main = body[3:].split(", ", 1)
                if condition and main and "," not in main: conditional = (condition, main, leading + 3, leading + 3 + len(condition) + 2, punctuation, True)
            elif " if " in body:
                main, condition = body.split(" if ", 1)
                if condition and main and "," not in condition and "," not in main: conditional = (condition, main, leading + len(main) + 4, leading, punctuation, False)
    if conditional is not None:
        condition, main, condition_offset, main_offset, punctuation, before = conditional
        for clause, ending, offset in [(condition, ".", condition_offset), (main, punctuation, main_offset)]:
            analysis = _english_analyze(clause + ending)
            if analysis is None: return []
            if analysis["edit"] is not None:
                start, end, replacement = analysis["edit"]
                edits.append((offset + start, offset + end, replacement))
        if options["condition_order"]:
            main = _english_initial_case(main, punctuation == "?", before)
            changed = main + " if " + condition + punctuation if before else "If " + condition + ", " + main + punctuation
            reordered = text[:leading] + changed + text[trailing:]
    else:
        segments = list(_english_re.finditer(r"[^.?]*(?:\?" + _english_space_pattern + r"*|\.(?:" + _english_space_pattern + r"+|$))", text))
        if not segments or "".join(segment.group() for segment in segments) != text: return []
        for index, segment in enumerate(segments):
            if index and segments[index - 1].group().rstrip(_english_space).endswith(".") and segment.group()[0].islower() and not any(c in segments[index - 1].group() for c in "\n\r\u0085\u2028\u2029"): return []
            analysis = _english_analyze(segment.group())
            if analysis is None: return []
            if analysis["edit"] is not None:
                start, end, replacement = analysis["edit"]
                edits.append((segment.start() + start, segment.start() + end, replacement))
    edits.sort()
    alternatives = []
    if options["negation"]:
        for contracts in [False, True]:
            changed = text
            for start, end, replacement in reversed(edits):
                target_contracts = not any(row[1] == replacement for row in _EnglishData.data["negations"])
                if target_contracts == contracts: changed = changed[:start] + replacement + changed[end:]
            if changed != text and changed not in alternatives: alternatives.append(changed)
    if reordered is not None and reordered != text and reordered not in alternatives: alternatives.append(reordered)
    return alternatives

def _english_ascii_lower(text):
    return text.translate(str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz"))

def _english_style_options(options):
    data = _EnglishData.data['styles']
    options = _english_object(options, list(data['defaults']))
    style = options.get('style')
    if style is not None: _english_enum(style, list(data['presets']))
    result = dict(data['presets'][style] if style is not None else data['defaults'])
    optional = ['style', 'negation', 'condition_order', 'contractions', 'shorthand', 'casual', 'slang', 'reductions', 'fragments', 'warmth', 'soften_requests', 'punctuation', 'casing']
    result.update({k:v for k,v in options.items() if v is not None or k not in optional})
    for key in optional[1:-2]:
        if type(result[key]) is not bool: raise ValueError('style controls must be booleans')
    _english_enum(result['punctuation'], ['preserve', 'omit_period', 'exclamation', 'expressive'])
    _english_enum(result['casing'], ['preserve', 'lowercase'])
    result['rendering'] = _english_render_options(result['rendering'])
    result['typos'] = _english_object(result['typos'], ['probability', 'max_errors', 'kinds'], data['defaults']['typos'])
    for value in [result['probability'], result['typos']['probability']]:
        if type(value) not in [int, float] or not 0 <= value <= 1: raise ValueError('invalid style probability')
    for value in [result['max_changes'], result['typos']['max_errors']]:
        if type(value) not in [int, float] or not 0 <= value <= 16 or value != int(value): raise ValueError('invalid style budget')
    for kind in _english_array(result['typos']['kinds']): _english_enum(kind, ['common', 'omission', 'transposition', 'repetition'])
    result['protected'] = list(_english_array(result['protected']))
    if any(not _english_string(term) for term in result['protected']): raise ValueError('empty protected term')
    alternatives = []
    for family in _english_array(result['alternatives']):
        family = _english_object(family, ['from', 'to'])
        if not _english_string(family.get('from')).strip(_english_space) or not _english_array(family.get('to')) or any(not _english_string(value).strip(_english_space) for value in family['to']): raise ValueError('empty phrase alternatives')
        alternatives.append(family)
    result['alternatives'] = alternatives
    return result

def _english_style_protected(text, terms):
    ranges = []
    for term in terms:
        ranges.extend((m.start(), m.end()) for m in _english_re.finditer(_english_re.escape(term), text))
    quoted = None
    for i, char in enumerate(text):
        if quoted is not None:
            if char == quoted[1]: ranges.append((quoted[0], i+1)); quoted = None
        elif char in ['"', '`', '“', '‘'] or char == "'" and (i == 0 or not text[i-1].isalnum()):
            quoted = (i, {'“':'”', '‘':'’'}.get(char, char))
    if quoted is not None: ranges.append((quoted[0], len(text)))
    for match in _english_re.finditer('[^' + _english_space_pattern[1:-1] + ']+', text):
        core = match.group().rstrip('.,!?;)]')
        if any(c.isnumeric() or c in '@/\\_-:.={}' for c in core): ranges.append((match.start(), match.end()))
    return ranges

def _english_style_case(source, replacement):
    return replacement[:1].upper() + replacement[1:] if source[:1].isupper() and replacement and ord(replacement[0]) < 128 else replacement

def _english_style_boundary(text, start, end):
    def word(c): return ord(c) > 127 or c.isalnum() or c in "_'’-"
    return (start == 0 or not word(text[start-1])) and (end == len(text) or not word(text[end]))

def _english_style_start(text, start):
    before = text[:start].rstrip(_english_space)
    return not before or before[-1] in '.!?\n'

def _english_style_candidates(text, options):
    data = _EnglishData.data['styles']
    protected = _english_style_protected(text, options['protected'])
    edits = []
    def push(rule, start, end, replacement, stage):
        if text[start:end] != replacement and not any(start < b and a < end for a,b in protected):
            edit = {'rule':rule, 'start':start, 'end':end, 'replacement':replacement, 'stage':stage}
            if edit not in edits: edits.append(edit)
    lower = _english_ascii_lower(text)
    for rule in data['rules']:
        if not options[rule['category']]: continue
        for match in _english_re.finditer(_english_re.escape(_english_ascii_lower(rule['from'])), lower):
            start, stop = match.span()
            source = text[start:stop]
            if source not in [rule['from'], _english_style_case('A', rule['from']), _english_ascii_lower(rule['from'])]: continue
            initial = _english_style_start(text, start)
            if not rule['from'].endswith(' ') and not _english_style_boundary(text, start, stop): continue
            if rule['from'].endswith(' ') and not initial: continue
            tail = text[stop:]
            end = next((i for i,c in enumerate(tail) if c in '.?!\n'), len(tail))
            body = tail[:end]
            words = _english_re.findall('[^' + _english_space_pattern[1:-1] + ']+', body)
            full = not tail.strip(_english_space) or tail.startswith(('.', '!', '?'))
            guard = rule['guard']
            valid = False
            if guard == 'phrase': valid = True
            elif guard == 'initial': valid = initial
            elif guard == 'opening': valid = initial and (full or tail.startswith(','))
            elif guard == 'full': valid = initial and full
            elif guard == 'base': valid = initial and _english_predicate('will', words) and not any(w in ['if','whether'] for w in words) and (not rule['from'].endswith('going to') or not words or words[0] not in ['work','school','college','church','bed','sleep','shop','practice'])
            elif guard == 'progressive': valid = initial and _english_predicate('am', words) and tail[end:end+1] != '?' and bool(tail) and tail[0] in 'abcdefghijklmnopqrstuvwxyz' and bool(words) and any(words[0] == v['present_participle'] for v in _EnglishData.data['verbs'].values()) and not any(w in ['not','if','and','but','or','because'] for w in words)
            elif guard == 'request': valid = initial and bool(words) and tail[end:end+1] == '.' and not any(c in body for c in ',;:') and not any(w in ['if','and','or','not'] for w in words)
            elif guard.startswith('aux:'): valid = initial and _english_predicate(guard[4:], words)
            if not valid: continue
            replacement = _english_style_case(source, rule['to'])
            if guard == 'progressive': replacement = text[stop:stop+1].upper(); stop += 1
            if guard == 'request': replacement += body + '?'; stop += end + 1
            push(rule['id'], start, stop, replacement, rule['stage'])
    if options['negation']:
        for match in _english_re.finditer(r'[^.?!\n]*[.?!\n]|[^.?!\n]+$', text):
            sentence = match.group()
            analysis = _english_analyze(sentence)
            if analysis is not None and analysis['edit'] is not None:
                a,b,replacement = analysis['edit']
                if "'" in replacement: push('negation:contract', match.start()+a, match.start()+b, replacement, 0)
    if options['condition_order']:
        for alternative in _english_text_alternatives(text, {'negation':False, 'condition_order':True}): push('condition:order', 0, len(text), alternative, 0)
    for index, family in enumerate(options['alternatives']):
        for match in _english_re.finditer(_english_re.escape(family['from']), text):
            start, end = match.span()
            if _english_style_boundary(text, start, end):
                for choice, replacement in enumerate(family['to']): push(f'authored:{index}:{choice}', start, end, replacement, 1)
    end = len(text.rstrip(_english_space))
    if end:
        mark, before = text[end-1:end], text[:end-1]
        words = _english_re.findall('[^' + _english_space_pattern[1:-1] + ']+', before)
        last = words[-1] if words else ''
        terminal = mark == '.' and bool(last) and '.' not in last and _english_ascii_lower(last) not in data['abbreviations']
        punctuation = options['punctuation']
        if punctuation == 'omit_period' and terminal: push('punctuation:omit_period', end-1, end, '', 3)
        elif punctuation == 'exclamation' and terminal: push('punctuation:exclamation', end-1, end, '!', 3)
        elif punctuation == 'expressive' and (terminal or mark == '!' and not before.endswith('!')): push('punctuation:expressive', end-1, end, '!!', 3)
    if options['casing'] == 'lowercase':
        for pronoun in data['contractions']:
            for match in _english_re.finditer(_english_re.escape(pronoun), text):
                if _english_style_boundary(text, *match.span()): push('casing:lowercase', match.start(), match.end(), _english_ascii_lower(pronoun), 3)
    for match in _english_re.finditer(r'[A-Za-z]+', text):
        start,end = match.span()
        word, lower = match.group(), _english_ascii_lower(match.group())
        if lower not in data['words'] and lower not in data['case_words'] or not _english_style_boundary(text, start, end): continue
        if options['casing'] == 'lowercase': push('casing:lowercase', start, end, lower, 3)
        if lower not in data['words'] or word == 'could' or len(word) < 5 or word != lower or options['typos']['probability'] == 0: continue
        for kind in options['typos']['kinds']:
            replacement = None
            if kind == 'common': replacement = next((b for a,b in data['typos'] if a == word), None)
            elif kind == 'omission': replacement = word[:2] + word[3:]
            elif kind == 'transposition' and word[1] != word[2]: replacement = word[:1] + word[2] + word[1] + word[3:]
            elif kind == 'repetition': replacement = word[:2] + word[1] + word[2:]
            if replacement is not None and replacement not in data['words'] and not any(replacement in v.values() for v in _EnglishData.data['verbs'].values()): push('typo:'+kind, start, end, replacement, 4)
    return sorted(edits, key=lambda e:(e['stage'],e['start'],e['rule'],e['replacement']))

def _english_style(value, options, random, details=False):
    options = _english_style_options(options)
    if type(value) is str: text = value
    else:
        sentence = _english_sentence(value)
        text = _english_render(sentence, options['rendering'])
        for clause in [sentence['clause']] + ([sentence['condition']] if sentence.get('condition') is not None else []):
            subject = clause['subject']['text']
            if _english_ascii_lower(subject) not in ['i','you','he','she','it','we','they']:
                options['protected'] += [subject, _english_style_case('A', subject)]
    budget, edits, gate = int(options['max_changes']), [], None
    for stage in range(5):
        if budget == 0: break
        candidates = [e for e in _english_style_candidates(text, options) if e['stage'] == stage]
        if not candidates: continue
        limit = budget
        if stage < 4:
            if gate is None: gate = options['probability'] == 1 or options['probability'] > 0 and random() < options['probability']
            if not gate: continue
        else:
            limit = min(budget, int(options['typos']['max_errors']))
            probability = options['typos']['probability']
            if limit == 0 or probability == 0 or probability < 1 and random() >= probability: continue
        for _ in range(limit):
            if not candidates: break
            index = 0 if len(candidates) == 1 else min(len(candidates)-1, int(random()*len(candidates)))
            edit = candidates.pop(index)
            start,end,replacement = edit['start'],edit['end'],edit['replacement']
            candidates = [e for e in candidates if not (start < e['end'] and e['start'] < end)]
            difference = len(replacement) - (end-start)
            for other in candidates:
                if other['start'] >= end: other['start'] += difference; other['end'] += difference
            edits.append({'rule':edit['rule'],'range':{'start':len(text[:start].encode('utf-8')),'end':len(text[:end].encode('utf-8'))},'replacement':replacement,'stage':edit['stage']})
            text = text[:start] + replacement + text[end:]
            budget -= 1
    return {'text':text,'edits':edits} if details else text
"#;

const TS_ENGLISH: &str = r#"const _englishSpace = "[\\u0009-\\u000d\\u0020\\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000]";
const _englishLeadingSpace = new RegExp("^" + _englishSpace + "+", "u");
const _englishTrailingSpace = new RegExp(_englishSpace + "+$", "u");
function _englishTrimStart(text: string): string { return text.replace(_englishLeadingSpace, ""); }
function _englishTrimEnd(text: string): string { return text.replace(_englishTrailingSpace, ""); }
function _englishTrim(text: string): string { return _englishTrimEnd(_englishTrimStart(text)); }

function _englishObject(value: any, fields: string[], defaults: any = {}): any {
    if (value === null || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => !fields.includes(key))) throw new Error("invalid English object fields");
    return {...defaults, ...value};
}
function _englishEnum(value: any, choices: string[]): string {
    if (typeof value !== "string" || !choices.includes(value)) throw new Error("invalid English option");
    return value;
}
function _englishString(value: any): string {
    if (typeof value !== "string") throw new Error("expected English string");
    return value;
}
function _englishArray(value: any): any[] {
    if (!Array.isArray(value)) throw new Error("expected English array");
    return value;
}
function _englishFragment(value: any): string {
    value = _englishString(value);
    if (!value || _englishTrim(value) !== value || /[\x00-\x1f\x7f-\x9f?!;,\u2028\u2029]/u.test(value) || value.endsWith(".") || new RegExp("\\." + _englishSpace, "u").test(value)) throw new Error("invalid English fragment");
    return value;
}
function _englishSubject(value: any): any {
    value = _englishObject(value, ["text", "person", "number"], {person: "third", number: "singular"});
    const text = _englishFragment(value.text), person = _englishEnum(value.person, ["first", "second", "third"]), number = _englishEnum(value.number, ["singular", "plural"]);
    const expectedForms: any = ({I: ["first", "singular"], we: ["first", "plural"], We: ["first", "plural"], he: ["third", "singular"], He: ["third", "singular"], she: ["third", "singular"], She: ["third", "singular"], it: ["third", "singular"], It: ["third", "singular"], they: ["third", "plural"], They: ["third", "plural"]} as any);
    const expected = Object.hasOwn(expectedForms, text) ? expectedForms[text] : undefined;
    if (expected && (expected[0] !== person || expected[1] !== number) || ["you", "You"].includes(text) && person !== "second") throw new Error("pronoun agreement mismatch");
    return value;
}
function _englishVerb(value: any): any {
    if (typeof value === "string") {
        if (!Object.hasOwn(_englishData.verbs, value)) throw new Error("unknown English verb");
        return _englishData.verbs[value];
    }
    const fields = ["base", "third_person_singular", "past", "past_participle", "present_participle"];
    value = _englishObject(value, fields);
    if (fields.some(key => typeof value[key] !== "string" || !/^[a-z]+(?![\s\S])/.test(value[key]))) throw new Error("invalid English verb forms");
    if (value.base === "be" && fields.some(key => value[key] !== _englishData.verbs.be[key])) throw new Error("invalid forms of be");
    return value;
}
function _englishClause(value: any, condition = false): any {
    const fields = ["subject", "predicate", "polarity", "tense", "aspect", ...(condition ? [] : ["modal", "form"])];
    const clause = _englishObject(value, fields, {polarity: "positive", tense: "present", aspect: "simple"});
    clause.subject = _englishSubject(clause.subject);
    const predicate = _englishObject(clause.predicate, ["verb", "complement"]);
    predicate.verb = _englishVerb(predicate.verb);
    if (predicate.complement != null) _englishFragment(predicate.complement);
    clause.predicate = predicate;
    _englishEnum(clause.polarity, ["positive", "negative"]);
    _englishEnum(clause.tense, ["present", "past", "future"]);
    _englishEnum(clause.aspect, ["simple", "progressive", "perfect", "perfect_progressive"]);
    clause.form = condition ? "statement" : _englishEnum(clause.form === undefined ? "statement" : clause.form, ["statement", "question"]);
    clause.modal = condition ? null : clause.modal ?? null;
    if (clause.modal !== null) {
        _englishEnum(clause.modal, ["can", "could", "may", "might", "must", "should", "will", "would"]);
        if (clause.tense !== "present") throw new Error("modal clauses require present tense");
    }
    return clause;
}
function _englishSentence(value: any): any {
    const sentence = _englishObject(value, ["clause", "condition"]);
    sentence.clause = _englishClause(sentence.clause);
    if (sentence.condition != null) sentence.condition = _englishClause(sentence.condition, true);
    return sentence;
}
function _englishRenderOptions(value: any): any {
    const options = _englishObject(value, ["negation", "condition_position"], {negation: "expanded", condition_position: "before"});
    _englishEnum(options.negation, ["expanded", "contracted"]);
    _englishEnum(options.condition_position, ["before", "after"]);
    return options;
}
class _EnglishUnsupported extends Error {}
function _englishContract(auxiliary: string): string | undefined {
    return _englishData.negations.find((row: string[]) => row[0] === auxiliary)?.[2];
}
function _englishStart(text: string): string {
    const words = ["if", "the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "we", "you", "he", "she", "it", "they", "can", "could", "may", "might", "must", "should", "will", "would", "am", "is", "are", "was", "were", "have", "has", "had", "do", "does", "did", ..._englishData.negations.map((row: string[]) => row[2])];
    return words.includes(text.split(" ")[0]) ? text[0].toUpperCase() + text.slice(1) : text;
}
function _englishRenderClause(clause: any, style: string): string {
    const subject = clause.subject, predicate = clause.predicate, verb = predicate.verb;
    const third = subject.person === "third" && subject.number === "singular", past = clause.tense === "past";
    const be = past ? (["first", "third"].includes(subject.person) && subject.number === "singular" ? "was" : "were") : subject.person === "first" && subject.number === "singular" ? "am" : third ? "is" : "are";
    const have = past ? "had" : third ? "has" : "have", doWord = past ? "did" : third ? "does" : "do";
    const modal = clause.modal ?? (clause.tense === "future" ? "will" : null), aspect = clause.aspect;
    const negative = clause.polarity === "negative", question = clause.form === "question";
    let first: string, rest: string;
    if (modal !== null) {
        first = modal;
        rest = ({simple: verb.base, progressive: "be " + verb.present_participle, perfect: "have " + verb.past_participle, perfect_progressive: "have been " + verb.present_participle} as any)[aspect];
    } else if (aspect === "simple") {
        if (verb.base === "be") [first, rest] = [be, ""];
        else if (negative || question) [first, rest] = [doWord, verb.base];
        else [first, rest] = [past ? verb.past : third ? verb.third_person_singular : verb.base, ""];
    } else if (aspect === "progressive") [first, rest] = [be, verb.present_participle];
    else if (aspect === "perfect") [first, rest] = [have, verb.past_participle];
    else [first, rest] = [have, "been " + verb.present_participle];
    let start: string;
    if (negative && style === "contracted") {
        const contracted = _englishContract(first);
        if (contracted === undefined) throw new _EnglishUnsupported("unsupported negation contraction");
        start = question ? contracted + " " + subject.text : subject.text + " " + contracted;
    } else if (negative) start = question ? first + " " + subject.text + " not" : subject.text + (first === "can" ? " cannot" : " " + first + " not");
    else start = question ? first + " " + subject.text : subject.text + " " + first;
    return [start, rest, predicate.complement].filter(Boolean).join(" ");
}
function _englishRender(sentence: any, options: any, fragment = false): string {
    const main = _englishRenderClause(sentence.clause, options.negation);
    let body: string;
    if (sentence.condition != null) {
        const condition = _englishRenderClause(sentence.condition, options.negation);
        body = options.condition_position === "before" ? (fragment ? "if " : "If ") + condition + ", " + main : (fragment ? main : _englishStart(main)) + " if " + condition;
    } else body = fragment ? main : _englishStart(main);
    return fragment ? body : body + (sentence.clause.form === "question" ? "?" : ".");
}
function _englishSafeWord(word: string): boolean {
    return /^[\p{L}\p{N}_-]+(?![\s\S])/u.test(word) && /[\p{L}\p{N}]/u.test(word) && !["if", "when", "unless", "because", "although", "while", "that", "whether", "who", "whom", "whose", "which", "where", "as", "or", "and", "but", "not", "no", "never", "neither", "nor", "only", "just", "merely", "except", "rather", "instead", "so"].includes(word.toLowerCase());
}
function _englishDeterminer(word: string): boolean { return ["the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "his", "her", "its"].includes(word); }
function _englishAuxiliary(word: string): boolean { return ["can", "could", "may", "might", "must", "should", "will", "would", "do", "does", "did", "am", "is", "are", "was", "were", "have", "has", "had"].includes(word); }
function _englishAnalyzeSubject(words: string[]): boolean {
    if (!words.length) return false;
    if (["i", "you", "he", "she", "it", "we", "they"].includes(words[0].toLowerCase())) return words.length === 1;
    if (_englishDeterminer(words[0].toLowerCase())) return words.length >= 2 && words.slice(1).every(word => _englishSafeWord(word) && !_englishDeterminer(word.toLowerCase()) && !_englishAuxiliary(word.toLowerCase()) && !["i", "you", "he", "she", "it", "we", "they", "to"].includes(word.toLowerCase()));
    if (words.length === 1 && _englishSafeWord(words[0]) && /\p{L}/u.test(words[0]) && /[_0-9]/.test(words[0])) return true;
    return words.length <= 3 && words.every(word => _englishSafeWord(word) && /^\p{Lu}/u.test(word) && !_englishAuxiliary(word.toLowerCase()));
}
function _englishAgrees(auxiliary: string, words: string[]): boolean {
    if (words.length !== 1) return true;
    const agreementForms: any = ({i: ["am", "was", "do", "have"], you: ["are", "were", "do", "have"], we: ["are", "were", "do", "have"], they: ["are", "were", "do", "have"], he: ["is", "was", "does", "has"], she: ["is", "was", "does", "has"], it: ["is", "was", "does", "has"]} as any);
    const key = words[0].toLowerCase(), allowed = Object.hasOwn(agreementForms, key) ? agreementForms[key] : undefined;
    return allowed === undefined || !["am", "is", "are", "was", "were", "do", "does", "have", "has"].includes(auxiliary) || allowed.includes(auxiliary);
}
function _englishBeComplement(words: string[]): boolean {
    if (!words.length) return false;
    if (Object.values(_englishData.verbs).some((verb: any) => [verb.present_participle, verb.past_participle].includes(words[0]))) return true;
    if (_englishDeterminer(words[0]) || ["in", "on", "at", "near", "under", "above", "outside", "inside", "with", "without"].includes(words[0])) return words.length > 1;
    return ["ready", "late", "early", "available", "unavailable", "hungry", "thirsty", "open", "closed", "complete", "correct", "incorrect", "certain", "safe", "sure", "possible", "impossible", "necessary", "happy", "sad", "tired", "well", "ill", "here", "there", "alone", "able", "unable", "busy", "free", "valid", "invalid", "empty", "full", "active", "inactive", "online", "offline", "clear", "wrong", "right"].includes(words[0]);
}
function _englishPerfect(words: string[]): boolean { return words.length > 0 && (words[0] === "been" ? _englishBeComplement(words.slice(1)) : Object.values(_englishData.verbs).some((verb: any) => verb.past_participle === words[0])); }
function _englishBase(words: string[]): boolean {
    if (!words.length) return false;
    if (words[0] === "have" && words[1] === "to") return words.length > 2 && Object.hasOwn(_englishData.verbs, words[2]);
    return Object.hasOwn(_englishData.verbs, words[0]);
}
function _englishPredicate(auxiliary: string, words: string[]): boolean {
    if (!words.length || !words.every(_englishSafeWord)) return false;
    if (["do", "does", "did"].includes(auxiliary)) return _englishBase(words);
    if (["am", "is", "are", "was", "were"].includes(auxiliary)) return _englishBeComplement(words);
    if (["have", "has", "had"].includes(auxiliary)) return _englishPerfect(words);
    if (words[0] === "be") return _englishBeComplement(words.slice(1));
    if (words[0] === "have" && words.length > 1 && (words[1] === "been" || Object.values(_englishData.verbs).some((verb: any) => verb.past_participle === words[1]))) return _englishPerfect(words.slice(1));
    return _englishBase(words);
}
function _englishAnalyze(text: string): any {
    const trimmed = _englishTrim(text);
    if (!/[.?](?![\s\S])/.test(trimmed)) return null;
    const body = trimmed.slice(0, -1), question = trimmed.endsWith("?");
    if (/[\x00-\x08\x0a-\x1f\x7f-\x9f\u2028\u2029.?!]/u.test(body)) return null;
    const matches = [...body.matchAll(new RegExp("[^" + _englishSpace.slice(1, -1) + "]+", "gu"))], words = matches.map(match => match[0]);
    if (question) {
        if (!words.length || !_englishAuxiliary(words[0].toLowerCase())) return null;
        let found = 0;
        for (let end = 2; end < Math.min(words.length, 7); end++) if (_englishAnalyzeSubject(words.slice(1, end)) && _englishAgrees(words[0].toLowerCase(), words.slice(1, end)) && _englishPredicate(words[0].toLowerCase(), words.slice(end))) found++;
        return found === 1 ? {edit: null} : null;
    }
    let found: any = null;
    for (let index = 1; index < Math.min(words.length, 6); index++) {
        if (!_englishAnalyzeSubject(words.slice(0, index))) continue;
        const word = words[index], expansion = _englishData.negations.find((row: string[]) => row.slice(2).includes(word));
        let auxiliary: string, edit: any = null, start = index + 1;
        if (expansion) { auxiliary = expansion[0]; edit = [matches[index].index!, matches[index].index! + word.length, expansion[1]]; }
        else if (word === "cannot") { auxiliary = "can"; edit = [matches[index].index!, matches[index].index! + word.length, "can't"]; }
        else if (_englishAuxiliary(word)) {
            auxiliary = word;
            if (words[start] === "not") {
                if (word === "can") continue;
                const contracted = _englishContract(word);
                if (contracted !== undefined) edit = [matches[index].index!, matches[start].index! + words[start].length, contracted];
                start++;
            }
        } else continue;
        if (_englishAgrees(auxiliary, words.slice(0, index)) && _englishPredicate(auxiliary, words.slice(start))) {
            if (found !== null) return null;
            if (edit !== null) { const offset = text.length - _englishTrimStart(text).length; edit[0] += offset; edit[1] += offset; }
            found = {edit};
        }
    }
    return found ?? (_englishLexical(words) ? {edit: null} : null);
}
function _englishVary(value: any, options: any, random: () => number): string {
    if (options !== null && typeof options === "object" && !Array.isArray(options) && Object.keys(options).some(k => !["negation", "condition_order", "rendering"].includes(k))) return _englishStyle(value, options, random);
    options = _englishObject(options, ["negation", "condition_order", "rendering"], {negation: true, condition_order: true, rendering: {}});
    if (typeof options.negation !== "boolean" || typeof options.condition_order !== "boolean") throw new Error("variation controls must be booleans");
    const rendering = _englishRenderOptions(options.rendering);
    if (typeof value === "string") {
        const alternatives = _englishTextAlternatives(value, options);
        return alternatives.length ? [value, ...alternatives][Math.min(alternatives.length, Math.floor(random() * (alternatives.length + 1)))] : value;
    }
    const sentence = _englishSentence(value), original = _englishRender(sentence, rendering), alternatives = [original];
    const negative = sentence.clause.polarity === "negative" || sentence.condition != null && sentence.condition.polarity === "negative";
    const negativeQuestion = sentence.clause.form === "question" && sentence.clause.polarity === "negative";
    if (options.negation && negative && !negativeQuestion) {
        try { alternatives.push(_englishRender(sentence, {...rendering, negation: rendering.negation === "expanded" ? "contracted" : "expanded"})); }
        catch (error) { if (!(error instanceof _EnglishUnsupported)) throw error; }
    }
    if (options.condition_order && sentence.condition != null) alternatives.push(_englishRender(sentence, {...rendering, condition_position: rendering.condition_position === "before" ? "after" : "before"}));
    return alternatives.length > 1 ? alternatives[Math.min(alternatives.length - 1, Math.floor(random() * alternatives.length))] : original;
}
function _englishText(value: any): string {
    value = _englishString(value);
    if (!value || _englishTrim(value) !== value || /[\x00-\x1f\x7f-\x9f\u2028\u2029]/.test(value)) throw new Error("invalid context text");
    return value;
}
function _englishEntity(value: any): any {
    value = _englishObject(value, ["id", "label", "person", "number", "pronouns"], {person: "third", number: "singular", pronouns: null});
    _englishText(value.id); _englishText(value.label);
    _englishEnum(value.person, ["first", "second", "third"]); _englishEnum(value.number, ["singular", "plural"]);
    if (value.pronouns !== null) {
        const fields = ["subject", "object", "possessive_determiner", "possessive"], forms = _englishObject(value.pronouns, fields);
        if (fields.some(key => typeof forms[key] !== "string" || !/^[A-Za-z]+(?![\s\S])/.test(forms[key]))) throw new Error("invalid pronoun forms");
        value.pronouns = forms;
    }
    return value;
}
function _englishContext(value: any): any {
    value = _englishObject(value, ["entities", "mentioned", "facts"], {entities: [], mentioned: [], facts: []});
    value.entities = _englishArray(value.entities).map(_englishEntity);
    value.mentioned = _englishArray(value.mentioned).map(_englishText);
    value.facts = _englishArray(value.facts).map(_englishText);
    const ids = value.entities.map((entity: any) => entity.id);
    if (new Set(ids).size !== ids.length || new Set(value.mentioned).size !== value.mentioned.length || new Set(value.facts).size !== value.facts.length || value.mentioned.some((id: string) => !ids.includes(id))) throw new Error("invalid discourse context");
    return value;
}
function _englishEntityEqual(left: any, right: any): boolean {
    return ["id", "label", "person", "number"].every(key => left[key] === right[key]) && (left.pronouns === null ? right.pronouns === null : right.pronouns !== null && ["subject", "object", "possessive_determiner", "possessive"].every(key => left.pronouns[key] === right.pronouns[key]));
}
function _englishRefer(entity: any, context: any, options: any): string {
    entity = _englishEntity(entity); context = _englishContext(context);
    options = _englishObject(options, ["form", "style"], {form: "subject", style: "auto"});
    const form = _englishEnum(options.form, ["subject", "object", "possessive_determiner", "possessive"]), style = _englishEnum(options.style, ["auto", "name", "pronoun"]);
    if (context.entities.some((other: any) => other.id === entity.id && !_englishEntityEqual(other, entity))) throw new Error("conflicting entity definition");
    const candidate = entity.pronouns === null ? null : entity.pronouns[form];
    const unambiguous = candidate !== null && !context.entities.some((other: any) => other.id !== entity.id && context.mentioned.includes(other.id) && (other.pronouns === null || other.pronouns[form].toLowerCase() === candidate.toLowerCase()));
    if (style !== "name" && context.mentioned.includes(entity.id) && unambiguous) return candidate;
    if (style === "pronoun") throw new Error("ambiguous or unintroduced pronoun");
    return ["subject", "object"].includes(form) ? entity.label : entity.label + (/[sS](?![\s\S])/.test(entity.label) ? "'" : "'s");
}
function _englishRemember(context: any, entities: any, facts: any): any {
    context = _englishContext(context);
    for (const entity of _englishArray(entities).map(_englishEntity)) {
        const old = context.entities.find((other: any) => other.id === entity.id);
        if (old !== undefined && !_englishEntityEqual(old, entity)) throw new Error("conflicting entity definition");
        if (old === undefined) context.entities.push(entity);
        if (!context.mentioned.includes(entity.id)) context.mentioned.push(entity.id);
    }
    for (const fact of _englishArray(facts).map(_englishText)) if (!context.facts.includes(fact)) context.facts.push(fact);
    return context;
}
function _englishUnmentioned(details: any, context: any): any[] {
    context = _englishContext(context);
    const seen = new Map<string, string>(), selected = [];
    for (let detail of _englishArray(details)) {
        detail = _englishObject(detail, ["id", "text"]);
        const id = _englishText(detail.id), text = _englishText(detail.text);
        if (seen.has(id)) { if (seen.get(id) !== text) throw new Error("conflicting detail text"); continue; }
        seen.set(id, text);
        if (!context.facts.includes(id)) selected.push(detail);
    }
    return selected;
}
function _englishLocale(value: any): string { return _englishEnum(value, ["en-US", "en-GB", "en-CA", "en-AU", "en-IN"]); }
function _englishNumber(value: any, options: any): string {
    options = _englishObject(options, ["locale", "grouping", "fraction_digits"], {locale: "en-US", grouping: true, fraction_digits: null});
    const locale = _englishLocale(options.locale), precision = options.fraction_digits;
    if (typeof options.grouping !== "boolean" || precision !== null && (!Number.isInteger(precision) || precision < 0 || precision > 9)) throw new Error("invalid number options");
    if (typeof value === "number") { if (!Number.isFinite(value)) throw new Error("invalid number"); value = String(value); }
    value = _englishString(value);
    const match = /^([+-]?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?(?![\s\S])/.exec(value);
    if (!match || value.length > 4096) throw new Error("invalid decimal");
    const sign = match[1], exponent = Number(match[4] ?? "0");
    if (!Number.isSafeInteger(exponent) || Math.abs(exponent) > 4096) throw new Error("decimal expansion too large");
    let integer = match[2], fraction = match[3] ?? "", digits = integer + fraction, point = integer.length + exponent;
    if (point <= 0 && 1 - point + digits.length > 4096 || point >= digits.length && point > 4096) throw new Error("decimal expansion too large");
    if (point < 0) { digits = "0".repeat(-point) + digits; point = 0; }
    if (point > digits.length) digits += "0".repeat(point - digits.length);
    integer = digits.slice(0, point).replace(/^0+/, "") || "0"; fraction = digits.slice(point);
    if (integer.length + fraction.length > 4096) throw new Error("decimal expansion too large");
    if (precision === null) fraction = fraction.replace(/0+(?![\s\S])/, "");
    else {
        const roundUp = fraction.length > precision && fraction[precision] >= "5";
        fraction = (fraction + "0".repeat(precision)).slice(0, precision);
        if (roundUp) {
            const combined = (integer + fraction).split("");
            let index = combined.length - 1;
            while (index >= 0 && combined[index] === "9") combined[index--] = "0";
            if (index >= 0) combined[index] = String(Number(combined[index]) + 1); else combined.unshift("1");
            const rounded = combined.join("");
            integer = precision ? rounded.slice(0, -precision) : rounded;
            fraction = precision ? rounded.slice(-precision) : "";
        }
    }
    const negative = sign === "-" && /[1-9]/.test(integer + fraction);
    if (options.grouping) {
        const parts = []; let width = 3;
        while (integer) { parts.push(integer.slice(-width)); integer = integer.slice(0, -width); if (locale === "en-IN") width = 2; }
        integer = parts.reverse().join(",");
    }
    return (negative ? "-" : "") + integer + (fraction ? "." + fraction : "");
}
function _englishDate(value: any, options: any): string {
    options = _englishObject(options, ["locale", "style"], {locale: "en-US", style: "long"});
    const locale = _englishLocale(options.locale), style = _englishEnum(options.style, ["short", "long"]);
    value = _englishString(value);
    if (!/^[0-9]{4}-[0-9]{2}-[0-9]{2}(?![\s\S])/.test(value)) throw new Error("invalid date");
    const [year, month, day] = value.split("-").map(Number), leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
    const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1 || day > days[month - 1]) throw new Error("invalid date");
    if (style === "short") { if (locale === "en-CA") return value; return locale === "en-US" ? `${String(month).padStart(2, "0")}/${String(day).padStart(2, "0")}/${String(year).padStart(4, "0")}` : `${String(day).padStart(2, "0")}/${String(month).padStart(2, "0")}/${String(year).padStart(4, "0")}`; }
    const name = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][month - 1];
    return ["en-US", "en-CA"].includes(locale) ? `${name} ${day}, ${year}` : `${day} ${name} ${year}`;
}
function _englishTime(value: any, options: any): string {
    options = _englishObject(options, ["locale", "hour_cycle", "seconds"], {locale: "en-US", hour_cycle: null, seconds: false});
    const locale = _englishLocale(options.locale), cycle = options.hour_cycle === null ? locale === "en-GB" ? "h24" : "h12" : _englishEnum(options.hour_cycle, ["h12", "h24"]);
    if (typeof options.seconds !== "boolean") throw new Error("seconds must be boolean");
    value = _englishString(value);
    if (!/^[0-9]{2}:[0-9]{2}(?::[0-9]{2})?(?![\s\S])/.test(value)) throw new Error("invalid time");
    const parts = value.split(":").map(Number), [hour, minute] = parts, second = parts[2] ?? 0;
    if (hour > 23 || minute > 59 || second > 59) throw new Error("invalid time");
    let result = cycle === "h24" ? `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}` : `${hour % 12 || 12}:${String(minute).padStart(2, "0")}`;
    if (options.seconds) result += `:${String(second).padStart(2, "0")}`;
    if (cycle === "h12") result += hour < 12 ? " AM" : " PM";
    return result;
}
function _language_english(operation: string, args: any[], random: () => number): any {
    const arities: any = {realize: [1, 2], vary: [1, 2], combine: [2, 3], refer: [2, 3], remember: [2, 3], unmentioned: [2, 2], format_number: [1, 2], format_date: [1, 2], format_time: [1, 2]};
    if (!Object.hasOwn(arities, operation) || !Array.isArray(args) || args.length < arities[operation][0] || args.length > arities[operation][1]) throw new Error("invalid English call");
    if (operation === "realize") return _englishRender(_englishSentence(args[0]), _englishRenderOptions(args.length > 1 ? args[1] : {}));
    if (operation === "vary") return _englishVary(args[0], args.length > 1 ? args[1] : {}, random);
    if (operation === "combine") {
        const sentences = _englishArray(args[0]).map(value => _englishSentence(value)), relation = _englishEnum(args[1], ["and", "but", "or", "because"]), options = _englishRenderOptions(args.length > 2 ? args[2] : {});
        if (sentences.length < 2 || relation === "because" && sentences.length !== 2 || sentences.some(sentence => sentence.clause.form !== "statement" || sentence.condition != null)) throw new Error("invalid proposition combination");
        if (relation === "because") return "Because " + _englishRender(sentences[1], options, true) + ", " + _englishRender(sentences[0], options, true) + ".";
        const separator = ", " + relation + " ";
        return _englishRender(sentences[0], options).slice(0, -1) + separator + sentences.slice(1).map(sentence => _englishRender(sentence, options, true)).join(separator) + ".";
    }
    if (operation === "refer") return _englishRefer(args[0], args[1], args.length > 2 ? args[2] : {});
    if (operation === "remember") return _englishRemember(args[0], args[1], args.length > 2 ? args[2] : []);
    if (operation === "unmentioned") return _englishUnmentioned(args[0], args[1]);
    if (operation === "format_number") return _englishNumber(args[0], args.length > 1 ? args[1] : {});
    if (operation === "format_date") return _englishDate(args[0], args.length > 1 ? args[1] : {});
    return _englishTime(args[0], args.length > 1 ? args[1] : {});
}
function _englishLexical(words: string[]): boolean {
    let found = 0;
    for (let index = 1; index < Math.min(words.length, 6); index++) {
        if (!_englishAnalyzeSubject(words.slice(0, index)) || !words.slice(index).every(_englishSafeWord)) continue;
        const pronoun = index === 1 ? words[0].toLowerCase() : null;
        const valid = Object.values(_englishData.verbs).some((verb: any) => verb.base !== "be" && (words[index] === verb.past || (["he", "she", "it"].includes(pronoun ?? "") ? words[index] === verb.third_person_singular : ["i", "you", "we", "they"].includes(pronoun ?? "") ? words[index] === verb.base : [verb.base, verb.third_person_singular].includes(words[index]))));
        if (valid) found++;
    }
    return found === 1;
}
function _englishInitialCase(text: string, question: boolean, upper: boolean): string {
    const end = text.indexOf(" ") === -1 ? text.length : text.indexOf(" "), first = text.slice(0, end), lower = first.toLowerCase(), tail = text.slice(end + 1);
    const pronoun = ["you", "we", "they", "he", "she", "it"].includes(lower), determiner = _englishDeterminer(lower) && /^\p{Ll}/u.test(tail), auxiliary = question && _englishAuxiliary(lower);
    if (!(pronoun || determiner || auxiliary) || !/^[\x00-\x7f]+(?![\s\S])/.test(first)) return text;
    return (upper ? text[0].toUpperCase() : text[0].toLowerCase()) + text.slice(1);
}
function _englishTextAlternatives(text: string, options: any): string[] {
    if (!options.negation && !options.condition_order) return [];
    const edits: any[] = [], trimmed = _englishTrim(text), leading = text.length - _englishTrimStart(text).length, trailing = _englishTrimEnd(text).length;
    let reordered: string | null = null, conditional: any = null;
    if (/[.?](?![\s\S])/.test(trimmed)) {
        const body = trimmed.slice(0, -1), punctuation = trimmed.slice(-1);
        if (!/[\x00-\x1f\x7f-\x9f\u2028\u2029.?!]/u.test(body)) {
            if ((body.startsWith("If ") || body.startsWith("if ")) && body.slice(3).includes(", ")) {
                const split = body.indexOf(", ", 3), condition = body.slice(3, split), main = body.slice(split + 2);
                if (condition && main && !main.includes(",")) conditional = {condition, main, conditionOffset: leading + 3, mainOffset: leading + split + 2, punctuation, before: true};
            } else if (body.includes(" if ")) {
                const split = body.indexOf(" if "), main = body.slice(0, split), condition = body.slice(split + 4);
                if (condition && main && !condition.includes(",") && !main.includes(",")) conditional = {condition, main, conditionOffset: leading + split + 4, mainOffset: leading, punctuation, before: false};
            }
        }
    }
    if (conditional !== null) {
        const {condition, main, conditionOffset, mainOffset, punctuation, before} = conditional;
        for (const [clause, ending, offset] of [[condition, ".", conditionOffset], [main, punctuation, mainOffset]]) {
            const analysis = _englishAnalyze(clause + ending);
            if (analysis === null) return [];
            if (analysis.edit !== null) { const [start, end, replacement] = analysis.edit; edits.push([offset + start, offset + end, replacement]); }
        }
        if (options.condition_order) {
            const adjusted = _englishInitialCase(main, punctuation === "?", before), changed = before ? adjusted + " if " + condition + punctuation : "If " + condition + ", " + adjusted + punctuation;
            reordered = text.slice(0, leading) + changed + text.slice(trailing);
        }
    } else {
        const segments = [...text.matchAll(new RegExp("[^.?]*(?:\\?" + _englishSpace + "*|\\.(?:" + _englishSpace + "+|$))", "gu"))];
        if (!segments.length || segments.map(segment => segment[0]).join("") !== text) return [];
        for (const [index, segment] of segments.entries()) {
            if (index && _englishTrimEnd(segments[index - 1][0]).endsWith(".") && /^\p{Lowercase}/u.test(segment[0]) && !/[\n\r\u0085\u2028\u2029]/u.test(segments[index - 1][0])) return [];
            const analysis = _englishAnalyze(segment[0]);
            if (analysis === null) return [];
            if (analysis.edit !== null) { const [start, end, replacement] = analysis.edit; edits.push([segment.index! + start, segment.index! + end, replacement]); }
        }
    }
    edits.sort((a, b) => a[0] - b[0]);
    const alternatives: string[] = [];
    if (options.negation) for (const contracts of [false, true]) {
        let changed = text;
        for (const [start, end, replacement] of [...edits].reverse()) {
            const targetContracts = !_englishData.negations.some((row: string[]) => row[1] === replacement);
            if (targetContracts === contracts) changed = changed.slice(0, start) + replacement + changed.slice(end);
        }
        if (changed !== text && !alternatives.includes(changed)) alternatives.push(changed);
    }
    if (reordered !== null && reordered !== text && !alternatives.includes(reordered)) alternatives.push(reordered);
    return alternatives;
}

function _englishAsciiLower(text: string): string { return text.replace(/[A-Z]/g, c => c.toLowerCase()); }
function _englishStyleOptions(value: any): any {
    const data = _englishData.styles;
    const options = _englishObject(value, Object.keys(data.defaults));
    if (options.style != null) _englishEnum(options.style, Object.keys(data.presets));
    const result = {...(options.style != null ? data.presets[options.style] : data.defaults)};
    const optional = ['style','negation','condition_order','contractions','shorthand','casual','slang','reductions','fragments','warmth','soften_requests','punctuation','casing'];
    for (const [key, value] of Object.entries(options)) if (value !== null || !optional.includes(key)) result[key] = value;
    for (const key of optional.slice(1,-2)) if (typeof result[key] !== 'boolean') throw new Error('style controls must be booleans');
    _englishEnum(result.punctuation, ['preserve','omit_period','exclamation','expressive']);
    _englishEnum(result.casing, ['preserve','lowercase']);
    result.rendering = _englishRenderOptions(result.rendering);
    result.typos = _englishObject(result.typos, ['probability','max_errors','kinds'], data.defaults.typos);
    for (const value of [result.probability,result.typos.probability]) if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1) throw new Error('invalid style probability');
    for (const value of [result.max_changes,result.typos.max_errors]) if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 16) throw new Error('invalid style budget');
    for (const kind of _englishArray(result.typos.kinds)) _englishEnum(kind, ['common','omission','transposition','repetition']);
    result.protected = [..._englishArray(result.protected)];
    if (result.protected.some((term: any) => !_englishString(term))) throw new Error('empty protected term');
    result.alternatives = _englishArray(result.alternatives).map((family: any) => {
        family = _englishObject(family, ['from','to']);
        if (!_englishTrim(_englishString(family.from)) || !_englishArray(family.to).length || family.to.some((s: any) => !_englishTrim(_englishString(s)))) throw new Error('empty phrase alternatives');
        return family;
    });
    return result;
}
function _englishStyleProtected(text: string, terms: string[]): [number,number][] {
    const ranges: [number,number][] = [];
    for (const term of terms) {
        let start = text.indexOf(term);
        while (start >= 0) { ranges.push([start,start+term.length]); start = text.indexOf(term,start+term.length); }
    }
    let quoted: [number,string] | null = null;
    for (let i=0; i<text.length;) {
        const char = String.fromCodePoint(text.codePointAt(i)!);
        if (quoted !== null) { if (char === quoted[1]) { ranges.push([quoted[0],i+char.length]); quoted = null; } }
        else if (['"','`','“','‘'].includes(char) || char === "'" && (i === 0 || !/[\p{L}\p{N}]/u.test([...text.slice(0,i)].at(-1)!))) {
            quoted = [i,char === '“' ? '”' : char === '‘' ? '’' : char];
        }
        i += char.length;
    }
    if (quoted !== null) ranges.push([quoted[0],text.length]);
    for (const match of text.matchAll(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu'))) {
        const core = match[0].replace(/[.,!?;)\]]+$/u, '');
        if (/[\p{N}@/\\_\-:.={}]/u.test(core)) ranges.push([match.index!,match.index!+match[0].length]);
    }
    return ranges;
}
function _englishStyleCase(source: string, replacement: string): string {
    return source && /\p{Lu}/u.test([...source][0]) && replacement && replacement.charCodeAt(0) < 128 ? replacement[0].toUpperCase()+replacement.slice(1) : replacement;
}
function _englishStyleBoundary(text: string, start: number, end: number): boolean {
    const word = (c: string) => c.codePointAt(0)! > 127 || /[\p{L}\p{N}_'’\-]/u.test(c);
    return (start === 0 || !word([...text.slice(0,start)].at(-1)!)) && (end === text.length || !word([...text.slice(end)][0]));
}
function _englishStyleStart(text: string, start: number): boolean {
    const before = _englishTrimEnd(text.slice(0,start));
    return !before || '.!?\n'.includes(before.at(-1)!);
}
function _englishStyleCandidates(text: string, options: any): any[] {
    const data = _englishData.styles, protectedRanges = _englishStyleProtected(text, options.protected), edits: any[] = [];
    const push = (rule: string, start: number, end: number, replacement: string, stage: number) => {
        if (text.slice(start,end) !== replacement && !protectedRanges.some(([a,b]) => start < b && a < end)) {
            const edit = {rule,start,end,replacement,stage};
            if (!edits.some(e => e.rule === rule && e.start === start && e.end === end && e.replacement === replacement && e.stage === stage)) edits.push(edit);
        }
    };
    const lower = _englishAsciiLower(text);
    for (const rule of data.rules) {
        if (!options[rule.category]) continue;
        let cursor = 0;
        while (cursor <= text.length) {
            const start = lower.indexOf(_englishAsciiLower(rule.from),cursor);
            if (start < 0) break;
            let stop = start+rule.from.length;
            cursor = stop;
            const source = text.slice(start,stop);
            if (![rule.from,_englishStyleCase('A',rule.from),_englishAsciiLower(rule.from)].includes(source)) continue;
            const initial = _englishStyleStart(text,start);
            if (!rule.from.endsWith(' ') && !_englishStyleBoundary(text,start,stop) || rule.from.endsWith(' ') && !initial) continue;
            const tail = text.slice(stop), found = tail.search(/[.?!\n]/), end = found < 0 ? tail.length : found, body = tail.slice(0,end);
            const words: string[] = body.match(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu')) ?? [];
            const full = !_englishTrim(tail) || /^[.!?]/.test(tail), guard = rule.guard;
            let valid = false;
            if (guard === 'phrase') valid = true;
            else if (guard === 'initial') valid = initial;
            else if (guard === 'opening') valid = initial && (full || tail.startsWith(','));
            else if (guard === 'full') valid = initial && full;
            else if (guard === 'base') valid = initial && _englishPredicate('will',words) && !words.some(w => ['if','whether'].includes(w)) && (!rule.from.endsWith('going to') || !['work','school','college','church','bed','sleep','shop','practice'].includes(words[0]));
            else if (guard === 'progressive') valid = initial && _englishPredicate('am',words) && tail[end] !== '?' && /^[a-z]/.test(tail) && words.length > 0 && Object.values(_englishData.verbs).some((v: any) => words[0] === v.present_participle) && !words.some(w => ['not','if','and','but','or','because'].includes(w));
            else if (guard === 'request') valid = initial && words.length > 0 && tail[end] === '.' && !/[,;:]/.test(body) && !words.some(w => ['if','and','or','not'].includes(w));
            else if (guard.startsWith('aux:')) valid = initial && _englishPredicate(guard.slice(4),words);
            if (!valid) continue;
            let replacement = _englishStyleCase(source,rule.to);
            if (guard === 'progressive') { replacement = text[stop].toUpperCase(); stop++; }
            if (guard === 'request') { replacement += body+'?'; stop += end+1; }
            push(rule.id,start,stop,replacement,rule.stage);
        }
    }
    if (options.negation) {
        for (const match of text.matchAll(/[^.?!\n]*[.?!\n]|[^.?!\n]+$/g)) {
            const analysis = _englishAnalyze(match[0]);
            if (analysis?.edit) {
                const [a,b,replacement] = analysis.edit;
                if (replacement.includes("'")) push('negation:contract',match.index!+a,match.index!+b,replacement,0);
            }
        }
    }
    if (options.condition_order) for (const alternative of _englishTextAlternatives(text,{negation:false,condition_order:true})) push('condition:order',0,text.length,alternative,0);
    options.alternatives.forEach((family: any,index: number) => {
        let start = text.indexOf(family.from);
        while (start >= 0) {
            const end = start+family.from.length;
            if (_englishStyleBoundary(text,start,end)) family.to.forEach((replacement: string,choice: number) => push(`authored:${index}:${choice}`,start,end,replacement,1));
            start = text.indexOf(family.from,end);
        }
    });
    const end = _englishTrimEnd(text).length;
    if (end) {
        const mark = text[end-1], before = text.slice(0,end-1), words = before.match(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu')) ?? [], last = words.at(-1) ?? '';
        const terminal = mark === '.' && !!last && !last.includes('.') && !data.abbreviations.includes(_englishAsciiLower(last));
        if (options.punctuation === 'omit_period' && terminal) push('punctuation:omit_period',end-1,end,'',3);
        else if (options.punctuation === 'exclamation' && terminal) push('punctuation:exclamation',end-1,end,'!',3);
        else if (options.punctuation === 'expressive' && (terminal || mark === '!' && !before.endsWith('!'))) push('punctuation:expressive',end-1,end,'!!',3);
    }
    if (options.casing === 'lowercase') for (const pronoun of data.contractions) {
        let start = text.indexOf(pronoun);
        while (start >= 0) {
            const end = start+pronoun.length;
            if (_englishStyleBoundary(text,start,end)) push('casing:lowercase',start,end,_englishAsciiLower(pronoun),3);
            start = text.indexOf(pronoun,end);
        }
    }
    for (const match of text.matchAll(/[A-Za-z]+/g)) {
        const start = match.index!, end = start+match[0].length, word = match[0], lower = _englishAsciiLower(word);
        if (!(data.words.includes(lower) || data.case_words.includes(lower)) || !_englishStyleBoundary(text,start,end)) continue;
        if (options.casing === 'lowercase') push('casing:lowercase',start,end,lower,3);
        if (!data.words.includes(lower) || word === 'could' || word.length < 5 || word !== lower || options.typos.probability === 0) continue;
        for (const kind of options.typos.kinds) {
            let replacement: string | undefined;
            if (kind === 'common') replacement = data.typos.find(([a,b]: string[]) => a === word)?.[1];
            else if (kind === 'omission') replacement = word.slice(0,2)+word.slice(3);
            else if (kind === 'transposition' && word[1] !== word[2]) replacement = word[0]+word[2]+word[1]+word.slice(3);
            else if (kind === 'repetition') replacement = word.slice(0,2)+word[1]+word.slice(2);
            if (replacement !== undefined && !data.words.includes(replacement) && !Object.values(_englishData.verbs).some((v: any) => Object.values(v).includes(replacement))) push('typo:'+kind,start,end,replacement,4);
        }
    }
    const compare = (a: string,b: string) => a < b ? -1 : a > b ? 1 : 0;
    return edits.sort((a,b) => a.stage-b.stage || a.start-b.start || compare(a.rule,b.rule) || compare(a.replacement,b.replacement));
}
function _englishStyle(value: any, supplied: any, random: () => number, details = false): any {
    const options = _englishStyleOptions(supplied);
    let text: string;
    if (typeof value === 'string') text = value;
    else {
        const sentence = _englishSentence(value);
        text = _englishRender(sentence,options.rendering);
        for (const clause of [sentence.clause,...(sentence.condition != null ? [sentence.condition] : [])]) {
            const subject = clause.subject.text;
            if (!['i','you','he','she','it','we','they'].includes(_englishAsciiLower(subject))) options.protected.push(subject,_englishStyleCase('A',subject));
        }
    }
    const edits: any[] = [];
    let budget = options.max_changes, gate: boolean | null = null;
    for (let stage=0; stage<=4; stage++) {
        if (!budget) break;
        let candidates = _englishStyleCandidates(text,options).filter(e => e.stage === stage);
        if (!candidates.length) continue;
        let limit = budget;
        if (stage < 4) {
            if (gate === null) gate = options.probability === 1 || options.probability > 0 && random() < options.probability;
            if (!gate) continue;
        } else {
            limit = Math.min(budget,options.typos.max_errors);
            const probability = options.typos.probability;
            if (!limit || probability === 0 || probability < 1 && random() >= probability) continue;
        }
        for (let i=0; i<limit; i++) {
            if (!candidates.length) break;
            const index = candidates.length === 1 ? 0 : Math.min(candidates.length-1,Math.floor(random()*candidates.length));
            const edit = candidates.splice(index,1)[0], {start,end,replacement} = edit;
            candidates = candidates.filter(e => !(start < e.end && e.start < end));
            const difference = replacement.length-(end-start);
            for (const other of candidates) if (other.start >= end) {other.start += difference; other.end += difference;}
            edits.push({rule:edit.rule,range:{start:new TextEncoder().encode(text.slice(0,start)).length,end:new TextEncoder().encode(text.slice(0,end)).length},replacement,stage:edit.stage});
            text = text.slice(0,start)+replacement+text.slice(end);
            budget--;
        }
    }
    return details ? {text,edits} : text;
}
"#;

pub(in crate::mocking::code) fn english_source(lang: ScorerLang, agent: bool) -> String {
    let data = crate::english::runtime_data().to_string();
    match lang {
        ScorerLang::Python => format!(
            "import json as _english_json\nimport re as _english_re\nimport math as _english_math\n\nclass _EnglishData:\n    data = _english_json.loads({})\n\n{}\n{}",
            serde_json::to_string(&data).unwrap(),
            PY_ENGLISH,
            if agent {
                "def _english(operation, args, random_source):\n    return _language_english(operation, _value(args), lambda: _draw(random_source))\n"
            } else {
                ""
            }
        ),
        ScorerLang::Typescript => format!(
            "const _englishData: any = {data};\n{}\n{}",
            TS_ENGLISH,
            if agent {
                "function _english(operation: string, args: unknown[], randomSource: any): any { return _language_english(operation, _serialize(args), () => _draw(randomSource)); }\n"
            } else {
                ""
            }
        ),
    }
}

#[cfg(all(test, feature = "native"))]
mod english_tests {
    use super::*;
    use crate::mocking::data::{self, Distribution};
    use serde_json::{Value, json};
    use std::{
        io::Write,
        process::{Command, Stdio},
        time::{Duration, UNIX_EPOCH},
    };

    fn literal(value: &Value) -> String {
        match value {
            Value::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", literal(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Array(items) => format!("[{}]", items.iter().map(literal).collect::<Vec<_>>().join(", ")),
            value => value.to_string(),
        }
    }

    fn native(operation: &str, args: &[Value]) -> Value {
        let source = format!(
            "trace \"language\" {{ output = {operation}({}) }}",
            args.iter().map(literal).collect::<Vec<_>>().join(", ")
        );
        let model = crate::dsl::semantics::compile(&source).unwrap_or_else(|error| panic!("{source}: {error:?}"));
        match data::generate(
            model,
            1,
            Duration::from_secs(1),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(10),
            42,
            &(),
        ) {
            Ok(batch) => json!({"value":batch.events[0].output.as_ref().unwrap().project(&()).unwrap()}),
            Err(_) => json!({"error":true}),
        }
    }

    #[test]
    fn portable_english_matches_native_values_errors_and_text_alternatives() {
        let sentence = json!({"clause":{"subject":{"text":"constructor"},"predicate":{"verb":"write","complement":"letters"},"tense":"past","aspect":"perfect"}});
        let cause = json!({"clause":{"subject":{"text":"Maya"},"predicate":{"verb":"arrive"},"tense":"past"}});
        let entity = json!({"id":"maya","label":"Maya","pronouns":{"subject":"she","object":"her","possessive_determiner":"her","possessive":"hers"}});
        let context = json!({"entities":[entity.clone()],"mentioned":["maya"],"facts":["arrival"]});
        let competing = json!({"entities":[entity.clone(),{"id":"other","label":"Chris"}],"mentioned":["maya","other"]});
        let mut cases = vec![
            ("realize", vec![sentence.clone()]),
            ("combine", vec![json!([sentence.clone(), cause.clone()]), json!("because")]),
            ("refer", vec![entity.clone(), context.clone()]),
            ("refer", vec![entity.clone(), competing.clone()]),
            ("refer", vec![entity.clone(), competing, json!({"style":"pronoun"})]),
            ("remember", vec![json!({}), json!([entity.clone()]), json!(["arrival"])]),
            (
                "unmentioned",
                vec![
                    json!([{"id":"arrival","text":"late"},{"id":"booking","text":"ZX-42"}]),
                    context,
                ],
            ),
            (
                "realize",
                vec![json!({"clause":{"subject":{"text":"toString"},"predicate":{"verb":"constructor"}}})],
            ),
            ("realize", vec![sentence.clone(), json!({"negation":"informal"})]),
            (
                "vary",
                vec![
                    json!("Constructor is not ready."),
                    json!({"negation":false,"condition_order":false}),
                ],
            ),
            ("vary", vec![json!("The guest cannot leave."), json!({"negation":null})]),
        ];
        for value in ["2024-02-29", "1900-02-29", "0000-01-01", "2026-11-10\n"] {
            for locale in ["en-US", "en-GB", "en-CA", "en-AU", "en-IN", "fr-FR"] {
                cases.push(("format_date", vec![json!(value), json!({"locale":locale})]));
            }
        }
        for value in ["00:05:09", "23:59", "24:00", "12:00\n"] {
            cases.push(("format_time", vec![json!(value), json!({"locale":"en-AU","seconds":true})]));
        }
        for value in [
            "1234567.895",
            "-0.004",
            "9.995",
            "1e-8",
            "0e4096",
            "1e-4096",
            "1\n",
            ".5",
            "5.",
        ] {
            cases.push((
                "format_number",
                vec![json!(value), json!({"locale":"en-IN","fraction_digits":2})],
            ));
        }
        for precision in [json!(true), json!(1.0), json!(1.5), Value::Null, json!(10)] {
            cases.push(("format_number", vec![json!(2.675), json!({"fraction_digits":precision})]));
        }
        for tense in ["present", "past", "future"] {
            for aspect in ["simple", "progressive", "perfect", "perfect_progressive"] {
                for form in ["statement", "question"] {
                    let mut value = sentence.clone();
                    value["clause"]["tense"] = json!(tense);
                    value["clause"]["aspect"] = json!(aspect);
                    value["clause"]["form"] = json!(form);
                    value["clause"]["polarity"] = json!("negative");
                    cases.push(("realize", vec![value, json!({"negation":"contracted"})]));
                }
            }
        }
        let texts = [
            "I won't leave. They cannot stay.",
            "I cannot leave.They cannot stay.",
            "I cannot leave. they cannot stay.",
            "I cannot leave. api_v2 cannot stay.",
            "I cannot leave? They cannot stay.",
            "Can they leave?They cannot stay.",
            "Can they leave? they cannot stay.",
            "I cannot leave.\nthey cannot stay.",
            "I cannot leave.They cannot stay.",
            "I cannot leave. Καλλιόπη cannot stay.",
            "If the guest arrives late, can they check in?",
            "Can they check in if the guest arrives late?",
            "  If Maya has finished, api_v2 will update résumé_ID-42.\n",
            "Constructor is not ready.",
            "They cannot\u{001c}leave.",
            "\u{feff}They cannot leave.",
            "\u{0085}They cannot leave.\u{00a0}",
            "I can't leave. She's gone.",
        ];
        let expected = json!({"calls":cases.iter().map(|(op,args)|native(op,args)).collect::<Vec<_>>(),"texts":texts.iter().map(|text|crate::english::text_alternatives(text,&crate::english::VariationOptions{negation:true,condition_order:true})).collect::<Vec<_>>()});
        let input = json!({"calls":cases,"texts":texts}).to_string();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let trailer = if lang == ScorerLang::Python {
                r#"
data = _english_json.load(__import__('sys').stdin)
results = []
for operation, args in data['calls']:
    try: results.append({'value':_language_english(operation,args,lambda:0.0)})
    except Exception: results.append({'error':True})
print(_english_json.dumps({'calls':results,'texts':[_english_text_alternatives(text,{'negation':True,'condition_order':True}) for text in data['texts']]}))
"#
            } else {
                r#"
import {readFileSync} from 'node:fs';
const data = JSON.parse(readFileSync(0,'utf8'));
const calls = data.calls.map(([operation,args]: any) => {try {return {value:_language_english(operation,args,()=>0)};} catch {return {error:true};}});
console.log(JSON.stringify({calls,texts:data.texts.map((text:string)=>_englishTextAlternatives(text,{negation:true,condition_order:true}))}));
"#
            };
            let source = format!("{}\n{trailer}", english_source(lang, false));
            let path = std::env::temp_dir().join(format!(
                "bts-english-runtime-{}.{}",
                uuid::Uuid::new_v4(),
                if lang == ScorerLang::Python { "py" } else { "mts" }
            ));
            std::fs::write(&path, source).unwrap();
            let mut child = Command::new(if lang == ScorerLang::Python { "python3" } else { "node" })
                .arg(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let output = child.wait_with_output().unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(
                output.status.success(),
                "{lang:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
            for (index, case) in cases.iter().enumerate() {
                assert_eq!(actual["calls"][index], expected["calls"][index], "{lang:?}: {case:?}");
            }
            assert_eq!(actual["texts"], expected["texts"], "{lang:?} text alternatives");
        }
    }
}
