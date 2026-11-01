use bts_lang::syntax::{BinOp, Expr, ExprKind, UnaryOp, parse_expression};
use bts_lang::{Diag, DiagPhase, SrcRange};
use std::str::FromStr;

// scorer definitions are not filtered because they do not generate spans
#[derive(Debug, Clone, Default)]
pub struct WriteFilter(Option<FilterExpr>);

#[derive(Debug, Clone)]
enum FilterExpr {
    Bool(bool),
    Not(Box<FilterExpr>),
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
    Eq(Operand, Operand),
    Ne(Operand, Operand),
}

#[derive(Debug, Clone)]
enum Operand {
    Kind,
    Name,
    String(String),
    Null,
}

/// A filter expression that could not be parsed or uses an unsupported value.
#[derive(Debug)]
pub enum ParseError {
    Syntax(bts_lang::Diags),
    Expression(Error),
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(diags) => f.write_str(&diags.iter().map(|d| d.what.as_str()).collect::<Vec<_>>().join("\n")),
            Self::Expression(source) => source.fmt(f),
        }
    }
}
impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Expression(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    pub range: SrcRange,
}
impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ErrorKind {
    ExpectedBooleanExpression,
    ExpectedOperand,
}
impl std::fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ExpectedBooleanExpression => {
                "expected a boolean expression using block.kind, block.name, ==, !=, &&, ||, or !"
            }
            Self::ExpectedOperand => "expected block.kind, block.name, a string, or null",
        })
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(formatter)
    }
}
impl std::error::Error for Error {}
impl From<Error> for Diag {
    fn from(error: Error) -> Self {
        Self {
            when: DiagPhase::Validation,
            what: error.kind.to_string(),
            r#where: error.range,
        }
    }
}

impl FromStr for WriteFilter {
    type Err = ParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let expr = parse_expression(source).map_err(ParseError::Syntax)?;
        compile(expr).map(|expr| Self(Some(expr))).map_err(ParseError::Expression)
    }
}

fn compile(expr: Expr) -> Result<FilterExpr, Error> {
    let range = expr.range;
    match expr.kind {
        ExprKind::Bool(value) => Ok(FilterExpr::Bool(value)),
        ExprKind::Unary {
            op: UnaryOp::Not,
            operand,
        } => Ok(FilterExpr::Not(Box::new(compile(*operand)?))),
        ExprKind::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } => Ok(FilterExpr::And(Box::new(compile(*lhs)?), Box::new(compile(*rhs)?))),
        ExprKind::Binary { op: BinOp::Or, lhs, rhs } => Ok(FilterExpr::Or(Box::new(compile(*lhs)?), Box::new(compile(*rhs)?))),
        ExprKind::Binary { op: BinOp::Eq, lhs, rhs } => Ok(FilterExpr::Eq(operand(*lhs)?, operand(*rhs)?)),
        ExprKind::Binary { op: BinOp::Ne, lhs, rhs } => Ok(FilterExpr::Ne(operand(*lhs)?, operand(*rhs)?)),
        _ => Err(Error {
            kind: ErrorKind::ExpectedBooleanExpression,
            range,
        }),
    }
}

fn operand(expr: Expr) -> Result<Operand, Error> {
    let range = expr.range;
    match expr.kind {
        ExprKind::Ref { path } if path == ["block", "kind"] => Ok(Operand::Kind),
        ExprKind::Ref { path } if path == ["block", "name"] => Ok(Operand::Name),
        ExprKind::Str(value) => Ok(Operand::String(value)),
        ExprKind::Null => Ok(Operand::Null),
        _ => Err(Error {
            kind: ErrorKind::ExpectedOperand,
            range,
        }),
    }
}

impl WriteFilter {
    pub fn matches(&self, kind: &str, name: Option<&str>) -> bool {
        self.0.as_ref().is_none_or(|expr| expr.matches(kind, name))
    }
}

impl FilterExpr {
    fn matches(&self, kind: &str, name: Option<&str>) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Not(expr) => !expr.matches(kind, name),
            Self::And(lhs, rhs) => lhs.matches(kind, name) && rhs.matches(kind, name),
            Self::Or(lhs, rhs) => lhs.matches(kind, name) || rhs.matches(kind, name),
            Self::Eq(lhs, rhs) => lhs.value(kind, name) == rhs.value(kind, name),
            Self::Ne(lhs, rhs) => lhs.value(kind, name) != rhs.value(kind, name),
        }
    }
}

impl Operand {
    fn value<'a>(&'a self, kind: &'a str, name: Option<&'a str>) -> Option<&'a str> {
        match self {
            Self::Kind => Some(kind),
            Self::Name => name,
            Self::String(value) => Some(value),
            Self::Null => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WriteFilter;

    #[test]
    fn filters_generated_block_identity() {
        let filter: WriteFilter = r#"block.kind != "scorer" || block.name == "keep""#.parse().unwrap();
        assert!(filter.matches("trace", Some("root")));
        assert!(!filter.matches("scorer", Some("drop")));
        assert!(filter.matches("scorer", Some("keep")));
        assert!("block.name == null".parse::<WriteFilter>().unwrap().matches("maybe", None));
    }

    #[test]
    fn rejects_values_outside_the_filter_context() {
        assert!("trace.output == \"x\"".parse::<WriteFilter>().is_err());
        assert!("block.kind ==".parse::<WriteFilter>().is_err());
    }
}
