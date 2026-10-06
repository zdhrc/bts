use crate::dsl::ast::{BinOp, Expr, ExprKind, UnaryOp};
use crate::dsl::{lexer::lex, parser::parse_expression};
use std::str::FromStr;

// scorer definitions are not filtered because they do not generate spans
#[derive(Debug, Clone, Default)]
pub(crate) struct WriteFilter(Option<FilterExpr>);

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

impl FromStr for WriteFilter {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let tokens = lex(source).map_err(|diags| render(diags, source))?;
        let expr = parse_expression(tokens, source).map_err(|diags| render(diags, source))?;
        compile(expr).map(|expr| Self(Some(expr))).map_err(|(at, message)| {
            let before = &source[..at.min(source.len())];
            let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            format!("--filter:{line}:{column}: {message}")
        })
    }
}

fn render(diags: crate::dsl::Diags, source: &str) -> String {
    diags
        .into_iter()
        .map(|diag| diag.render("--filter", source))
        .collect::<Vec<_>>()
        .join("\n")
}

fn compile(expr: Expr) -> Result<FilterExpr, (usize, String)> {
    let at = expr.range.start;
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
        _ => Err((
            at,
            "expected a boolean expression using block.kind, block.name, ==, !=, &&, ||, or !".to_owned(),
        )),
    }
}

fn operand(expr: Expr) -> Result<Operand, (usize, String)> {
    let at = expr.range.start;
    match expr.kind {
        ExprKind::Ref { path } if path == ["block", "kind"] => Ok(Operand::Kind),
        ExprKind::Ref { path } if path == ["block", "name"] => Ok(Operand::Name),
        ExprKind::Str(value) => Ok(Operand::String(value)),
        ExprKind::Null => Ok(Operand::Null),
        _ => Err((at, "expected block.kind, block.name, a string, or null".to_owned())),
    }
}

impl WriteFilter {
    pub(crate) fn matches(&self, kind: &str, name: Option<&str>) -> bool {
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
