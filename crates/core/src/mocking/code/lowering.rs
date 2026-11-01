//! Lower domain expressions and execution scopes into generated functions.

use crate::dsl::{
    Accessor, ArrayElem, BinOp, Child, CtxRef, Field, Func, Model, NodeId, Number, Part, ScorerLang, Selection, SpanFields,
    Step, Trace, UnaryOp, Value,
};
use code::{Dependency, Expr, Statement, Variable};
use serde_json::{Value as Json, json};
use std::collections::HashSet;

pub(crate) struct Agent {
    pub functions: Vec<code::Function>,
    pub tokens: bool,
    pub contracts: Vec<(String, Json)>,
}

/// Code structure shared by the target emitters.
pub(crate) mod code {
    use crate::dsl::{BinOp, UnaryOp};
    use std::collections::HashSet;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub enum Dependency {
        Variable(String),
        Field(String, String),
        Child(String, String),
    }

    #[derive(Clone, Debug)]
    pub enum Expr {
        Name(String),
        String(String),
        Number(String),
        Bool(bool),
        Null,
        Call {
            function: Box<Expr>,
            args: Vec<Expr>,
            keywords: Vec<(String, Expr)>,
        },
        New {
            class: String,
            args: Vec<Expr>,
        },
        Binary {
            left: Box<Expr>,
            op: BinOp,
            right: Box<Expr>,
        },
        Unary {
            op: UnaryOp,
            value: Box<Expr>,
        },
        Conditional {
            condition: Box<Expr>,
            yes: Box<Expr>,
            no: Box<Expr>,
        },
        Required(Box<Expr>),
        Member(Box<Expr>, String),
        Index(Box<Expr>, Box<Expr>),
        Slice {
            value: Box<Expr>,
            start: Option<Box<Expr>>,
            end: Option<Box<Expr>>,
        },
        Array(Vec<Expr>),
        Object(Vec<(String, Expr)>),
        Spread(Box<Expr>),
        Lambda {
            parameters: Vec<Parameter>,
            value: Box<Expr>,
        },
        Map {
            item: String,
            iterable: Box<Expr>,
            value: Box<Expr>,
        },
        Template(Vec<Expr>),
        Identity(Box<Expr>, Box<Expr>),
        Contains(Box<Expr>, Box<Expr>),
        /// Runtime reads retain their semantic dependency independently of syntax.
        Read {
            dependency: Dependency,
            value: Box<Expr>,
        },
    }
    impl From<&str> for Expr {
        fn from(value: &str) -> Self {
            Self::Name(value.into())
        }
    }
    impl From<String> for Expr {
        fn from(value: String) -> Self {
            Self::Name(value)
        }
    }
    impl Expr {
        pub fn string(value: impl Into<String>) -> Self {
            Self::String(value.into())
        }
        pub fn number(value: impl ToString) -> Self {
            Self::Number(value.to_string())
        }
        pub fn call(name: impl Into<Expr>, args: Vec<Expr>) -> Self {
            Self::Call {
                function: Box::new(name.into()),
                args,
                keywords: vec![],
            }
        }
        pub fn member(self, name: impl Into<String>) -> Self {
            Self::Member(Box::new(self), name.into())
        }
        pub fn index(self, index: Expr) -> Self {
            Self::Index(Box::new(self), Box::new(index))
        }
        pub fn binary(self, op: BinOp, right: Expr) -> Self {
            Self::Binary {
                left: Box::new(self),
                op,
                right: Box::new(right),
            }
        }
        pub fn not(self) -> Self {
            Self::Unary {
                op: UnaryOp::Not,
                value: Box::new(self),
            }
        }
        pub fn lambda(self, parameters: Vec<Parameter>) -> Self {
            Self::Lambda {
                parameters,
                value: Box::new(self),
            }
        }
        pub fn lazy(self) -> Self {
            Self::call("_lazy", vec![self.lambda(vec![])])
        }
        pub fn read(self, dependency: Dependency) -> Self {
            Self::Read {
                dependency,
                value: Box::new(self),
            }
        }
        pub fn references(&self) -> HashSet<Dependency> {
            let mut references = HashSet::new();
            self.visit(&mut |node| match node {
                Self::Name(name) => {
                    references.insert(Dependency::Variable(name.clone()));
                }
                Self::Read { dependency, .. } => {
                    references.insert(dependency.clone());
                }
                _ => {}
            });
            // Lambda and map locals are excluded by the traversal.
            references
        }
        fn visit(&self, visitor: &mut dyn FnMut(&Expr)) {
            visitor(self);
            match self {
                Self::Call {
                    function,
                    args,
                    keywords,
                } => {
                    function.visit(visitor);
                    for arg in args {
                        arg.visit(visitor);
                    }
                    for (_, arg) in keywords {
                        arg.visit(visitor);
                    }
                }
                Self::New { args, .. } | Self::Array(args) | Self::Template(args) => {
                    for arg in args {
                        arg.visit(visitor);
                    }
                }
                Self::Binary { left, right, .. }
                | Self::Index(left, right)
                | Self::Identity(left, right)
                | Self::Contains(left, right) => {
                    left.visit(visitor);
                    right.visit(visitor);
                }
                Self::Unary { value, .. } | Self::Member(value, _) | Self::Spread(value) | Self::Read { value, .. } => {
                    value.visit(visitor)
                }
                Self::Conditional { condition, yes, no } => {
                    condition.visit(visitor);
                    yes.visit(visitor);
                    no.visit(visitor);
                }
                Self::Slice { value, start, end } => {
                    value.visit(visitor);
                    for bound in [start, end].into_iter().flatten() {
                        bound.visit(visitor);
                    }
                }
                Self::Object(fields) => {
                    for (_, value) in fields {
                        value.visit(visitor);
                    }
                }
                Self::Lambda { parameters, value } => {
                    for parameter in parameters {
                        if let Some(default) = &parameter.default {
                            default.visit(visitor);
                        }
                    }
                    let locals: Vec<_> = parameters
                        .iter()
                        .flat_map(|p| {
                            if p.members.is_empty() {
                                vec![p.name.as_str()]
                            } else {
                                p.members.iter().map(String::as_str).collect()
                            }
                        })
                        .collect();
                    value.visit(&mut |node| {
                        if !matches!(node, Self::Name(name) if locals.contains(&name.as_str()))
                            && !matches!(node, Self::Read{dependency:Dependency::Field(name,_)|Dependency::Child(name,_)|Dependency::Variable(name),..} if locals.contains(&name.as_str())) {
                            visitor(node);
                        }
                    });
                }
                Self::Map { item, iterable, value } => {
                    iterable.visit(visitor);
                    value.visit(&mut |node| {
                        if !matches!(node, Self::Name(name) if name == item)
                            && !matches!(node, Self::Read{dependency:Dependency::Field(name,_)|Dependency::Child(name,_)|Dependency::Variable(name),..} if name == item) {
                            visitor(node);
                        }
                    });
                }
                _ => {}
            }
        }
    }

    /// Rewrite expressions bottom-up without rendering or parsing target code.
    pub fn rewrite_expr(value: &mut Expr, rewrite: &mut impl FnMut(&mut Expr)) {
        match value {
            Expr::Call {
                function,
                args,
                keywords,
            } => {
                rewrite_expr(function, rewrite);
                for arg in args {
                    rewrite_expr(arg, rewrite);
                }
                for (_, arg) in keywords {
                    rewrite_expr(arg, rewrite);
                }
            }
            Expr::New { args, .. } | Expr::Array(args) | Expr::Template(args) => {
                for arg in args {
                    rewrite_expr(arg, rewrite);
                }
            }
            Expr::Binary { left, right, .. }
            | Expr::Index(left, right)
            | Expr::Identity(left, right)
            | Expr::Contains(left, right) => {
                rewrite_expr(left, rewrite);
                rewrite_expr(right, rewrite);
            }
            Expr::Required(value)
            | Expr::Unary { value, .. }
            | Expr::Member(value, _)
            | Expr::Spread(value)
            | Expr::Read { value, .. } => rewrite_expr(value, rewrite),
            Expr::Conditional { condition, yes, no } => {
                rewrite_expr(condition, rewrite);
                rewrite_expr(yes, rewrite);
                rewrite_expr(no, rewrite);
            }
            Expr::Slice { value, start, end } => {
                rewrite_expr(value, rewrite);
                for bound in [start, end].into_iter().flatten() {
                    rewrite_expr(bound, rewrite);
                }
            }
            Expr::Object(fields) => {
                for (_, value) in fields {
                    rewrite_expr(value, rewrite);
                }
            }
            Expr::Lambda { parameters, value } => {
                for p in parameters {
                    if let Some(default) = &mut p.default {
                        rewrite_expr(default, rewrite);
                    }
                }
                rewrite_expr(value, rewrite);
            }
            Expr::Map { iterable, value, .. } => {
                rewrite_expr(iterable, rewrite);
                rewrite_expr(value, rewrite);
            }
            _ => {}
        }
        rewrite(value);
    }

    pub fn rewrite(body: &mut [Statement], rewrite: &mut impl FnMut(&mut Expr)) {
        for statement in body {
            match statement {
                Statement::Variable(Variable::Declare { value, .. })
                | Statement::Return(value)
                | Statement::Evaluate(value)
                | Statement::Raise(value) => rewrite_expr(value, rewrite),
                Statement::Variable(Variable::Assign { name, value }) => {
                    rewrite_expr(name, rewrite);
                    rewrite_expr(value, rewrite);
                }
                Statement::Function(function) => self::rewrite(&mut function.body, rewrite),
                Statement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    rewrite_expr(condition, rewrite);
                    self::rewrite(then_body, rewrite);
                    self::rewrite(else_body, rewrite);
                }
                Statement::ForRange { count, body, .. } => {
                    rewrite_expr(count, rewrite);
                    self::rewrite(body, rewrite);
                }
                Statement::TraceScope { scope, body } => {
                    rewrite_expr(scope, rewrite);
                    self::rewrite(body, rewrite);
                }
            }
        }
    }

    pub fn references(statements: &[Statement]) -> HashSet<Dependency> {
        let mut refs = HashSet::new();
        let mut locals = HashSet::new();
        for statement in statements {
            match statement {
                Statement::Variable(Variable::Declare { name, value, .. }) => {
                    locals.insert(name.clone());
                    refs.extend(value.references());
                }
                Statement::Variable(Variable::Assign { name, value }) => {
                    refs.extend(name.references());
                    refs.extend(value.references());
                }
                Statement::Return(value) | Statement::Evaluate(value) | Statement::Raise(value) => {
                    refs.extend(value.references())
                }
                Statement::Function(function) => {
                    locals.insert(function.name.clone());
                    let mut captures = references(&function.body);
                    let params: HashSet<_> = function
                        .parameters
                        .iter()
                        .flat_map(|p| {
                            if p.members.is_empty() {
                                vec![p.name.clone()]
                            } else {
                                p.members.clone()
                            }
                        })
                        .collect();
                    captures.retain(|d| !bound(d, &params));
                    refs.extend(captures);
                    for p in &function.parameters {
                        if let Some(value) = &p.default {
                            refs.extend(value.references());
                        }
                    }
                }
                Statement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    refs.extend(condition.references());
                    refs.extend(references(then_body));
                    refs.extend(references(else_body));
                }
                Statement::ForRange { index, count, body } => {
                    refs.extend(count.references());
                    let mut captures = references(body);
                    captures.retain(|d| !bound(d, &HashSet::from([index.clone()])));
                    refs.extend(captures);
                }
                Statement::TraceScope { scope, body } => {
                    refs.extend(scope.references());
                    refs.extend(references(body));
                }
            }
        }
        refs.retain(|d| !bound(d, &locals));
        refs
    }
    fn bound(dependency: &Dependency, locals: &HashSet<String>) -> bool {
        match dependency {
            Dependency::Variable(name) | Dependency::Field(name, _) | Dependency::Child(name, _) => locals.contains(name),
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct Parameter {
        pub name: String,
        pub members: Vec<String>,
        pub annotation: Option<String>,
        pub default: Option<Expr>,
        pub keyword_only: bool,
    }

    impl Parameter {
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                ..Self::default()
            }
        }
        pub fn object(names: &[&str]) -> Self {
            Self {
                members: names.iter().map(|name| (*name).into()).collect(),
                ..Self::default()
            }
        }
        pub fn typed(mut self, annotation: impl Into<String>) -> Self {
            self.annotation = Some(annotation.into());
            self
        }
        pub fn with_default(mut self, value: impl Into<Expr>) -> Self {
            self.default = Some(value.into());
            self
        }
        pub fn keyword(mut self) -> Self {
            self.keyword_only = true;
            self
        }
    }

    #[derive(Clone, Debug)]
    pub enum Variable {
        Declare {
            name: String,
            value: Expr,
            mutable: bool,
            annotation: Option<String>,
        },
        Assign {
            name: Expr,
            value: Expr,
        },
    }

    impl Variable {
        pub fn constant(name: impl Into<String>, value: impl Into<Expr>) -> Self {
            Self::Declare {
                name: name.into(),
                value: value.into(),
                mutable: false,
                annotation: None,
            }
        }
    }

    #[derive(Clone, Debug)]
    pub enum Statement {
        Variable(Variable),
        Return(Expr),
        Evaluate(Expr),
        If {
            condition: Expr,
            then_body: Vec<Statement>,
            else_body: Vec<Statement>,
        },
        ForRange {
            index: String,
            count: Expr,
            body: Vec<Statement>,
        },
        TraceScope {
            scope: Expr,
            body: Vec<Statement>,
        },
        Raise(Expr),
        Function(Function),
    }

    #[derive(Clone, Debug, Default)]
    pub struct Function {
        pub name: String,
        pub parameters: Vec<Parameter>,
        pub returns: Option<String>,
        pub asynchronous: bool,
        pub exported: bool,
        pub body: Vec<Statement>,
    }

    impl Function {
        pub fn new(name: impl Into<String>, parameters: Vec<Parameter>) -> Self {
            Self {
                name: name.into(),
                parameters,
                ..Self::default()
            }
        }
        /// Names explicitly owned by this function; nested function locals belong
        /// to their own scope.
        pub fn local_names(&self) -> Vec<&str> {
            let mut names = Vec::new();
            for parameter in &self.parameters {
                if parameter.members.is_empty() {
                    names.push(parameter.name.as_str());
                } else {
                    names.extend(parameter.members.iter().map(String::as_str));
                }
            }
            for statement in &self.body {
                match statement {
                    Statement::Variable(Variable::Declare { name, .. }) => names.push(name.as_str()),
                    Statement::Function(function) => names.push(function.name.as_str()),
                    _ => {}
                }
            }
            names
        }
        pub fn declare(&mut self, name: &str, value: impl Into<Expr>) -> Result<(), String> {
            if self.local_names().contains(&name) {
                return Err(format!("duplicate binding {name:?} in function {:?}", self.name));
            }
            self.body.push(Statement::Variable(Variable::constant(name, value)));
            Ok(())
        }
    }
}

#[derive(Clone)]
struct Context<'a> {
    model: &'a Model,
    parents: Vec<(NodeId, NodeId)>,
    site: NodeId,
    iterations: Vec<(NodeId, String, String)>,
    names: Vec<(NodeId, Names)>,
    local_names: Vec<(String, String)>,
    frames: Vec<(NodeId, String)>,
    direct: bool,
}

#[derive(Clone, Default)]
struct Names {
    index: String,
    count: String,
    item: String,
    function: String,
    state: String,
}

impl Context<'_> {
    fn names(&self, node: NodeId) -> &Names {
        &self.names.iter().find(|(id, _)| *id == node).expect("named child").1
    }
}

fn allocate(base: &str, role: &str, lang: ScorerLang, used: &mut HashSet<String>) -> String {
    let mut stem = crate::mocking::code::planning::identifier_stem(base).unwrap_or_else(|| "activity".into());
    if stem.starts_with(|c: char| c.is_ascii_digit()) {
        stem = format!("activity_{stem}");
    }
    let words = if role.is_empty() { stem } else { format!("{stem}_{role}") };
    let name = match lang {
        ScorerLang::Python => words,
        ScorerLang::Typescript => {
            let mut words = words.split('_');
            let mut name = words.next().unwrap().to_owned();
            for word in words {
                let mut letters = word.chars();
                if let Some(first) = letters.next() {
                    name.push(first.to_ascii_uppercase());
                    name.extend(letters);
                }
            }
            name
        }
    };
    let mut candidate = name.clone();
    let mut suffix = 2;
    while !used.insert(candidate.clone()) {
        candidate = match lang {
            ScorerLang::Python => format!("{name}_{suffix}"),
            ScorerLang::Typescript => format!("{name}{suffix}"),
        };
        suffix += 1;
    }
    candidate
}

fn child_node(child: &Child) -> NodeId {
    match child {
        Child::Span(child) => child.node,
        Child::Repeat(child) => child.node,
        Child::Choice(child) => child.node,
        Child::Maybe(child) => child.node,
        Child::Scorer(child) => child.node,
    }
}

fn random_name(lang: ScorerLang) -> &'static str {
    match lang {
        ScorerLang::Python => "random_source",
        ScorerLang::Typescript => "randomSource",
    }
}

fn collect_names(children: &[Child], lang: ScorerLang, used: &mut HashSet<String>) -> Vec<(NodeId, Names)> {
    fn visit(children: &[Child], lang: ScorerLang, used: &mut HashSet<String>, names: &mut Vec<(NodeId, Names)>) {
        for child in children {
            match child {
                Child::Span(span) => {
                    names.push((
                        span.node,
                        Names {
                            function: allocate(&span.name, "", lang, used),
                            state: allocate(&span.name, "span", lang, used),

                            ..Names::default()
                        },
                    ));
                    visit(&span.children, lang, used, names);
                }
                Child::Repeat(repeat) => {
                    let name = repeat.name.as_deref().unwrap_or("batch");
                    names.push((
                        repeat.node,
                        Names {
                            function: allocate(&format!("process_{name}"), "", lang, used),
                            state: allocate(name, "span", lang, used),

                            index: allocate(name, "index", lang, used),
                            count: allocate(name, "count", lang, used),
                            item: allocate(name, "result", lang, used),
                        },
                    ));
                    visit(&repeat.children, lang, used, names);
                }
                Child::Choice(choice) => {
                    branch_names(
                        choice.node,
                        choice.name.as_deref().unwrap_or("choice"),
                        &choice.children,
                        true,
                        lang,
                        used,
                        names,
                    );
                    visit(&choice.children, lang, used, names);
                }
                Child::Maybe(maybe) => {
                    branch_names(
                        maybe.node,
                        maybe.name.as_deref().unwrap_or("optional"),
                        &maybe.children,
                        false,
                        lang,
                        used,
                        names,
                    );
                    visit(&maybe.children, lang, used, names);
                }
                _ => {}
            }
        }
    }
    fn branch_names(
        node: NodeId,
        name: &str,
        _children: &[Child],
        _choice: bool,
        lang: ScorerLang,
        used: &mut HashSet<String>,
        names: &mut Vec<(NodeId, Names)>,
    ) {
        names.push((
            node,
            Names {
                function: allocate(name, "", lang, used),
                state: allocate(name, "span", lang, used),

                ..Names::default()
            },
        ));
    }
    let mut names = Vec::new();
    visit(children, lang, used, &mut names);
    names
}

pub(crate) fn agent(model: &Model, trace: &Trace, lang: ScorerLang) -> Result<Agent, String> {
    execution::emit(model, trace, lang)
}

fn is_literal(value: &Value) -> bool {
    match value {
        Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null => true,
        Value::Array(array) => array.elem.iter().all(|item| match item {
            ArrayElem::Item(v) | ArrayElem::Spread(v) => is_literal(v),
        }),
        Value::Object(object) => object.elem.iter().all(|field| is_literal(&field.value)),
        _ => false,
    }
}

fn expression(value: &Value, ctx: &Context<'_>, lang: ScorerLang) -> Result<Expr, String> {
    let expr = |v: &Value| expression(v, ctx, lang);
    Ok(match value {
        Value::CtxRef(reference) => context_ref(*reference, ctx)?,
        Value::Func { func, .. } => expressions::emit(func, ctx, lang)?,
        Value::Index { target, index, .. } => Expr::call("_index", vec![expr(target)?, expr(index)?]),
        Value::Slice { target, start, end, .. } => {
            slice_expression(expr(target)?, start.as_deref(), end.as_deref(), ctx, lang)?
        }
        Value::VarRef(name) => Expr::call(
            ctx.local_names
                .iter()
                .find(|(n, _)| n == name)
                .ok_or_else(|| format!("unknown agent variable {name:?}"))?
                .1
                .clone(),
            vec![],
        ),
        Value::Template(template) => {
            let parts = template
                .parts
                .iter()
                .map(|part| {
                    Ok(match part {
                        Part::Lit(text) => Expr::string(text),
                        Part::Dynamic(value) => Expr::call("_text", vec![expr(value)?]),
                        Part::VarRef(name) => Expr::call("_text", vec![expr(&Value::VarRef(name.clone()))?]),
                        Part::Ref(reference) => Expr::call("_text", vec![context_ref(*reference, ctx)?]),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Expr::Template(parts)
        }
        Value::Unary { op, operand, .. } => match op {
            UnaryOp::Not => Expr::call("_boolean", vec![expr(operand)?]).not(),
            UnaryOp::Neg => Expr::call("_arithmetic", vec![Expr::string("-"), Expr::number(0), expr(operand)?]),
        },
        Value::Str(value) => Expr::string(value),
        Value::Num(Number::Int(value)) => {
            if lang == ScorerLang::Typescript && !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(value) {
                return Err("integer is outside TypeScript's safe integer range".into());
            }
            Expr::number(value)
        }
        Value::Num(Number::Float(value)) if value.is_finite() => {
            let value = Expr::Number(format!("{value:?}"));
            if lang == ScorerLang::Typescript {
                Expr::call("_float", vec![value])
            } else {
                value
            }
        }
        Value::Bool(value) => Expr::Bool(*value),
        Value::Null => Expr::Null,
        Value::Array(array) => Expr::Array(
            array
                .elem
                .iter()
                .map(|item| {
                    Ok(match item {
                        ArrayElem::Item(value) => {
                            let e = expr(value)?;
                            if ctx.direct && is_literal(value) { e } else { e.lazy() }
                        }
                        ArrayElem::Spread(value) => Expr::Spread(Box::new(Expr::call("_array", vec![expr(value)?]))),
                    })
                })
                .collect::<Result<_, String>>()?,
        ),
        Value::Object(object) => Expr::Object(
            object
                .elem
                .iter()
                .map(|item| {
                    let value = expr(&item.value)?;
                    Ok((item.key.clone(), if ctx.direct { value } else { value.lazy() }))
                })
                .collect::<Result<_, String>>()?,
        ),
        Value::Cond { cond, then, otherwise } => Expr::Conditional {
            condition: Box::new(Expr::call("_boolean", vec![expr(cond)?])),
            yes: Box::new(expr(then)?),
            no: Box::new(expr(otherwise)?),
        },
        Value::Binary { op, lhs, rhs, .. } => {
            let left = expr(lhs)?;
            let right = expr(rhs)?;
            match op {
                BinOp::Eq | BinOp::Ne => {
                    let equal = Expr::call("_equal", vec![left, right]);
                    if *op == BinOp::Ne { equal.not() } else { equal }
                }
                BinOp::And | BinOp::Or => Expr::call("_boolean", vec![left]).binary(*op, Expr::call("_boolean", vec![right])),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    Expr::call("_num", vec![left]).binary(*op, Expr::call("_num", vec![right]))
                }
                _ => Expr::call(
                    "_arithmetic",
                    vec![
                        Expr::string(match op {
                            BinOp::Add => "+",
                            BinOp::Sub => "-",
                            BinOp::Mul => "*",
                            BinOp::Div => "/",
                            _ => "%",
                        }),
                        left,
                        right,
                    ],
                ),
            }
        }
        Value::BlockRef { ref_id, .. } => {
            let reference = &ctx.model.refs[ref_id.0 as usize];
            let mut node = reference.module.unwrap_or(ctx.site);
            for _ in 0..reference.up {
                node = ctx
                    .parents
                    .iter()
                    .find(|(child, _)| *child == node)
                    .map(|(_, parent)| *parent)
                    .ok_or("reference leaves the selected agent trace")?;
            }
            reference_expression(node, &reference.steps, &reference.accessor, &reference.path, None, ctx, lang)?
        }
        _ => return Err("this runtime value is not supported by agent generation".into()),
    })
}
fn context_ref(reference: CtxRef, ctx: &Context<'_>) -> Result<Expr, String> {
    if reference == CtxRef::TraceIndex {
        return Ok(Expr::number(0));
    }
    let mut node = ctx.site;
    loop {
        if let Some((_, index, count)) = ctx.iterations.iter().find(|(id, _, _)| *id == node) {
            return Ok(Expr::Name(
                if reference == CtxRef::RepeatIndex { index } else { count }.clone(),
            ));
        }
        node = ctx
            .parents
            .iter()
            .find(|(child, _)| *child == node)
            .map(|(_, parent)| *parent)
            .ok_or("repeat context is unavailable")?;
    }
}
fn slice_expression(
    target: Expr,
    start: Option<&Value>,
    end: Option<&Value>,
    ctx: &Context<'_>,
    lang: ScorerLang,
) -> Result<Expr, String> {
    let bound = |value: Option<&Value>| {
        value
            .map(|v| expression(v, ctx, lang))
            .transpose()
            .map(|v| v.unwrap_or(Expr::Null))
    };
    Ok(Expr::call("_slice", vec![target, bound(start)?, bound(end)?]))
}

fn field_key(field: &Field) -> &'static str {
    match field {
        Field::Input => "input",
        Field::Output => "output",
        Field::Expected => "expected",
        Field::Error => "error",
        Field::Metadata => "metadata",
        Field::Metrics => "metrics",
        Field::Tags => "tags",
    }
}

fn read_field(frame: Expr, key: &str, direct: bool) -> Expr {
    let value = if direct {
        frame.clone().member(key)
    } else {
        Expr::call("_read", vec![frame.clone(), Expr::string(key)])
    };
    if let Expr::Name(name) = frame {
        value.read(Dependency::Field(name, key.into()))
    } else {
        value
    }
}
fn reference_expression(
    node: NodeId,
    steps: &[Step],
    accessor: &Accessor,
    path: &[Selection],
    record: Option<Expr>,
    ctx: &Context<'_>,
    lang: ScorerLang,
) -> Result<Expr, String> {
    if ctx.direct
        && record.is_none()
        && let Some((
            Step::Child {
                candidates,
                position: None,
            },
            rest,
        )) = steps.split_first()
        && candidates.len() == 1
        && let Some((_, frame)) = ctx.frames.iter().rev().find(|(id, _)| *id == candidates[0])
    {
        return reference_expression(candidates[0], rest, accessor, path, Some(frame.clone().into()), ctx, lang);
    }
    let frame = record
        .or_else(|| {
            ctx.frames
                .iter()
                .rev()
                .find(|(id, _)| *id == node)
                .map(|(_, frame)| Expr::Name(frame.clone()))
        })
        .ok_or("reference leaves the selected agent trace")?;
    if let Some((step, rest)) = steps.split_first() {
        return match step {
            Step::Child { candidates, position } => {
                let results = candidates
                    .iter()
                    .map(|child| {
                        let key = &ctx.names(*child).state;
                        let mut selected = Expr::call("_child", vec![frame.clone(), Expr::string(key)]);
                        if let Expr::Name(name) = &frame {
                            selected = selected.read(Dependency::Child(name.clone(), key.clone()));
                        }
                        let item = format!("{key}_selected");
                        let result = reference_expression(*child, rest, accessor, path, Some(item.clone().into()), ctx, lang)?;
                        let parameter = code::Parameter::new(item);
                        Ok(Expr::call(
                            "_at",
                            vec![
                                selected,
                                result.lambda(vec![if lang == ScorerLang::Typescript {
                                    parameter.typed("Span")
                                } else {
                                    parameter
                                }]),
                            ],
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                if let Some(position) = position {
                    Ok(Expr::call(
                        "_select",
                        vec![
                            expression(position, ctx, lang)?,
                            Expr::Array(results.into_iter().map(|r| r.lambda(vec![])).collect()),
                        ],
                    ))
                } else {
                    results.into_iter().next().ok_or("empty child selection".into())
                }
            }
            Step::Iteration(index) => reference_expression(
                node,
                rest,
                accessor,
                path,
                Some(Expr::call("_iteration", vec![frame, expression(index, ctx, lang)?])),
                ctx,
                lang,
            ),
            Step::Iterations { start, end } => {
                let selected = slice_expression(frame.member("iterations"), start.as_ref(), end.as_ref(), ctx, lang)?;
                let item = ctx.names(node).item.clone();
                let value = reference_expression(node, rest, accessor, path, Some(item.clone().into()), ctx, lang)?;
                Ok(Expr::Map {
                    item,
                    iterable: Box::new(selected),
                    value: Box::new(value),
                })
            }
        };
    }
    let mut result = match accessor {
        Accessor::Field(field) => read_field(frame, field_key(field), ctx.direct),
        Accessor::Chosen => read_field(frame, "chosen", ctx.direct),
        Accessor::Included => read_field(frame, "included", ctx.direct),
        Accessor::Index | Accessor::Count => {
            let (_, index, count) = ctx
                .iterations
                .iter()
                .rev()
                .find(|(id, _, _)| *id == node)
                .ok_or("repeat context is unavailable")?;
            Expr::Name(if matches!(accessor, Accessor::Index) { index } else { count }.clone())
        }
        _ => return Err("whole block values are not agent expressions".into()),
    };
    for selection in path {
        result = match selection {
            Selection::Index(index) => Expr::call("_index", vec![result, expression(index, ctx, lang)?]),
            Selection::Slice { start, end } => slice_expression(result, start.as_ref(), end.as_ref(), ctx, lang)?,
        };
    }
    Ok(result)
}

mod execution {
    use super::*;
    use crate::dsl::{Binding, SpanKind};
    use crate::mocking::code::planning::Operation;

    pub(crate) fn emit(model: &Model, trace: &Trace, lang: ScorerLang) -> Result<Agent, String> {
        let mut parents = Vec::new();
        let mut bindings = model.bindings.iter().chain(&trace.bindings).collect::<Vec<_>>();
        fn collect<'a>(
            children: &'a [Child],
            parent: NodeId,
            parents: &mut Vec<(NodeId, NodeId)>,
            bindings: &mut Vec<&'a Binding>,
        ) -> Result<(), String> {
            for child in children {
                let (node, locals, children) = match child {
                    Child::Span(v) => (v.node, &v.bindings, &v.children),
                    Child::Repeat(v) => (v.node, &v.bindings, &v.children),
                    Child::Choice(v) => (v.node, &v.bindings, &v.children),
                    Child::Maybe(v) => (v.node, &v.bindings, &v.children),
                    Child::Scorer(_) => return Err("scorer spans are not supported by agent generation yet".into()),
                };
                parents.push((node, parent));
                bindings.extend(locals);
                collect(children, node, parents, bindings)?;
            }
            Ok(())
        }
        collect(&trace.children, trace.node, &mut parents, &mut bindings)?;
        let mut used = "context ExecutionContext input run run_details runDetails run_traced runTraced logger random_source randomSource root scope range len str int bool float type isinstance abs object print None True False def class return for while if else elif in is not and or lambda yield import from as with try except finally pass break continue global nonlocal assert del raise await async match case function const let var new this null true false switch default export extends super typeof void delete throw catch do of enum implements interface package private protected public static arguments eval".split_whitespace().map(str::to_owned).collect();
        let names = collect_names(&trace.children, lang, &mut used);
        let local_names = bindings
            .iter()
            .map(|binding| (binding.name.clone(), allocate(&binding.name, "", lang, &mut used)))
            .collect();
        let ctx = Context {
            model,
            site: trace.node,
            parents,
            iterations: Vec::new(),
            names,
            local_names,
            frames: vec![(trace.node, "root".into())],
            direct: false,
        };
        let all_bindings = model.bindings.iter().chain(&trace.bindings).cloned().collect::<Vec<_>>();
        let mut direct_ctx = ctx.clone();
        direct_ctx.direct = true;
        let mut body = match scope(
            Some(&trace.fields),
            &all_bindings,
            &trace.children,
            "root",
            &direct_ctx,
            lang,
            true,
        ) {
            Ok(mut work) => {
                work.push(Statement::Return(Expr::call("_details", vec!["root".into()])));
                vec![Statement::TraceScope {
                    scope: "root".into(),
                    body: work,
                }]
            }
            Err(_) => {
                let mut work = scope(Some(&trace.fields), &all_bindings, &trace.children, "root", &ctx, lang, true)?;
                work.push(Statement::Return(Expr::call("_details", vec!["root".into()])));
                work
            }
        };
        let tokens = code::references(&body).contains(&Dependency::Variable("_tokens".into()));
        let parameters = match lang {
            ScorerLang::Python => vec![
                code::Parameter::new("input").with_default("_MISSING"),
                code::Parameter::new("random_source")
                    .with_default(Expr::from("_random").member("random"))
                    .keyword(),
                code::Parameter::new("logger").with_default(Expr::Null).keyword(),
            ],
            ScorerLang::Typescript => vec![
                code::Parameter::new("input").typed("unknown").with_default("_MISSING"),
                code::Parameter::new("randomSource")
                    .typed("() => number")
                    .with_default(Expr::from("Math").member("random")),
                code::Parameter::new("logger").typed("any").with_default(Expr::Null),
            ],
        };
        let mut function = code::Function::new("_execute", parameters);
        function.declare("root", scope_expr(&trace.name, "task", Expr::Null))?;
        function.body.append(&mut body);
        let parameters = function.parameters[..2].to_vec();
        let details_name = if lang == ScorerLang::Python {
            "run_details"
        } else {
            "runDetails"
        };
        let invoke = |name: &str, traced: bool| {
            let random = Expr::Name(random_name(lang).into());
            if lang == ScorerLang::Python {
                Expr::Call {
                    function: Box::new(name.into()),
                    args: vec!["input".into()],
                    keywords: if traced {
                        vec![("random_source".into(), random), ("logger".into(), "logger".into())]
                    } else {
                        vec![("random_source".into(), random)]
                    },
                }
            } else {
                Expr::call(
                    name,
                    if traced {
                        vec!["input".into(), random, "logger".into()]
                    } else {
                        vec!["input".into(), random]
                    },
                )
            }
        };
        let details_call = invoke("_execute", false);
        let run_call = invoke(details_name, false).index(Expr::string("output"));
        let traced_call = invoke("_execute", true).index(Expr::string("output"));
        let mut details = code::Function::new(details_name, parameters.clone());
        details.exported = true;
        details.body.push(code::Statement::Return(details_call));
        let mut run = code::Function::new("run", parameters.clone());
        run.exported = true;
        run.body.push(code::Statement::Return(run_call));
        let logger = if lang == ScorerLang::Python {
            code::Parameter::new("logger")
        } else {
            code::Parameter::new("logger").typed("any")
        };
        let mut traced = code::Function::new(
            if lang == ScorerLang::Python {
                "run_traced"
            } else {
                "runTraced"
            },
            std::iter::once(logger).chain(parameters).collect(),
        );
        traced.exported = true;
        traced.body.push(code::Statement::Return(traced_call));
        if lang == ScorerLang::Typescript {
            run.returns = Some("unknown".into());
            traced.returns = Some("unknown".into());
        }
        let mut functions = vec![function, details, run, traced];
        let contracts = if direct_ctx.direct && matches!(functions[0].body.get(1), Some(Statement::TraceScope { .. })) {
            native::functions(&mut functions, lang)
        } else {
            vec![]
        };
        Ok(Agent {
            tokens,
            functions,
            contracts,
        })
    }

    fn scope_expr(name: &str, kind: &str, parent: Expr) -> Expr {
        Expr::New {
            class: "Span".into(),
            args: vec![Expr::string(name), Expr::string(kind), parent, "logger".into()],
        }
    }
    fn fields(fields: &SpanFields, ctx: &Context<'_>, lang: ScorerLang, root: bool) -> Result<Vec<(String, Expr)>, String> {
        let mut values = vec![
            ("input", fields.input.clone()),
            ("output", fields.output.clone()),
            ("expected", fields.expected.clone()),
            ("error", fields.error.clone()),
            ("metadata", fields.metadata.clone().map(Value::Object)),
            ("metrics", fields.metrics.clone().map(Value::Object)),
            (
                "tags",
                if fields.tags.is_empty() {
                    None
                } else {
                    Some(Value::Array(crate::dsl::Array {
                        elem: fields
                            .tags
                            .iter()
                            .cloned()
                            .map(|template| ArrayElem::Item(Value::Template(template)))
                            .collect(),
                    }))
                },
            ),
            ("duration", fields.duration.as_ref().map(|(value, _)| value.clone())),
        ];
        if root && fields.input.is_none() {
            values[0].1 = Some(Value::Null);
        }
        values
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .map(|(key, value)| {
                let mut value = expression(&value, ctx, lang)?;
                if root && key == "input" {
                    value = Expr::Conditional {
                        condition: Box::new(Expr::Identity(Box::new("input".into()), Box::new("_MISSING".into()))),
                        yes: Box::new(value),
                        no: Box::new("input".into()),
                    };
                }
                if key == "duration" {
                    value = Expr::call("_duration", vec![value]);
                }
                Ok((key.into(), value))
            })
            .collect()
    }
    fn assign(target: Expr, value: Expr) -> Statement {
        Statement::Variable(Variable::Assign { name: target, value })
    }
    fn scope(
        fields_value: Option<&SpanFields>,
        bindings: &[Binding],
        children: &[Child],
        frame: &str,
        ctx: &Context<'_>,
        lang: ScorerLang,
        root: bool,
    ) -> Result<Vec<Statement>, String> {
        let mut operations = Vec::new();
        for binding in bindings {
            let name = &ctx.local_names.iter().find(|(name, _)| *name == binding.name).unwrap().1;
            let value = expression(&binding.value, ctx, lang)?;
            operations.push(Operation {
                key: Dependency::Variable(name.clone()),
                reads: value.references(),
                code: vec![Statement::Variable(Variable::constant(name, value.lazy()))],
            });
        }
        if let Some(fields_value) = fields_value {
            for (key, value) in fields(fields_value, ctx, lang, root)? {
                let reads = value.references();
                let statement = if ctx.direct {
                    let value = Expr::call("_value", vec![value]);
                    Statement::Evaluate(Expr::Call {
                        function: Box::new(Expr::from(frame).member("log")),
                        args: vec![],
                        keywords: vec![(key.clone(), value)],
                    })
                } else {
                    assign(Expr::from(frame).member("fields").index(Expr::string(&key)), value.lazy())
                };
                operations.push(Operation {
                    key: Dependency::Field(frame.into(), key),
                    reads,
                    code: vec![statement],
                });
            }
        }
        for child in children {
            operations.push(child_operation(child, frame, ctx, lang)?);
        }
        if ctx.direct {
            crate::mocking::code::planning::order(operations, frame)
        } else {
            Ok(operations.into_iter().flat_map(|op| op.code).collect())
        }
    }
    fn child_operation(child: &Child, parent: &str, ctx: &Context<'_>, lang: ScorerLang) -> Result<Operation, String> {
        let node = child_node(child);
        let names = ctx.names(node);
        let frame = &names.state;
        let mut child_ctx = ctx.clone();
        child_ctx.site = node;
        child_ctx.frames.push((node, frame.clone()));
        let (name, kind, bindings, children, fields) = match child {
            Child::Span(v) => (
                v.name.as_str(),
                match v.kind {
                    SpanKind::Task => "task",
                    SpanKind::Tool => "tool",
                    SpanKind::Llm => "llm",
                    SpanKind::Function => "function",
                },
                &v.bindings,
                &v.children,
                Some(&v.fields),
            ),
            Child::Repeat(v) => (
                v.name.as_deref().unwrap_or("repeat"),
                "repeat",
                &v.bindings,
                &v.children,
                None,
            ),
            Child::Choice(v) => (
                v.name.as_deref().unwrap_or("choice"),
                "choice",
                &v.bindings,
                &v.children,
                None,
            ),
            Child::Maybe(v) => (v.name.as_deref().unwrap_or("maybe"), "maybe", &v.bindings, &v.children, None),
            Child::Scorer(_) => return Err("scorer spans are unsupported".into()),
        };
        let mut parameters = vec![];
        if matches!(child, Child::Repeat(_)) {
            parameters = [&names.index, &names.count]
                .into_iter()
                .map(|name| {
                    let p = code::Parameter::new(name);
                    if lang == ScorerLang::Typescript {
                        p.typed("number")
                    } else {
                        p
                    }
                })
                .collect();
            child_ctx.iterations.push((node, names.index.clone(), names.count.clone()));
        }
        let branching = matches!(child, Child::Choice(_) | Child::Maybe(_));
        let mut work = scope(
            fields,
            bindings,
            if branching { &[] } else { children },
            frame,
            &child_ctx,
            lang,
            false,
        )?;
        match child {
            Child::Choice(_) => {
                work.push(assign(
                    Expr::from(frame.clone()).member("fields").index(Expr::string("chosen")),
                    Expr::call("_pick", vec![Expr::number(children.len()), random_name(lang).into()]),
                ));
                for (index, child) in children.iter().enumerate() {
                    work.push(Statement::If {
                        condition: read_field(frame.clone().into(), "chosen", ctx.direct)
                            .binary(BinOp::Eq, Expr::number(index)),
                        then_body: scope(None, &[], std::slice::from_ref(child), frame, &child_ctx, lang, false)?,
                        else_body: vec![],
                    });
                }
            }
            Child::Maybe(v) => {
                work.push(assign(
                    Expr::from(frame.clone()).member("fields").index(Expr::string("included")),
                    Expr::call("_include", vec![expression(&v.chance, ctx, lang)?, random_name(lang).into()]),
                ));
                work.push(Statement::If {
                    condition: Expr::call("_boolean", vec![read_field(frame.clone().into(), "included", ctx.direct)]),
                    then_body: scope(None, &[], children, frame, &child_ctx, lang, false)?,
                    else_body: vec![],
                });
            }
            _ => {}
        }
        work.push(Statement::Return(frame.clone().into()));
        let mut function = code::Function::new(&names.function, parameters);
        if lang == ScorerLang::Typescript {
            function.returns = Some("Span".into());
        }
        function.declare(frame, scope_expr(name, kind, parent.into()))?;
        if ctx.direct {
            function.body.push(Statement::TraceScope {
                scope: frame.clone().into(),
                body: work,
            });
        } else {
            function.body.extend(work);
        }
        let call = if let Child::Repeat(v) = child {
            Expr::call(
                "_repeat",
                vec![
                    expression(&v.count, ctx, lang)?,
                    names.function.clone().into(),
                    Expr::string(name),
                ],
            )
        } else {
            Expr::call(names.function.clone(), vec![])
        };
        let mut code = vec![Statement::Function(function)];
        let target = Expr::from(parent).member("children").index(Expr::string(frame));
        if ctx.direct {
            code.push(Statement::Variable(Variable::constant(frame, call)));
            code.push(assign(target, frame.clone().into()));
        } else {
            code.push(assign(target, call.lazy()));
        }
        Ok(Operation {
            key: Dependency::Child(parent.into(), frame.clone()),
            reads: code::references(&code),
            code,
        })
    }
}
// Prefer native values after dependency ordering has established execution order.
mod native {
    use super::*;
    use std::collections::HashMap;
    #[derive(Clone, Debug)]
    enum Type {
        Unknown,
        Span(HashMap<String, Type>, HashMap<String, Type>),
        List(Box<Type>),
        Record(Box<Branch>),
        Product(Box<Type>),
        Value,
        Optional(Box<Type>),
        Null,
        Bool,
        Integer(i128, i128),
        Number,
        String,
        Array(Vec<Type>),
        Object(HashMap<String, Type>),
        Deferred(Box<Type>),
    }
    #[derive(Clone, Default)]
    struct Environment {
        names: HashMap<String, Type>,
        fields: HashMap<(String, String), (Expr, Type)>,
        outputs: HashMap<String, (Expr, Type)>,
        eager: HashSet<String>,
        labels: HashMap<(String, String), String>,
        pending: HashMap<String, Expr>,
        captured: HashSet<String>,
        returns: HashMap<String, Type>,
        branches: HashMap<String, Branch>,
        records: HashMap<String, Record>,
        exports: HashMap<String, Vec<String>>,
        collections: HashMap<String, (Expr, Branch)>,
        input: Option<Type>,
        types: std::rc::Rc<std::cell::RefCell<HashMap<String, Type>>>,
    }
    #[derive(Clone, Debug)]
    struct Branch {
        span: Type,
        fields: HashMap<String, Type>,
        children: HashMap<String, (String, Type)>,
    }
    #[derive(Clone)]
    struct Record {
        value: Expr,
        shape: Branch,
    }
    struct Lifted {
        frame: String,
        constructor: Expr,
        result: String,
        output: Type,
        span: Type,
        fields: HashMap<String, Type>,
    }
    fn schema(value: &Expr, nullable: &HashSet<String>) -> Option<Json> {
        Some(match value {
            Expr::String(_) => json!({"type":"string"}),
            Expr::Number(_) => json!({"type":"number"}),
            Expr::Bool(_) => json!({"type":"boolean"}),
            Expr::Null => return None,
            Expr::Object(fields) => {
                let mut properties = serde_json::Map::new();
                for (key, value) in fields {
                    let mut property = schema(value, nullable)?;
                    if nullable.contains(key) {
                        property = json!({"anyOf":[property,{"type":"null"}]});
                    }
                    properties.insert(key.clone(), property);
                }
                json!({"type":"object", "properties":properties})
            }
            Expr::Array(items) if !items.is_empty() => {
                let first = schema(&items[0], nullable)?;
                if items
                    .iter()
                    .skip(1)
                    .any(|item| schema(item, nullable).as_ref() != Some(&first))
                {
                    return None;
                }
                json!({"type":"array", "items":first})
            }
            _ => return None,
        })
    }
    fn schema_type(schema: &Json) -> Type {
        if let Some(items) = schema.get("anyOf").and_then(Json::as_array) {
            return Type::Optional(Box::new(schema_type(&items[0])));
        }
        match schema["type"].as_str() {
            Some("string") => Type::String,
            Some("number") => Type::Number,
            Some("boolean") => Type::Bool,
            Some("null") => Type::Null,
            Some("array") => Type::List(Box::new(schema_type(&schema["items"]))),
            Some("object") => Type::Object(
                schema["properties"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), schema_type(v)))
                    .collect(),
            ),
            _ => Type::Unknown,
        }
    }
    fn schema_expr(schema: &Json) -> Expr {
        match schema {
            Json::Null => Expr::Null,
            Json::Bool(v) => Expr::Bool(*v),
            Json::Number(v) => Expr::number(v),
            Json::String(v) => Expr::string(v),
            Json::Array(items) => Expr::Array(items.iter().map(schema_expr).collect()),
            Json::Object(fields) => Expr::Object(fields.iter().map(|(k, v)| (k.clone(), schema_expr(v))).collect()),
        }
    }
    pub(crate) fn functions(functions: &mut Vec<code::Function>, lang: ScorerLang) -> Vec<(String, Json)> {
        let mut nullable = HashSet::new();
        let mut accessed = HashSet::new();
        for f in functions.iter_mut() {
            code::rewrite(&mut f.body, &mut |value| {
                if let Expr::Call { function, args, .. } = value
                    && matches!(&**function, Expr::Name(n) if n == "_index")
                    && let Some(Expr::String(key)) = args.get(1)
                {
                    accessed.insert(key.clone());
                }
                if let Expr::Call { function, args, .. } = value
                    && matches!(&**function, Expr::Name(n) if n == "_equal")
                    && args.iter().any(|arg| matches!(arg, Expr::Null))
                {
                    for arg in args {
                        if let Expr::Call { function, args, .. } = arg
                            && matches!(&**function, Expr::Name(n) if n == "_index")
                            && let Some(Expr::String(key)) = args.get(1)
                        {
                            nullable.insert(key.clone());
                        }
                    }
                }
            });
        }
        let mut input_schema = None;
        code::rewrite(&mut functions[0].body, &mut |value| {
            if let Expr::Call { function, keywords, .. } = value
                && matches!(&**function, Expr::Member(target, method) if method == "log" && matches!(&**target, Expr::Name(n) if n == "root"))
                && let Some((_, Expr::Call { args, .. })) = keywords.iter().find(|(key, _)| key == "input")
                && let Some(Expr::Conditional { yes, .. }) = args.first()
            {
                input_schema = schema(yes, &nullable);
            }
        });
        fn consumed(schema: &mut Json, accessed: &HashSet<String>) -> bool {
            if schema["type"] == "array" {
                return consumed(&mut schema["items"], accessed);
            }
            if let Some(properties) = schema.get_mut("properties").and_then(Json::as_object_mut) {
                properties.retain(|key, _| accessed.contains(key));
                return !properties.is_empty();
            }
            false
        }
        if input_schema.as_mut().is_some_and(|schema| !consumed(schema, &accessed)) {
            input_schema = None;
        }
        let input_type = input_schema.as_ref().map(schema_type);
        fn collect(body: &[Statement], used: &mut HashSet<String>, frames: &mut Vec<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, value, .. }) => {
                        used.insert(name.clone());
                        if matches!(value,Expr::New{class,..} if class=="Span") {
                            frames.push(name.clone());
                        }
                    }
                    Statement::Function(f) => {
                        used.insert(f.name.clone());
                        for p in &f.parameters {
                            used.insert(p.name.clone());
                        }
                        collect(&f.body, used, frames);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => collect(body, used, frames),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        collect(then_body, used, frames);
                        collect(else_body, used, frames);
                    }
                    _ => {}
                }
            }
        }
        // Bind a deferred value in its sole consumer when that consumer is invoked
        // exactly once. Evaluation remains deferred until the original first read.
        fn localize(body: &mut Vec<Statement>) {
            let candidates: Vec<_> = body
                .iter()
                .filter_map(|s| match s {
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Call { function, .. },
                        ..
                    }) if matches!(&**function, Expr::Name(n) if n == "_lazy") => Some(name.clone()),
                    _ => None,
                })
                .collect();
            for name in candidates {
                let mut consumers = Vec::new();
                let mut outside = false;
                for statement in body.iter() {
                    match statement {
                        Statement::Function(f) if code::references(&f.body).contains(&Dependency::Variable(name.clone())) => {
                            consumers.push(f.name.clone())
                        }
                        Statement::Function(_) => {}
                        Statement::Variable(Variable::Declare { name: n, .. }) if n == &name => {}
                        other => {
                            outside |=
                                code::references(std::slice::from_ref(other)).contains(&Dependency::Variable(name.clone()))
                        }
                    }
                }
                if outside || consumers.len() != 1 {
                    continue;
                }
                let consumer = &consumers[0];
                let mut calls = 0;
                for statement in body.iter().filter(|s| !matches!(s, Statement::Function(_))) {
                    code::rewrite(&mut [statement.clone()], &mut |e| {
                        if matches!(e, Expr::Call { function, .. } if matches!(&**function, Expr::Name(n) if n == consumer)) {
                            calls += 1;
                        }
                    });
                }
                if calls != 1 {
                    continue;
                }
                let Some(index) = body.iter().position(|s| matches!(s, Statement::Function(f) if &f.name == consumer && f.parameters.is_empty() && matches!(f.body.get(1), Some(Statement::TraceScope { .. })))) else { continue; };
                let declaration = body
                    .iter()
                    .find(|s| matches!(s, Statement::Variable(Variable::Declare { name: n, .. }) if n == &name))
                    .unwrap()
                    .clone();
                if let Statement::Function(f) = &mut body[index]
                    && let Statement::TraceScope { body, .. } = &mut f.body[1]
                {
                    body.insert(0, declaration);
                }
                body.retain(|s| !matches!(s, Statement::Variable(Variable::Declare { name: n, .. }) if n == &name));
            }
            for statement in body {
                match statement {
                    Statement::Function(f) => localize(&mut f.body),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => localize(body),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        localize(then_body);
                        localize(else_body);
                    }
                    _ => {}
                }
            }
        }
        for function in functions.iter_mut() {
            localize(&mut function.body);
        }
        fn exported_fields(body: &[Statement], ancestors: &[String], exports: &mut HashMap<String, Vec<String>>) {
            for statement in body {
                match statement {
                    Statement::Function(function) => {
                        let mut ancestors = ancestors.to_vec();
                        if let Some(Statement::Variable(Variable::Declare {
                            name,
                            value: Expr::New { class, .. },
                            ..
                        })) = function.body.first()
                            && class == "Span"
                        {
                            ancestors.push(name.clone());
                        }
                        exported_fields(&function.body, &ancestors, exports);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => {
                        exported_fields(body, ancestors, exports)
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        exported_fields(then_body, ancestors, exports);
                        exported_fields(else_body, ancestors, exports);
                    }
                    other => code::rewrite(&mut [other.clone()], &mut |value| {
                        if let Expr::Read {
                            dependency: Dependency::Field(frame, field),
                            ..
                        } = value
                        {
                            let frame = frame.strip_suffix("_selected").unwrap_or(frame).to_string();
                            if !ancestors.contains(&frame) {
                                let fields = exports.entry(frame).or_default();
                                if !fields.contains(field) {
                                    fields.push(field.clone());
                                    fields.sort();
                                }
                            }
                        }
                    }),
                }
            }
        }
        let mut exports = HashMap::new();
        for function in functions.iter() {
            exported_fields(&function.body, &["root".into()], &mut exports);
        }
        let mut used = HashSet::new();
        let mut frames = Vec::new();
        for function in functions.iter() {
            used.insert(function.name.clone());
            for p in &function.parameters {
                used.insert(p.name.clone());
            }
            collect(&function.body, &mut used, &mut frames);
        }
        let mut labels = HashMap::new();
        for frame in frames {
            for key in ["input", "output"] {
                let base = field_name(&frame, key, lang);
                let mut name = base.clone();
                let mut n = 2;
                while !used.insert(name.clone()) {
                    name = if lang == ScorerLang::Python {
                        format!("{base}_{n}")
                    } else {
                        format!("{base}{n}")
                    };
                    n += 1;
                }
                labels.insert((frame.clone(), key.into()), name);
            }
        }
        let types = std::rc::Rc::new(std::cell::RefCell::new(HashMap::new()));
        for function in functions.iter_mut() {
            let mut env = Environment {
                labels: labels.clone(),
                exports: exports.clone(),
                input: input_type.clone(),
                types: types.clone(),
                ..Environment::default()
            };
            function.body = block(std::mem::take(&mut function.body), &mut env, lang, None);
        }
        own_traces(functions);
        for function in functions.iter_mut() {
            code::rewrite(&mut function.body, &mut |value| {
                if let Expr::New { class, args } = value
                    && class == "Span"
                    && args.len() >= 4
                    && !matches!(args[2], Expr::Null)
                {
                    args[3] = Expr::Null;
                }
            });
        }
        observe(functions, lang);
        eager_values(functions, &types.borrow(), lang);
        pure_functions(functions);
        hoist(functions, lang);
        let mut contracts = Vec::new();
        if let Some(schema) = &input_schema {
            contracts.push(("AgentInput".into(), schema.clone()));
        }
        annotate(functions, &types.borrow(), &mut contracts, lang);
        expand_results(functions, lang, &types.borrow());
        local_names(functions, lang);
        native_locals(functions, &types.borrow(), &contracts, lang);
        idioms(functions, lang);
        request_parameters(functions, lang);
        aliases(functions);
        arithmetic_steps(functions, lang);
        if let Some(schema) = input_schema {
            let name = if lang == ScorerLang::Python {
                "parse_input"
            } else {
                "parseInput"
            };
            let parameter = if lang == ScorerLang::Python {
                code::Parameter::new("value")
            } else {
                code::Parameter::new("value").typed("unknown")
            };
            let mut parse = code::Function::new(name, vec![parameter]);
            parse.returns = Some("AgentInput".into());
            parse.body.push(Statement::Return(Expr::call(
                "validate_input",
                vec!["value".into(), schema_expr(&schema)],
            )));
            if let Some(execute) = functions.iter_mut().find(|f| f.name == "_execute") {
                let mut input = None;
                for statement in &mut execute.body {
                    if let Statement::TraceScope { body, .. } = statement
                        && let Some(index) = body.iter().position(|s| matches!(s, Statement::Variable(Variable::Declare { value: Expr::Call { function, .. }, .. }) if matches!(&**function, Expr::Name(n) if n == name)))
                    { input = Some(body.remove(index)); }
                }
                if let Some(mut input) = input {
                    if let Statement::Variable(Variable::Declare {
                        value: Expr::Call { args, .. },
                        ..
                    }) = &mut input
                        && let Some(Expr::Call {
                            function, args: values, ..
                        }) = args.first()
                        && matches!(&**function,Expr::Name(n) if n=="_value")
                        && values.len() == 1
                    {
                        args[0] = values[0].clone();
                    }
                    execute.body.insert(0, input);
                }
            }
            functions.insert(0, parse);
            return contracts;
        }
        contracts
    }
    fn type_schema(kind: &Type) -> Json {
        match kind {
            Type::Null => json!({"type":"null"}),
            Type::Bool => json!({"type":"boolean"}),
            Type::String => json!({"type":"string"}),
            Type::Integer(..) | Type::Number => json!({"type":"number"}),
            Type::Optional(inner) => json!({"anyOf":[type_schema(inner),{"type":"null"}]}),
            Type::Object(fields) => {
                json!({"type":"object", "properties": fields.iter().map(|(k,v)|(k.clone(),type_schema(v))).collect::<serde_json::Map<_,_>>()})
            }
            Type::Product(item) => type_schema(item),
            Type::Record(branch) => {
                if branch.fields.is_empty() && branch.children.len() == 1 {
                    return type_schema(&branch.children.values().next().unwrap().1);
                }
                let mut fields = branch.fields.clone();
                fields.extend(branch.children.values().cloned());
                type_schema(&Type::Object(fields))
            }
            Type::List(item) => json!({"type":"array","items":type_schema(item)}),
            Type::Array(items) => {
                json!({"type":"array","items":type_schema(&items.iter().cloned().reduce(join).unwrap_or(Type::Unknown))})
            }
            _ => json!({}),
        }
    }
    fn annotate(
        functions: &mut [code::Function],
        types: &HashMap<String, Type>,
        contracts: &mut Vec<(String, Json)>,
        lang: ScorerLang,
    ) {
        fn name(value: &str) -> String {
            value
                .split('_')
                .filter(|p| !p.is_empty())
                .map(|p| p[..1].to_uppercase() + &p[1..])
                .collect()
        }
        fn annotation(kind: &Type, label: &str, contracts: &mut Vec<(String, Json)>, lang: ScorerLang) -> String {
            match kind {
                Type::String => return if lang == ScorerLang::Python { "str" } else { "string" }.into(),
                Type::Integer(..) => return if lang == ScorerLang::Python { "int" } else { "number" }.into(),
                Type::Number => return if lang == ScorerLang::Python { "int | float" } else { "number" }.into(),
                Type::Bool => return if lang == ScorerLang::Python { "bool" } else { "boolean" }.into(),
                Type::Span(..) => return "Span".into(),
                Type::Unknown | Type::Value | Type::Deferred(_) => {
                    return if lang == ScorerLang::Python { "Any" } else { "any" }.into();
                }
                _ => {}
            }
            let schema = type_schema(kind);
            if let Some((name, _)) = contracts.iter().find(|(_, existing)| existing == &schema) {
                return name.clone();
            }
            if let Some((name, _)) = contracts
                .iter()
                .find(|(_, existing)| existing["type"] == "array" && existing["items"] == schema)
            {
                return format!("{name}Item");
            }
            let base = name(label);
            let mut label = base.clone();
            let mut n = 2;
            while contracts.iter().any(|(name, _)| name == &label) {
                label = format!("{base}{n}");
                n += 1;
            }
            contracts.push((label.clone(), schema));
            label
        }
        for function in functions {
            if function.exported || function.name == "_execute" {
                continue;
            }
            for parameter in &mut function.parameters {
                parameter.annotation = Some(if parameter.name == "context" {
                    "ExecutionContext".into()
                } else if let Some(kind) = types.get(&parameter.name) {
                    annotation(kind, &parameter.name, contracts, lang)
                } else {
                    parameter
                        .annotation
                        .clone()
                        .unwrap_or_else(|| if lang == ScorerLang::Python { "Any" } else { "any" }.into())
                });
            }
            if let Some(kind) = types.get(&format!("@{}", function.name)) {
                function.returns = Some(annotation(kind, &format!("{}_result", function.name), contracts, lang));
            }
        }
    }

    // Pure deferred expressions can be ordinary functions. Keep memoization for
    // sampling and span reads, whose repeated evaluation would change behavior.
    fn arithmetic_steps(functions: &mut [code::Function], lang: ScorerLang) {
        fn expand(body: &mut Vec<Statement>, used: &mut HashSet<String>, lang: ScorerLang) {
            let mut output = Vec::new();
            for mut statement in std::mem::take(body) {
                match &mut statement {
                    Statement::Variable(Variable::Declare { value, .. } | Variable::Assign { value, .. }) => {
                        if let Expr::Call { function, args, .. } = value
                            && matches!(&**function,Expr::Name(n) if n=="checked_number")
                            && let [Expr::Binary { left, .. }] = args.as_mut_slice()
                            && let Expr::Call { function, args, .. } = &**left
                            && matches!(&**function,Expr::Name(n) if n=="checked_number")
                            && let [Expr::Binary { op, .. }] = args.as_slice()
                        {
                            let label = match op {
                                BinOp::Sub => "difference",
                                BinOp::Add => "sum",
                                BinOp::Mul => "product",
                                _ => "amount",
                            };
                            let name = allocate(label, "", lang, used);
                            output.push(Statement::Variable(Variable::constant(&name, (**left).clone())));
                            **left = Expr::Name(name);
                        }
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => expand(body, used, lang),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        expand(then_body, used, lang);
                        expand(else_body, used, lang);
                    }
                    _ => {}
                }
                output.push(statement);
            }
            *body = output;
        }
        for function in functions {
            let mut used: HashSet<_> = function.parameters.iter().map(|p| p.name.clone()).collect();
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Name(name) = e {
                    used.insert(name.clone());
                }
            });
            expand(&mut function.body, &mut used, lang);
        }
    }

    fn aliases(functions: &mut [code::Function]) {
        fn simplify(
            body: &mut Vec<Statement>,
            inherited: &HashMap<String, Expr>,
            immutable: &HashSet<String>,
            uses: &HashMap<String, usize>,
        ) {
            let mut aliases = inherited.clone();
            let mut objects = HashMap::<String, Vec<(String, Expr)>>::new();
            let mut output = Vec::new();
            for mut statement in std::mem::take(body) {
                code::rewrite(std::slice::from_mut(&mut statement), &mut |value| {
                    if let Expr::Name(name) = value
                        && let Some(replacement) = aliases.get(name)
                    {
                        *value = replacement.clone();
                    }
                    if let Expr::Index(target, key) = value
                        && let (Expr::Name(name), Expr::String(key)) = (&**target, &**key)
                        && let Some(fields) = objects.get(name)
                        && let Some((_, field)) = fields.iter().find(|(name, _)| name == key)
                        && (matches!(field,Expr::Name(name) if immutable.contains(name))
                            || matches!(field, Expr::String(_) | Expr::Bool(_) | Expr::Number(_) | Expr::Null))
                    {
                        *value = field.clone();
                    }
                });
                if let Statement::Variable(Variable::Declare {
                    name,
                    value,
                    mutable: false,
                    ..
                }) = &statement
                {
                    if let Expr::Object(fields) = value {
                        objects.insert(name.clone(), fields.clone());
                    }
                    if !["request", "input", "span"].contains(&name.as_str())
                        && (matches!(value,Expr::Name(source) if immutable.contains(source))
                            || uses.get(name) == Some(&1) && matches!(value, Expr::String(_) | Expr::Bool(_) | Expr::Number(_)))
                    {
                        aliases.insert(name.clone(), value.clone());
                        continue;
                    }
                }
                match &mut statement {
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => {
                        simplify(body, &aliases, immutable, uses)
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        simplify(then_body, &aliases, immutable, uses);
                        simplify(else_body, &aliases, immutable, uses);
                    }
                    _ => {}
                }
                output.push(statement);
            }
            *body = output;
        }
        for function in functions {
            let mut mutable = HashSet::new();
            fn collect(body: &[Statement], immutable: &mut HashSet<String>, mutable: &mut HashSet<String>) {
                for s in body {
                    match s {
                        Statement::Variable(Variable::Declare {
                            name,
                            mutable: is_mutable,
                            ..
                        }) => {
                            if *is_mutable {
                                mutable.insert(name.clone());
                            } else {
                                immutable.insert(name.clone());
                            }
                        }
                        Statement::Variable(Variable::Assign {
                            name: Expr::Name(name), ..
                        }) => {
                            mutable.insert(name.clone());
                        }
                        Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => {
                            collect(body, immutable, mutable)
                        }
                        Statement::If {
                            then_body, else_body, ..
                        } => {
                            collect(then_body, immutable, mutable);
                            collect(else_body, immutable, mutable);
                        }
                        _ => {}
                    }
                }
            }
            let mut immutable: HashSet<_> = function.parameters.iter().map(|p| p.name.clone()).collect();
            collect(&function.body, &mut immutable, &mut mutable);
            immutable.retain(|name| !mutable.contains(name));
            let mut uses = HashMap::new();
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Name(name) = e {
                    *uses.entry(name.clone()).or_insert(0) += 1;
                }
            });
            simplify(&mut function.body, &HashMap::new(), &immutable, &uses);
        }
    }

    fn eager_values(functions: &mut [code::Function], types: &HashMap<String, Type>, lang: ScorerLang) {
        fn total(value: &Expr, env: &Environment, lang: ScorerLang) -> bool {
            match value {
                Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Name(name) => env
                    .names
                    .get(name)
                    .is_some_and(|t| !matches!(t, Type::Unknown | Type::Value | Type::Deferred(_))),
                Expr::Index(target, index) => {
                    total(target, env, lang)
                        && matches!((&expression((**target).clone(),env,lang).1, &**index), (Type::Object(fields),Expr::String(key)) if fields.contains_key(key))
                }
                Expr::Member(target, key) => {
                    total(target, env, lang)
                        && matches!(expression((**target).clone(),env,lang).1, Type::Object(fields) if fields.contains_key(key))
                }
                Expr::Binary { left, op, right } => {
                    matches!(
                        op,
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or
                    ) && total(left, env, lang)
                        && total(right, env, lang)
                }
                Expr::Unary { op: UnaryOp::Not, value } => total(value, env, lang),
                Expr::Template(items) => items.iter().all(|v| total(v, env, lang)),
                _ => false,
            }
        }
        fn visit(body: &mut Vec<Statement>, env: &Environment, lang: ScorerLang) {
            let candidates: Vec<_> = body
                .iter()
                .filter_map(|s| {
                    if let Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Call { function, args, .. },
                        ..
                    }) = s
                        && matches!(&**function,Expr::Name(n) if n=="_lazy")
                        && let [Expr::Lambda { parameters, value }] = args.as_slice()
                        && parameters.is_empty()
                        && total(value, env, lang)
                    {
                        Some((name.clone(), (**value).clone()))
                    } else {
                        None
                    }
                })
                .collect();
            for (name, value) in candidates {
                let first = body.iter().position(|s| {
                    !matches!(s, Statement::Variable(Variable::Declare {name:n,..}) if n == &name)
                        && code::references(std::slice::from_ref(s)).contains(&Dependency::Variable(name.clone()))
                });
                let Some(first) = first else { continue };
                let dependencies = code::references(&[Statement::Return(value.clone())]);
                if body[first..].iter().any(|s| matches!(s,Statement::Variable(Variable::Declare {name,..}) if dependencies.contains(&Dependency::Variable(name.clone())))) {continue;}
                // Leave computations inside their existing lexical execution scope.
                if matches!(&body[first], Statement::Function(_)) {
                    continue;
                }
                body.insert(first, Statement::Variable(Variable::constant(&name, value)));
                body.retain(|s| !matches!(s,Statement::Variable(Variable::Declare {name:n,value:Expr::Call {function,..},..}) if n == &name && matches!(&**function,Expr::Name(f) if f=="_lazy")));
                code::rewrite(body, &mut |e| {
                    if matches!(e,Expr::Call {function,args,keywords} if args.is_empty() && keywords.is_empty() && matches!(&**function,Expr::Name(n) if n==&name))
                    {
                        *e = Expr::Name(name.clone());
                    }
                });
            }
            for statement in body {
                match statement {
                    Statement::Function(f) => visit(&mut f.body, env, lang),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => visit(body, env, lang),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        visit(then_body, env, lang);
                        visit(else_body, env, lang);
                    }
                    _ => {}
                }
            }
        }
        let env = Environment {
            names: types.clone(),
            ..Environment::default()
        };
        for function in functions {
            visit(&mut function.body, &env, lang);
        }
    }

    fn request_parameters(functions: &mut [code::Function], lang: ScorerLang) {
        let mut projections = HashMap::new();
        for function in functions.iter_mut() {
            // A handler owns a single request; collection slicing belongs to its caller.
            for statement in &mut function.body {
                let Statement::TraceScope { body, .. } = statement else {
                    continue;
                };
                let Some(Statement::Variable(Variable::Declare {
                    name,
                    value: Expr::Call {
                        function: index, args, ..
                    },
                    ..
                })) = body.first()
                else {
                    continue;
                };
                if !matches!(&**index, Expr::Name(n) if n == "_index") || args.len() != 2 {
                    continue;
                }
                let Expr::Call {
                    function: slice,
                    args: bounds,
                    ..
                } = &args[0]
                else {
                    continue;
                };
                if !matches!(&**slice, Expr::Name(n) if n == "_slice") || bounds.len() != 3 || !matches!(bounds[1], Expr::Null)
                {
                    continue;
                }
                let (Expr::Name(index_name), Expr::Name(count_name)) = (&args[1], &bounds[2]) else {
                    continue;
                };
                let index_stem = index_name.strip_suffix("_index").or_else(|| index_name.strip_suffix("Index"));
                let count_stem = count_name.strip_suffix("_count").or_else(|| count_name.strip_suffix("Count"));
                if index_stem.is_none() || index_stem != count_stem {
                    continue;
                }
                let name = name.clone();
                let projection = Expr::call("_index", vec![bounds[0].clone(), args[1].clone()]);
                let reads = code::references(&body[1..]);
                let consumed = code::references(&body[..1]);
                let removed: Vec<_> = function
                    .parameters
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        consumed.contains(&Dependency::Variable(p.name.clone()))
                            && !reads.contains(&Dependency::Variable(p.name.clone()))
                    })
                    .map(|(i, _)| i)
                    .collect();
                if removed.is_empty() {
                    continue;
                }
                let old = function.parameters.clone();
                function.parameters = old
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !removed.contains(i))
                    .map(|(_, p)| p.clone())
                    .collect();
                let request_type = old.iter().find_map(|p| {
                    if let Expr::Name(n) = &bounds[0]
                        && n == &p.name
                    {
                        p.annotation.as_ref().map(|a| format!("{a}Item"))
                    } else {
                        None
                    }
                });
                let mut parameter = code::Parameter::new(&name);
                parameter.annotation = request_type.or_else(|| (lang == ScorerLang::Typescript).then(|| "any".into()));
                function.parameters.insert(0, parameter);
                body.remove(0);
                projections.insert(function.name.clone(), (old, removed, projection));
                break;
            }
        }
        for function in functions {
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Call { function, args, .. } = e
                    && let Expr::Name(name) = &**function
                    && let Some((old, removed, projection)) = projections.get(name)
                {
                    let bindings: HashMap<_, _> = old
                        .iter()
                        .zip(args.iter())
                        .map(|(p, a)| (p.name.clone(), a.clone()))
                        .collect();
                    let mut projection = projection.clone();
                    code::rewrite_expr(&mut projection, &mut |e| {
                        if let Expr::Name(n) = e
                            && let Some(v) = bindings.get(n)
                        {
                            *e = v.clone();
                        }
                    });
                    *args = args
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !removed.contains(i))
                        .map(|(_, a)| a.clone())
                        .collect();
                    args.insert(0, projection);
                }
            });
        }
    }

    fn pure_functions(functions: &mut [code::Function]) {
        fn convert(body: &mut [Statement]) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Call { function, args, .. },
                        ..
                    }) if matches!(&**function, Expr::Name(n) if n == "_lazy") => {
                        if let [Expr::Lambda { parameters, value }] = args.as_slice()
                            && parameters.is_empty()
                        {
                            let mut pure = true;
                            code::rewrite_expr(&mut (**value).clone(), &mut |e| match e {
                                Expr::Call { function, .. } if !matches!(&**function, Expr::Name(n) if ["_num", "_integer", "_index", "_equal", "_boolean", "_len", "_text", "_trim", "_float", "_value", "checked_number", "remainder", "divide"].contains(&n.as_str())) => {
                                    pure = false
                                }
                                Expr::Member(_, field)
                                    if ["fields", "children", "iterations", "output", "input"].contains(&field.as_str()) =>
                                {
                                    pure = false
                                }
                                Expr::Read { .. } | Expr::New { .. } => pure = false,
                                _ => {}
                            });
                            if pure {
                                let mut f = code::Function::new(name.clone(), vec![]);
                                f.body.push(Statement::Return((**value).clone()));
                                *statement = Statement::Function(f);
                            }
                        }
                    }
                    Statement::Function(f) => convert(&mut f.body),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => convert(body),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        convert(then_body);
                        convert(else_body);
                    }
                    _ => {}
                }
            }
        }
        for function in functions {
            convert(&mut function.body);
        }
    }

    fn idioms(functions: &mut Vec<code::Function>, lang: ScorerLang) {
        fn literal(value: &Expr) -> bool {
            matches!(value, Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null)
        }
        // Structural functions that just forward one result are not application boundaries.
        let wrappers: HashMap<_, _> = functions
            .iter()
            .filter_map(|f| {
                if let [
                    Statement::Variable(Variable::Declare {
                        name, value: Expr::Null, ..
                    }),
                    Statement::Variable(Variable::Assign {
                        name: Expr::Name(target),
                        value,
                    }),
                    Statement::Return(Expr::Name(result)),
                ] = f.body.as_slice()
                    && name == target
                    && name == result
                {
                    Some((f.name.clone(), (f.parameters.clone(), value.clone())))
                } else {
                    None
                }
            })
            .collect();
        let mut traverses = false;
        for function in functions.iter_mut() {
            code::rewrite(&mut function.body, &mut |e| {
                if matches!(e, Expr::Member(_, member) if member == "children" || member == "iterations") {
                    traverses = true;
                }
            });
        }
        for function in functions.iter_mut() {
            code::rewrite(&mut function.body, &mut |value| {
                if let Expr::Call { function, args, .. } = value
                    && let Expr::Name(name) = &**function
                {
                    if let Some((parameters, body)) = wrappers.get(name) {
                        let bindings: HashMap<_, _> = parameters
                            .iter()
                            .zip(args.iter())
                            .map(|(p, a)| (p.name.clone(), a.clone()))
                            .collect();
                        // Duplicating or dropping effectful arguments is not safe.
                        if args.iter().all(|a| matches!(a, Expr::Name(_) | Expr::Number(_))) {
                            let mut body = body.clone();
                            code::rewrite_expr(&mut body, &mut |e| {
                                if let Expr::Name(n) = e
                                    && let Some(v) = bindings.get(n)
                                {
                                    *e = v.clone();
                                }
                            });
                            *value = body;
                            return;
                        }
                    }
                    if name == "_float"
                        && let [Expr::Number(number)] = args.as_slice()
                        && (lang == ScorerLang::Python || number.parse::<f64>().is_ok_and(|v| v.fract() != 0.0))
                    {
                        *value = Expr::number(number);
                        return;
                    }
                    if name == "_duration"
                        && let [Expr::Number(number)] = args.as_slice()
                        && number.parse::<f64>().is_ok_and(|v| v.is_finite() && v >= 0.0)
                    {
                        *value = args[0].clone();
                        return;
                    }
                    if name == "_select"
                        && let [
                            Expr::Call {
                                function: pick,
                                args: choice,
                                ..
                            },
                            Expr::Array(options),
                        ] = args.as_slice()
                        && matches!(&**pick, Expr::Member(_,method) if method == "pick")
                        && choice.len() == 1
                    {
                        let options: Option<Vec<_>> = options
                            .iter()
                            .map(|o| {
                                if let Expr::Call { function, args, .. } = o
                                    && matches!(&**function, Expr::Name(n) if n == "_lazy")
                                    && let [Expr::Lambda { value, .. }] = args.as_slice()
                                    && literal(value)
                                {
                                    Some((**value).clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if let Some(options) = options {
                            *value = Expr::call(Expr::from("context").member("choose"), vec![Expr::Array(options)]);
                            return;
                        }
                    }
                    if name == "_weighted"
                        && let [weights, Expr::Array(options), random] = args.as_slice()
                        && matches!(random, Expr::Member(target, field) if field == "random" && matches!(&**target, Expr::Name(n) if n == "context"))
                    {
                        let options: Option<Vec<_>> = options
                            .iter()
                            .map(|o| {
                                if let Expr::Call { function, args, .. } = o
                                    && matches!(&**function, Expr::Name(n) if n == "_lazy")
                                    && let [Expr::Lambda { value, .. }] = args.as_slice()
                                    && literal(value)
                                {
                                    Some((**value).clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if let Some(options) = options {
                            *value = Expr::call(
                                Expr::from("context").member(if lang == ScorerLang::Python {
                                    "weighted_choice"
                                } else {
                                    "weightedChoice"
                                }),
                                vec![Expr::Array(options), weights.clone()],
                            );
                            return;
                        }
                    }
                    let method = match name.as_str() {
                        "_pick" => Some("pick"),
                        "_include" => Some("chance"),
                        "_random_text" => Some(if lang == ScorerLang::Python {
                            "random_text"
                        } else {
                            "randomText"
                        }),
                        "_sample" => Some("sample"),
                        _ => None,
                    };
                    if let Some(method) = method
                        && matches!(args.last(), Some(Expr::Member(target, field)) if field == "random" && matches!(&**target, Expr::Name(n) if n == "context"))
                    {
                        args.pop();
                        **function = Expr::from("context").member(method);
                    }
                }
            });
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Call {
                    function,
                    args,
                    keywords,
                } = e
                    && matches!(&**function, Expr::Member(_, method) if method == "log")
                {
                    let unwrap = |value: &mut Expr| {
                        if let Expr::Call { function, args, .. } = value
                            && matches!(&**function, Expr::Name(n) if n == "_value")
                            && args.len() == 1
                        {
                            *value = args[0].clone();
                        }
                    };
                    for (_, value) in keywords {
                        unwrap(value);
                    }
                    for arg in args {
                        if let Expr::Object(fields) = arg {
                            for (_, value) in fields {
                                unwrap(value);
                            }
                        }
                    }
                }
            });
            // No internal frame keys are needed once data dependencies are plain values.
            if !traverses {
                code::rewrite(&mut function.body, &mut |e| {
                    if let Expr::Call { function, args, .. } = e
                        && matches!(&**function, Expr::Member(target, method) if method == "trace" && matches!(&**target, Expr::Name(n) if n == "context"))
                    {
                        args.truncate(2);
                    }
                });
            }
        }
        let references: HashSet<_> = functions.iter().flat_map(|f| code::references(&f.body)).collect();
        functions.retain(|f| !wrappers.contains_key(&f.name) || references.contains(&Dependency::Variable(f.name.clone())));
    }

    fn native_locals(
        functions: &mut [code::Function],
        types: &HashMap<String, Type>,
        contracts: &[(String, Json)],
        lang: ScorerLang,
    ) {
        fn infer(body: &[Statement], env: &mut Environment, lang: ScorerLang) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, value, .. }) => {
                        let kind = expression(value.clone(), env, lang).1;
                        if !matches!(kind, Type::Unknown)
                            && !matches!(&kind,Type::Array(items) if items.is_empty() && env.names.contains_key(name))
                        {
                            env.names.insert(name.clone(), kind);
                        }
                    }
                    Statement::Variable(Variable::Assign {
                        name: Expr::Name(name),
                        value,
                    }) => {
                        let kind = expression(value.clone(), env, lang).1;
                        let previous = env.names.get(name).cloned().unwrap_or(Type::Null);
                        env.names.insert(
                            name.clone(),
                            if matches!(previous, Type::Null) {
                                kind
                            } else {
                                join(previous, kind)
                            },
                        );
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => infer(body, env, lang),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        infer(then_body, env, lang);
                        infer(else_body, env, lang);
                    }
                    _ => {}
                }
            }
        }
        fn apply(body: &mut [Statement], env: &Environment, contracts: &[(String, Json)], lang: ScorerLang) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Null,
                        annotation,
                        ..
                    }) => {
                        let kind = env.names.get(name).unwrap_or(&Type::Unknown);
                        let kind = if let Type::Optional(inner) = kind { &**inner } else { kind };
                        let scalar = match kind {
                            Type::String => Some(if lang == ScorerLang::Python { "str" } else { "string" }),
                            Type::Number | Type::Integer(..) => {
                                Some(if lang == ScorerLang::Python { "int | float" } else { "number" })
                            }
                            Type::Bool => Some(if lang == ScorerLang::Python { "bool" } else { "boolean" }),
                            _ => None,
                        };
                        let shape = type_schema(kind);
                        let scalar = scalar.map(str::to_string).or_else(|| {
                            contracts
                                .iter()
                                .find(|(_, schema)| schema == &shape)
                                .map(|(name, _)| name.clone())
                        });
                        if let Some(scalar) = scalar {
                            *annotation = Some(format!(
                                "{scalar} | {}",
                                if lang == ScorerLang::Python { "None" } else { "null" }
                            ));
                        }
                    }
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Array(items),
                        annotation,
                        ..
                    }) if items.is_empty() => {
                        if let Some(Type::List(item)) = env.names.get(name) {
                            let shape = type_schema(item);
                            if let Some((name, _)) = contracts.iter().find(|(_, schema)| schema == &shape) {
                                *annotation = Some(if lang == ScorerLang::Python {
                                    format!("list[{name}]")
                                } else {
                                    format!("Array<{name}>")
                                });
                            }
                        }
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => apply(body, env, contracts, lang),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        apply(then_body, env, contracts, lang);
                        apply(else_body, env, contracts, lang);
                    }
                    _ => {}
                }
            }
        }
        for function in functions {
            let mut env = Environment {
                names: types.clone(),
                ..Environment::default()
            };
            env.returns = types
                .iter()
                .filter_map(|(name, kind)| name.strip_prefix('@').map(|name| (name.to_string(), kind.clone())))
                .collect();
            for parameter in &function.parameters {
                if let Some(annotation) = &parameter.annotation {
                    if let Some((_, schema)) = contracts.iter().find(|(name, _)| name == annotation) {
                        env.names.insert(parameter.name.clone(), schema_type(schema));
                    } else if let Some((_, schema)) = contracts
                        .iter()
                        .find(|(name, schema)| format!("{name}Item") == *annotation && schema["type"] == "array")
                    {
                        env.names.insert(parameter.name.clone(), schema_type(&schema["items"]));
                    }
                }
            }
            infer(&function.body, &mut env, lang);
            if lang == ScorerLang::Typescript {
                code::rewrite(&mut function.body, &mut |value| {
                    if let Expr::Index(target, _) | Expr::Member(target, _) = value
                        && matches!(expression((**target).clone(), &env, lang).1, Type::Optional(_))
                    {
                        **target = Expr::Required(target.clone());
                    }
                });
            }
            apply(&mut function.body, &env, contracts, lang);
        }
    }

    // Trace records observe the execution; application results never come from
    // the collector. Structural control-flow scopes are not SDK spans.
    fn observe(functions: &mut [code::Function], lang: ScorerLang) {
        fn scopes(body: &mut Vec<Statement>) {
            let mut structural = HashSet::new();
            for statement in body.iter() {
                if let Statement::Variable(Variable::Declare {
                    name,
                    value: Expr::Call { function, args, .. },
                    ..
                }) = statement
                    && matches!(&**function, Expr::Member(_, method) if method == "trace")
                    && matches!(args.get(1), Some(Expr::String(kind)) if ["maybe", "choice", "repeat"].contains(&kind.as_str()))
                {
                    structural.insert(name.clone());
                }
            }
            // Keep a compatibility scope only if a remaining traversal needs it.
            let mut needed = HashSet::new();
            code::rewrite(body, &mut |value| {
                if let Expr::Member(target, field) = value
                    && ["children", "iterations"].contains(&field.as_str())
                    && let Expr::Name(name) = &**target
                {
                    needed.insert(name.clone());
                }
            });
            structural.retain(|name| !needed.contains(name));
            let mut output = Vec::new();
            for mut statement in std::mem::take(body) {
                match &mut statement {
                    Statement::Variable(Variable::Declare { name, .. }) if structural.contains(name) => continue,
                    Statement::TraceScope {
                        scope: Expr::Name(name),
                        body,
                    } if structural.contains(name) => {
                        let frame = name.clone();
                        fn fields(body: &mut [Statement], frame: &str) {
                            for statement in body {
                                match statement {
                                    Statement::Variable(Variable::Assign {
                                        name: Expr::Index(target, field),
                                        value,
                                    }) if matches!(&**target, Expr::Member(target, member) if member == "fields" && matches!(&**target, Expr::Name(n) if n == frame)) => {
                                        if let Expr::String(field) = &**field {
                                            *statement = Statement::Variable(Variable::constant(
                                                format!(
                                                    "{}_{}",
                                                    frame
                                                        .strip_suffix("_span")
                                                        .or_else(|| frame.strip_suffix("Span"))
                                                        .unwrap_or(frame),
                                                    field
                                                ),
                                                value.clone(),
                                            ));
                                        }
                                    }
                                    Statement::If {
                                        then_body, else_body, ..
                                    } => {
                                        fields(then_body, frame);
                                        fields(else_body, frame);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        fields(body, &frame);
                        code::rewrite(body, &mut |value| {
                            if let Expr::Member(target, field) = value
                                && matches!(&**target, Expr::Name(n) if n == &frame)
                                && ["chosen", "included"].contains(&field.as_str())
                            {
                                *value = Expr::Name(format!(
                                    "{}_{}",
                                    frame
                                        .strip_suffix("_span")
                                        .or_else(|| frame.strip_suffix("Span"))
                                        .unwrap_or(&frame),
                                    field
                                ));
                            }
                        });
                        scopes(body);
                        output.append(body);
                        continue;
                    }
                    Statement::Function(f) => scopes(&mut f.body),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => scopes(body),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        scopes(then_body);
                        scopes(else_body);
                    }
                    _ => {}
                }
                output.push(statement);
            }
            *body = output;
        }
        for function in functions {
            if function.name != "_execute" {
                continue;
            }
            let mut result = Expr::Null;
            code::rewrite(&mut function.body, &mut |value| {
                if let Expr::Call { function, keywords, .. } = value
                    && matches!(&**function, Expr::Member(target, method) if method == "log" && matches!(&**target, Expr::Name(n) if n == "root"))
                    && let Some((_, value)) = keywords.iter().find(|(key, _)| key == "output")
                {
                    result = value.clone();
                }
            });
            code::rewrite(&mut function.body, &mut |value| match value {
                Expr::New { class, args } if class == "Span" => {
                    let mut arguments = args[..2].to_vec();
                    if let Some(key) = args.get(4) {
                        arguments.push(key.clone());
                    }
                    *value = Expr::call(Expr::from("context").member("trace"), arguments);
                }
                Expr::Call { function, .. } if matches!(&**function, Expr::Name(n) if n == "_details") => {
                    *value = Expr::call(Expr::from("context").member("finish"), vec![result.clone()]);
                }
                Expr::Name(name) if name == random_name(lang) => {
                    *value = Expr::from("context").member("random");
                }
                _ => {}
            });
            scopes(&mut function.body);
            function.body.insert(
                0,
                Statement::Variable(Variable::constant(
                    "context",
                    Expr::New {
                        class: "ExecutionContext".into(),
                        args: vec!["logger".into(), random_name(lang).into()],
                    },
                )),
            );
        }
    }

    fn expand_results(functions: &mut [code::Function], lang: ScorerLang, types: &HashMap<String, Type>) {
        fn collect(body: &[Statement], used: &mut HashSet<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, .. }) => {
                        used.insert(name.clone());
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => collect(body, used),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        collect(then_body, used);
                        collect(else_body, used);
                    }
                    _ => {}
                }
            }
        }
        fn simple(value: &Expr, env: &Environment, lang: ScorerLang) -> bool {
            match value {
                Expr::Name(_) | Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Index(target, key) => {
                    simple(target, env, lang)
                        && matches!((&expression((**target).clone(),env,lang).1,&**key), (Type::Object(fields),Expr::String(key)) if fields.contains_key(key))
                }
                _ => false,
            }
        }
        fn expand(body: &mut Vec<Statement>, used: &mut HashSet<String>, lang: ScorerLang, env: &Environment) {
            let mut output = Vec::new();
            for mut statement in std::mem::take(body) {
                match &mut statement {
                    Statement::Variable(Variable::Declare { value, .. }) => {
                        let object = match value {
                            Expr::Object(fields) => Some(fields),
                            Expr::Call { function, args, .. } if matches!(&**function, Expr::Name(n) if n == "_value") => {
                                match args.first_mut() {
                                    Some(Expr::Object(fields)) => Some(fields),
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some(fields) = object
                            && fields.iter().any(|(_, v)| matches!(v, Expr::Conditional { .. }))
                        {
                            for (key, value) in fields {
                                if simple(value, env, lang) {
                                    continue;
                                }
                                let name = allocate(key, "", lang, used);
                                let initializer = std::mem::replace(value, Expr::Name(name.clone()));
                                if matches!(initializer, Expr::Conditional { .. }) {
                                    output.push(Statement::Variable(Variable::Declare {
                                        name: name.clone(),
                                        value: Expr::Null,
                                        mutable: true,
                                        annotation: None,
                                    }));
                                    output.extend(assign_value(&name, initializer));
                                } else {
                                    output.push(Statement::Variable(Variable::constant(name, initializer)));
                                }
                            }
                        }
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => expand(body, used, lang, env),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        expand(then_body, used, lang, env);
                        expand(else_body, used, lang, env);
                    }
                    _ => {}
                }
                output.push(statement);
            }
            *body = output;
        }
        for function in functions {
            let mut used: HashSet<_> = function.parameters.iter().map(|p| p.name.clone()).collect();
            collect(&function.body, &mut used);
            code::rewrite(&mut function.body, &mut |value| {
                if let Expr::Name(name) = value {
                    used.insert(name.clone());
                }
            });
            expand(
                &mut function.body,
                &mut used,
                lang,
                &Environment {
                    names: types.clone(),
                    ..Environment::default()
                },
            );
        }
    }

    fn local_names(functions: &mut [code::Function], lang: ScorerLang) {
        fn declarations(body: &mut [Statement], names: &HashMap<String, String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, .. }) => {
                        if let Some(new) = names.get(name) {
                            *name = new.clone();
                        }
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => declarations(body, names),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        declarations(then_body, names);
                        declarations(else_body, names);
                    }
                    _ => {}
                }
            }
        }
        for function in functions {
            if function.exported || function.name.starts_with('_') {
                continue;
            }
            let mut used = HashSet::new();
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Name(n) = e {
                    used.insert(n.clone());
                }
            });
            used.extend(function.parameters.iter().map(|p| p.name.clone()));
            let mut names = HashMap::new();
            if !used.contains("parent")
                && let Some(Statement::Variable(Variable::Declare {
                    value: Expr::New { class, args },
                    ..
                })) = function.body.first()
                && class == "Span"
                && let Some(Expr::Name(parent)) = args.get(2)
                && function.parameters.iter().any(|p| &p.name == parent)
            {
                names.insert(parent.clone(), "parent".into());
            }
            for suffix in ["span", "request", "result"] {
                let old = if lang == ScorerLang::Python {
                    format!("{}_{suffix}", function.name)
                } else {
                    format!("{}{}{}", function.name, suffix[..1].to_uppercase(), &suffix[1..])
                };
                if used.contains(&old) && !used.contains(suffix) {
                    names.insert(old, suffix.to_string());
                }
            }
            for (field, local) in [("included", "included"), ("chosen", "selection")] {
                let old = format!("{}_{field}", function.name);
                if used.contains(&old) && !used.contains(local) {
                    names.insert(old, local.into());
                }
            }
            // Prefer the request's role over the generated name of its owning span.
            for parameter in &function.parameters {
                if names.contains_key(&parameter.name) {
                    continue;
                }
                if (parameter.name.ends_with("_request") || parameter.name.ends_with("Request")) && !used.contains("input") {
                    names.insert(parameter.name.clone(), "input".into());
                    used.insert("input".into());
                }
            }
            let mut candidates: Vec<_> = used.iter().cloned().collect();
            candidates.sort();
            for old in candidates {
                if names.contains_key(&old) {
                    continue;
                }
                let Some(stem) = old.strip_suffix("_result").or_else(|| old.strip_suffix("Result")) else {
                    continue;
                };
                for verb in ["lookup", "check", "draft", "get", "fetch", "create"] {
                    let Some(rest) = stem.strip_prefix(verb) else { continue };
                    let rest = rest.trim_start_matches('_');
                    if rest.is_empty() {
                        continue;
                    }
                    let mut new = rest[..1].to_lowercase() + &rest[1..];
                    if used.contains(&new) || names.values().any(|n| n == &new) {
                        new = if lang == ScorerLang::Python {
                            format!("{new}_{verb}")
                        } else {
                            format!("{new}{}{}", verb[..1].to_uppercase(), &verb[1..])
                        };
                    }
                    if !used.contains(&new) && !names.values().any(|n| n == &new) {
                        used.insert(new.clone());
                        names.insert(old.clone(), new);
                    }
                    break;
                }
            }
            for parameter in &mut function.parameters {
                if let Some(name) = names.get(&parameter.name) {
                    parameter.name = name.clone();
                }
                if lang == ScorerLang::Typescript {
                    if parameter.name == "parent" {
                        parameter.annotation = Some("Span".into());
                    }
                    if parameter.name == "randomSource" {
                        parameter.annotation = Some("() => number".into());
                    }
                }
            }
            declarations(&mut function.body, &names);
            code::rewrite(&mut function.body, &mut |e| {
                if let Expr::Name(n) = e
                    && let Some(new) = names.get(n)
                {
                    *n = new.clone();
                }
            });
        }
    }

    // A business function owns its span. The parent retains the span only for
    // detailed trace inspection; the caller receives the ordinary return value.
    fn own_traces(functions: &mut [code::Function]) {
        fn convert(body: &mut Vec<Statement>) {
            let mut spans = HashMap::<String, Expr>::new();
            let mut constructors = HashMap::<String, Expr>::new();
            for statement in body.iter() {
                if let Statement::Variable(Variable::Declare {
                    name,
                    value: Expr::New { class, args },
                    ..
                }) = statement
                    && class == "Span"
                    && args.len() == 4
                {
                    constructors.insert(
                        name.clone(),
                        Expr::New {
                            class: class.clone(),
                            args: args.clone(),
                        },
                    );
                }
            }
            let mut owned = HashMap::<String, String>::new();
            for statement in body.iter_mut() {
                if let Statement::Function(function) = statement
                    && let Some(parameter) = function.parameters.first()
                    && let Some(mut constructor) = constructors.get(&parameter.name).cloned()
                {
                    let frame = parameter.name.clone();
                    if let Expr::New { args, .. } = &mut constructor {
                        spans.insert(
                            frame.clone(),
                            Expr::call("_child", vec![args[2].clone(), Expr::string(&frame)]),
                        );
                        args.push(Expr::string(&frame));
                    }
                    function.parameters.remove(0);
                    function
                        .body
                        .insert(0, Statement::Variable(Variable::constant(&frame, constructor)));
                    owned.insert(function.name.clone(), frame);
                }
            }
            body.retain(|statement| {
                if let Statement::Variable(Variable::Declare { name, .. }) = statement {
                    return !spans.contains_key(name);
                }
                if let Statement::Variable(Variable::Assign {
                    value: Expr::Name(name), ..
                }) = statement
                {
                    return !spans.contains_key(name);
                }
                true
            });
            for statement in body.iter_mut() {
                if let Statement::Variable(
                    Variable::Declare {
                        value: Expr::Call { function, args, .. },
                        ..
                    }
                    | Variable::Assign {
                        value: Expr::Call { function, args, .. },
                        ..
                    },
                ) = statement
                    && let Expr::Name(name) = &**function
                    && owned.contains_key(name)
                {
                    args.remove(0);
                }
                match statement {
                    Statement::Function(function) => convert(&mut function.body),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => convert(body),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        convert(then_body);
                        convert(else_body);
                    }
                    _ => {}
                }
            }
            // Rewrite outside the function which owns a binding, including sibling
            // functions that may still inspect another step's trace fields.
            for statement in body.iter_mut() {
                if matches!(statement, Statement::Function(f) if owned.contains_key(&f.name)) {
                    if let Statement::Function(f) = statement {
                        let own = &owned[&f.name];
                        code::rewrite(&mut f.body, &mut |value| {
                            if let Expr::Name(name) = value
                                && name != own
                                && let Some(replacement) = spans.get(name)
                            {
                                *value = replacement.clone();
                            }
                        });
                    }
                } else {
                    code::rewrite(std::slice::from_mut(statement), &mut |value| {
                        if let Expr::Name(name) = value
                            && let Some(replacement) = spans.get(name)
                        {
                            *value = replacement.clone();
                        }
                    });
                }
            }
        }
        for function in functions {
            convert(&mut function.body);
        }
    }

    // Closure conversion: module-level functions receive their lexical dependencies
    // explicitly. Capture arguments are read at the call site, never at definition time.
    fn hoist(functions: &mut Vec<code::Function>, lang: ScorerLang) {
        fn extract(body: &mut Vec<Statement>, extracted: &mut Vec<code::Function>) {
            let mut kept = Vec::new();
            for mut statement in std::mem::take(body) {
                match &mut statement {
                    Statement::Function(function) => {
                        extract(&mut function.body, extracted);
                        extracted.push(function.clone());
                        continue;
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        extract(then_body, extracted);
                        extract(else_body, extracted);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => extract(body, extracted),
                    _ => {}
                }
                kept.push(statement);
            }
            *body = kept;
        }
        fn locals(body: &[Statement], names: &mut HashSet<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, .. }) => {
                        names.insert(name.clone());
                    }
                    Statement::ForRange { index, body, .. } => {
                        names.insert(index.clone());
                        locals(body, names);
                    }
                    Statement::TraceScope { body, .. } => locals(body, names),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        locals(then_body, names);
                        locals(else_body, names);
                    }
                    _ => {}
                }
            }
        }
        let mut extracted = Vec::new();
        for f in functions.iter_mut() {
            extract(&mut f.body, &mut extracted);
        }
        let mut bindings = HashSet::new();
        for f in functions.iter().chain(&extracted) {
            locals(&f.body, &mut bindings);
            bindings.extend(f.parameters.iter().map(|p| p.name.clone()));
        }
        let mut captures: HashMap<String, Vec<String>> = extracted.iter().map(|f| (f.name.clone(), vec![])).collect();
        loop {
            let previous = captures.clone();
            for f in &extracted {
                let mut owned = HashSet::new();
                locals(&f.body, &mut owned);
                owned.extend(f.parameters.iter().map(|p| p.name.clone()));
                let refs = code::references(&f.body);
                let mut needed = HashSet::new();
                for dependency in refs {
                    let name = match dependency {
                        Dependency::Variable(n) | Dependency::Field(n, _) | Dependency::Child(n, _) => n,
                    };
                    if bindings.contains(&name) {
                        needed.insert(name.clone());
                    }
                    if let Some(transitive) = previous.get(&name) {
                        needed.extend(transitive.iter().cloned());
                    }
                }
                needed.retain(|n| !owned.contains(n));
                let mut needed: Vec<_> = needed.into_iter().collect();
                needed.sort();
                captures.insert(f.name.clone(), needed);
            }
            if captures == previous {
                break;
            }
        }
        for f in functions.iter_mut().chain(extracted.iter_mut()) {
            code::rewrite(&mut f.body, &mut |expr| {
                if let Expr::Call { function, args, .. } = expr
                    && let Expr::Name(name) = &**function
                    && let Some(names) = captures.get(name)
                {
                    args.splice(0..0, names.iter().cloned().map(Expr::Name));
                }
            });
        }
        for f in &mut extracted {
            let params = captures[&f.name].iter().map(|name| {
                let p = code::Parameter::new(name);
                if lang == ScorerLang::Typescript { p.typed("any") } else { p }
            });
            f.parameters.splice(0..0, params);
        }
        extracted.append(functions);
        *functions = extracted;
    }

    fn export_name(field: &str) -> &str {
        match field {
            "input" => "request",
            "output" => "result",
            field => field,
        }
    }
    fn label(env: &Environment, frame: &str, key: &str, lang: ScorerLang) -> String {
        env.labels
            .get(&(frame.into(), key.into()))
            .cloned()
            .unwrap_or_else(|| field_name(frame, key, lang))
    }
    fn field_name(frame: &str, key: &str, lang: ScorerLang) -> String {
        let stem = frame
            .strip_suffix("_span")
            .or_else(|| frame.strip_suffix("Span"))
            .unwrap_or(frame);
        let suffix = if key == "input" { "request" } else { "result" };
        if lang == ScorerLang::Python {
            format!("{stem}_{suffix}")
        } else {
            format!("{stem}{}{}", suffix[..1].to_uppercase(), &suffix[1..])
        }
    }
    fn block(
        statements: Vec<Statement>,
        env: &mut Environment,
        lang: ScorerLang,
        return_frame: Option<&str>,
    ) -> Vec<Statement> {
        let mut output = Vec::new();
        let mut lifted = HashMap::<String, Lifted>::new();
        for statement in statements {
            match statement {
                Statement::Function(mut function) => {
                    for dependency in code::references(&function.body) {
                        if let Dependency::Variable(name) = dependency {
                            env.captured.insert(name);
                        }
                    }
                    let mut child_env = env.clone();
                    let lift = match function.body.first() {
                        Some(Statement::Variable(Variable::Declare {
                            name,
                            value: Expr::New { class, args },
                            ..
                        })) if class == "Span"
                            && matches!(args.get(1),Some(Expr::String(kind)) if ["task","tool","llm","function"].contains(&kind.as_str()))
                            && matches!(function.body.get(1), Some(Statement::TraceScope { .. })) =>
                        {
                            Some((
                                name.clone(),
                                Expr::New {
                                    class: class.clone(),
                                    args: args.clone(),
                                },
                            ))
                        }
                        _ => None,
                    };
                    if let Some((frame, constructor)) = lift {
                        child_env
                            .names
                            .insert(frame.clone(), Type::Span(HashMap::new(), HashMap::new()));
                        function.body.remove(0);
                        let parameter = code::Parameter::new(&frame);
                        function.parameters.insert(
                            0,
                            if lang == ScorerLang::Typescript {
                                parameter.typed("Span")
                            } else {
                                parameter
                            },
                        );
                        function.body = block(function.body, &mut child_env, lang, Some(&frame));
                        let kind = child_env
                            .fields
                            .get(&(frame.clone(), "output".into()))
                            .map(|(_, t)| t.clone())
                            .unwrap_or(Type::Null);
                        lifted.insert(
                            function.name.clone(),
                            Lifted {
                                result: label(env, &frame, "output", lang),
                                frame: frame.clone(),
                                constructor,
                                output: kind,
                                span: child_env.names.get(&frame).cloned().unwrap_or(Type::Unknown),
                                fields: env
                                    .exports
                                    .get(&frame)
                                    .into_iter()
                                    .flatten()
                                    .map(|key| {
                                        (
                                            key.clone(),
                                            child_env
                                                .fields
                                                .get(&(frame.clone(), key.clone()))
                                                .map(|(_, t)| t.clone())
                                                .unwrap_or(Type::Null),
                                        )
                                    })
                                    .collect(),
                            },
                        );
                        if lang == ScorerLang::Typescript {
                            function.returns = Some("any".into());
                        }
                    } else {
                        function.body = block(function.body, &mut child_env, lang, None);
                    }
                    if let Some(branch) = branch_result(&mut function, &child_env, lang) {
                        env.branches.insert(function.name.clone(), branch);
                    }
                    fn returned(body: &[Statement]) -> Option<&Expr> {
                        match body.last()? {
                            Statement::Return(value) => Some(value),
                            Statement::TraceScope { body, .. } => returned(body),
                            _ => None,
                        }
                    }
                    if let Some(value) = returned(&function.body) {
                        env.returns
                            .insert(function.name.clone(), expression(value.clone(), &child_env, lang).1);
                    }
                    if let Some(branch) = env.branches.get(&function.name) {
                        let kind = if branch.fields.is_empty() && branch.children.len() == 1 {
                            branch.children.values().next().unwrap().1.clone()
                        } else {
                            Type::Object(
                                branch
                                    .fields
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .chain(branch.children.values().map(|(k, v)| (k.clone(), v.clone())))
                                    .collect(),
                            )
                        };
                        env.returns.insert(function.name.clone(), kind);
                    }
                    if let Some(kind) = env.returns.get(&function.name) {
                        env.types.borrow_mut().insert(format!("@{}", function.name), kind.clone());
                    }
                    output.push(Statement::Function(function));
                }
                Statement::Variable(Variable::Declare {
                    name,
                    value,
                    mutable,
                    annotation,
                }) => {
                    if let Expr::Call {
                        function,
                        args,
                        keywords,
                    } = &value
                        && let Expr::Name(callee) = &**function
                        && let Some(lift) = lifted.get(callee)
                    {
                        let constructor = expression(lift.constructor.clone(), env, lang).0;
                        output.push(Statement::Variable(Variable::constant(&lift.frame, constructor)));
                        let mut args = args.clone();
                        args.insert(0, lift.frame.clone().into());
                        let call = Expr::Call {
                            function: function.clone(),
                            args,
                            keywords: keywords.clone(),
                        };
                        output.push(Statement::Variable(Variable::constant(&lift.result, call)));
                        env.names.insert(lift.frame.clone(), lift.span.clone());
                        let bundled = lift.fields.keys().any(|key| key != "output");
                        let result = Expr::from(lift.result.clone());
                        let output_value = if bundled {
                            result.clone().index(Expr::string("result"))
                        } else {
                            result.clone()
                        };
                        env.names.insert(
                            lift.result.clone(),
                            if bundled {
                                Type::Object(lift.fields.iter().map(|(k, v)| (export_name(k).into(), v.clone())).collect())
                            } else {
                                lift.output.clone()
                            },
                        );
                        env.outputs.insert(lift.frame.clone(), (output_value, lift.output.clone()));
                        for (field, kind) in &lift.fields {
                            let value = if bundled {
                                result.clone().index(Expr::string(export_name(field)))
                            } else {
                                result.clone()
                            };
                            env.fields.insert((lift.frame.clone(), field.clone()), (value, kind.clone()));
                        }
                        continue;
                    }
                    if let Expr::Call { function, .. } = &value
                        && let Expr::Name(callee) = &**function
                        && let Some(branch) = env.branches.get(callee).cloned()
                    {
                        let result = label(env, &name, "output", lang);
                        output.push(Statement::Variable(Variable::constant(
                            &result,
                            expression(value, env, lang).0,
                        )));
                        env.names.insert(name.clone(), branch.span.clone());
                        env.names.insert(result.clone(), Type::Record(Box::new(branch.clone())));
                        env.records.insert(
                            name,
                            Record {
                                value: result.into(),
                                shape: branch,
                            },
                        );
                        continue;
                    }
                    // Repetition is visible as a native loop, retaining the trace collection.
                    if let Expr::Call { function, args, .. } = &value
                        && matches!(&**function,Expr::Name(n) if n=="_repeat")
                        && args.len() == 3
                    {
                        let stem = name
                            .strip_suffix("_span")
                            .or_else(|| name.strip_suffix("Span"))
                            .unwrap_or(&name);
                        let count = if lang == ScorerLang::Python {
                            format!("{stem}_count")
                        } else {
                            format!("{stem}Count")
                        };
                        let index = if lang == ScorerLang::Python {
                            format!("{stem}_index")
                        } else {
                            format!("{stem}Index")
                        };
                        let size = Expr::call("_count", vec![args[0].clone()]);
                        let size = expression(size, env, lang).0;
                        output.push(Statement::Variable(Variable::constant(&count, size)));
                        if let Expr::Name(callee) = &args[1]
                            && let Some(branch) = env.branches.get(callee).cloned()
                        {
                            let results = label(env, &name, "output", lang);
                            output.push(Statement::Variable(Variable::constant(&results, Expr::Array(vec![]))));
                            output.push(Statement::ForRange {
                                index: index.clone(),
                                count: count.clone().into(),
                                body: vec![Statement::Evaluate(Expr::call(
                                    Expr::from(results.clone()).member(if lang == ScorerLang::Python {
                                        "append"
                                    } else {
                                        "push"
                                    }),
                                    vec![Expr::call(callee.as_str(), vec![index.into(), count.into()])],
                                ))],
                            });
                            env.names
                                .insert(results.clone(), Type::List(Box::new(Type::Record(Box::new(branch.clone())))));
                            env.collections.insert(name, (results.into(), branch));
                            continue;
                        }
                        output.push(Statement::Variable(Variable::constant(
                            &name,
                            Expr::New {
                                class: "Span".into(),
                                args: vec![args[2].clone(), Expr::string("repeat")],
                            },
                        )));
                        let append = Expr::from(name.clone())
                            .member("iterations")
                            .member(if lang == ScorerLang::Python { "append" } else { "push" });
                        output.push(Statement::ForRange {
                            index: index.clone(),
                            count: count.clone().into(),
                            body: vec![Statement::Evaluate(Expr::call(
                                append,
                                vec![Expr::call(args[1].clone(), vec![index.into(), count.into()])],
                            ))],
                        });
                        continue;
                    }
                    let was_lazy =
                        matches!(&value,Expr::Call{function,..} if matches!(&**function,Expr::Name(n) if n=="_lazy"));
                    let (mut value, mut kind) = expression(value, env, lang);
                    if was_lazy
                        && !matches!(&value,Expr::Call{function,..} if matches!(&**function,Expr::Name(n) if n=="_lazy"))
                    {
                        env.eager.insert(name.clone());
                    }
                    if let Expr::Call { function, args, .. } = &value
                        && matches!(&**function,Expr::Name(n) if n=="_lazy")
                        && let Some(Expr::Lambda {
                            parameters,
                            value: inner,
                        }) = args.first()
                        && parameters.is_empty()
                        && infallible(inner, env)
                    {
                        let (v, t) = expression((**inner).clone(), env, lang);
                        value = v;
                        kind = t;
                        env.eager.insert(name.clone());
                    }
                    if let Expr::Call { function, args, .. } = &value
                        && matches!(&**function,Expr::Name(n) if n=="_lazy")
                        && let Some(Expr::Lambda { parameters, value }) = args.first()
                        && parameters.is_empty()
                    {
                        env.pending.insert(name.clone(), (**value).clone());
                    }
                    env.names.insert(name.clone(), kind);
                    output.push(Statement::Variable(Variable::Declare {
                        name,
                        value,
                        mutable,
                        annotation,
                    }));
                }
                Statement::Evaluate(Expr::Call {
                    function,
                    args,
                    keywords,
                }) if matches!(&*function,Expr::Member(_,method) if method=="log") => {
                    let frame = match &*function {
                        Expr::Member(frame, _) => match &**frame {
                            Expr::Name(name) => Some(name.clone()),
                            _ => None,
                        },
                        _ => None,
                    };
                    let mut fields = Vec::new();
                    for (key, value) in keywords {
                        materialize(&value, env, &mut output, lang);
                        let (mut value, mut kind) = expression(value, env, lang);
                        if frame.as_deref() == Some("root")
                            && key == "input"
                            && let Some(input) = &env.input
                        {
                            value = Expr::call(
                                if lang == ScorerLang::Python {
                                    "parse_input"
                                } else {
                                    "parseInput"
                                },
                                vec![value],
                            );
                            kind = input.clone();
                        }
                        if let Some(frame) = &frame
                            && ["input", "output"].contains(&key.as_str())
                        {
                            let name = label(env, frame, &key, lang);
                            if matches!(value, Expr::Conditional { .. }) {
                                output.push(Statement::Variable(Variable::Declare {
                                    name: name.clone(),
                                    value: Expr::Null,
                                    mutable: true,
                                    annotation: None,
                                }));
                                output.extend(assign_value(&name, value));
                            } else {
                                output.push(Statement::Variable(Variable::constant(&name, value)));
                            }
                            env.names.insert(name.clone(), kind.clone());
                            if let Some(Type::Span(fields, _)) = env.names.get_mut(frame) {
                                fields.insert(key.clone(), kind.clone());
                            }
                            env.fields.insert((frame.clone(), key.clone()), (name.clone().into(), kind));
                            fields.push((key, name.into()));
                        } else {
                            fields.push((key, value));
                        }
                    }
                    output.push(Statement::Evaluate(Expr::Call {
                        function,
                        args,
                        keywords: fields,
                    }));
                }
                Statement::Variable(Variable::Assign { name, value }) => {
                    let (value, kind) = expression(value, env, lang);
                    if let Expr::Index(target, key) = &name
                        && let Expr::Member(frame, member) = &**target
                        && let Expr::Name(frame) = &**frame
                        && let Expr::String(key) = &**key
                        && let Some(Type::Span(fields, children)) = env.names.get_mut(frame)
                    {
                        if member == "children" {
                            children.insert(key.clone(), kind);
                        } else if member == "fields" {
                            fields.insert(key.clone(), kind);
                        }
                    }
                    if matches!(&value, Expr::Name(n) if env.records.contains_key(n) || env.collections.contains_key(n)) {
                        continue;
                    }
                    output.push(Statement::Variable(Variable::Assign {
                        name: expression(name, env, lang).0,
                        value,
                    }));
                }
                Statement::Return(value) => {
                    let value = if matches!(&value,Expr::Name(name) if Some(name.as_str())==return_frame) {
                        let frame = return_frame.unwrap();
                        let fields = env.exports.get(frame).cloned().unwrap_or_default();
                        if fields.iter().any(|key| key != "output") {
                            Expr::Object(
                                fields
                                    .iter()
                                    .map(|key| {
                                        (
                                            export_name(key).into(),
                                            env.fields
                                                .get(&(frame.into(), key.clone()))
                                                .map(|(v, _)| v.clone())
                                                .unwrap_or(Expr::Null),
                                        )
                                    })
                                    .collect(),
                            )
                        } else {
                            env.fields
                                .get(&(frame.into(), "output".into()))
                                .map(|(v, _)| v.clone())
                                .unwrap_or(Expr::Null)
                        }
                    } else {
                        value
                    };
                    output.push(Statement::Return(expression(value, env, lang).0));
                }
                Statement::Evaluate(value) => output.push(Statement::Evaluate(expression(value, env, lang).0)),
                Statement::Raise(value) => output.push(Statement::Raise(expression(value, env, lang).0)),
                Statement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let condition = expression(condition, env, lang).0;
                    let mut yes = env.clone();
                    let mut no = env.clone();
                    let then_body = block(then_body, &mut yes, lang, return_frame);
                    let else_body = block(else_body, &mut no, lang, return_frame);
                    for (name, kind) in &mut env.names {
                        if let (Some(a), Some(b)) = (yes.names.get(name), no.names.get(name)) {
                            *kind = join(a.clone(), b.clone());
                        }
                    }
                    output.push(Statement::If {
                        condition,
                        then_body,
                        else_body,
                    });
                }
                Statement::ForRange { index, count, body } => output.push(Statement::ForRange {
                    index,
                    count: expression(count, env, lang).0,
                    body: block(body, &mut env.clone(), lang, return_frame),
                }),
                Statement::TraceScope { scope, body } => output.push(Statement::TraceScope {
                    scope,
                    body: block(body, env, lang, return_frame),
                }),
            }
        }
        code::rewrite(&mut output, &mut |value| {
            if let Expr::Call {
                function,
                args,
                keywords,
            } = value
                && args.is_empty()
                && keywords.is_empty()
                && let Expr::Name(name) = &**function
                && env.eager.contains(name)
            {
                *value = Expr::Name(name.clone());
            }
        });
        inline_local_reads(&mut output);
        env.types.borrow_mut().extend(env.names.clone());
        output
    }

    // A pure deferred read used entirely within one pure expression needs no
    // memoization: no intervening operation can change its value. Keep caches
    // across statements, callbacks, loops, or effects.
    fn inline_local_reads(body: &mut Vec<Statement>) {
        let candidates: HashMap<String, Expr> = body
            .iter()
            .filter_map(|s| {
                if let Statement::Variable(Variable::Declare {
                    name,
                    value: Expr::Call { function, args, .. },
                    ..
                }) = s
                    && matches!(&**function, Expr::Name(n) if n == "_lazy")
                    && let [Expr::Lambda { parameters, value }] = args.as_slice()
                    && parameters.is_empty()
                {
                    Some((name.clone(), (**value).clone()))
                } else {
                    None
                }
            })
            .collect();
        fn pure(value: &Expr, candidates: &HashMap<String, Expr>, seen: &mut HashSet<String>) -> bool {
            match value {
                Expr::Name(_) | Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Call {
                    function,
                    args,
                    keywords,
                } => {
                    let Expr::Name(name) = &**function else {
                        return false;
                    };
                    if let Some(value) = candidates.get(name) {
                        if !args.is_empty() || !keywords.is_empty() || !seen.insert(name.clone()) {
                            return false;
                        }
                        let result = pure(value, candidates, seen);
                        seen.remove(name);
                        return result;
                    }
                    [
                        "_num",
                        "_integer",
                        "_index",
                        "_equal",
                        "_boolean",
                        "_arithmetic",
                        "_len",
                        "_text",
                        "_trim",
                        "_float",
                        "_value",
                    ]
                    .contains(&name.as_str())
                        && args
                            .iter()
                            .chain(keywords.iter().map(|(_, v)| v))
                            .all(|v| pure(v, candidates, seen))
                }
                Expr::Member(value, _) | Expr::Unary { value, .. } | Expr::Read { value, .. } => pure(value, candidates, seen),
                Expr::Binary { left, right, .. } | Expr::Index(left, right) | Expr::Identity(left, right) => {
                    pure(left, candidates, seen) && pure(right, candidates, seen)
                }
                Expr::Conditional { condition, yes, no } => {
                    pure(condition, candidates, seen) && pure(yes, candidates, seen) && pure(no, candidates, seen)
                }
                Expr::Object(fields) => fields.iter().all(|(_, v)| pure(v, candidates, seen)),
                Expr::Array(items) => items.iter().all(|v| pure(v, candidates, seen)),
                _ => false,
            }
        }
        let mut removable = HashSet::new();
        let mut ordered: Vec<_> = candidates.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(b.0));
        for (name, initializer) in ordered {
            if !pure(initializer, &candidates, &mut HashSet::new()) {
                continue;
            }
            let uses: Vec<_> = body
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    !matches!(s, Statement::Variable(Variable::Declare { name: n, .. }) if n == name)
                        && code::references(std::slice::from_ref(*s)).contains(&Dependency::Variable(name.clone()))
                })
                .map(|(i, _)| i)
                .collect();
            let [index] = uses.as_slice() else {
                continue;
            };
            let value = match &mut body[*index] {
                Statement::Variable(Variable::Declare { value, .. }) | Statement::Return(value) => value,
                _ => continue,
            };
            if !pure(value, &candidates, &mut HashSet::new()) {
                continue;
            }
            code::rewrite_expr(value, &mut |value| {
                if matches!(value, Expr::Call { function, args, keywords } if args.is_empty() && keywords.is_empty() && matches!(&**function, Expr::Name(n) if n == name))
                {
                    *value = initializer.clone();
                }
            });
            removable.insert(name.clone());
        }
        body.retain(|s| !matches!(s, Statement::Variable(Variable::Declare { name, .. }) if removable.contains(name)));
    }

    fn branch_result(function: &mut code::Function, env: &Environment, lang: ScorerLang) -> Option<Branch> {
        let Statement::Variable(Variable::Declare {
            name: frame,
            value: Expr::New { class, args },
            ..
        }) = function.body.first()?
        else {
            return None;
        };
        if class != "Span"
            || !matches!(args.get(1), Some(Expr::String(kind)) if kind == "maybe" || kind == "choice" || kind == "repeat")
        {
            return None;
        }
        let frame = frame.clone();
        let span = env.names.get(&frame)?.clone();
        let Type::Span(fields, children) = &span else {
            return None;
        };
        let mut results = Vec::new();
        let mut child_fields = HashMap::new();
        let mut keys: Vec<_> = children.keys().collect();
        keys.sort();
        for key in keys {
            // Only direct business children have ordinary result bindings. Nested
            // structural selections keep their established representation.
            let result = label(env, key, "output", lang);
            let kind = match &children[key] {
                Type::Span(fields, _) => fields.get("output").cloned().unwrap_or(Type::Null),
                Type::Optional(child) => match &**child {
                    Type::Span(fields, _) => join(Type::Null, fields.get("output").cloned().unwrap_or(Type::Null)),
                    _ => return None,
                },
                _ => return None,
            };
            let field = key
                .strip_suffix("_span")
                .or_else(|| key.strip_suffix("Span"))
                .unwrap_or(key)
                .to_string();
            if fields.contains_key(&field) {
                return None;
            }
            results.push((result, field.clone()));
            child_fields.insert(key.clone(), (field, kind));
        }
        fn present(body: &[Statement], name: &str) -> bool {
            body.iter().any(|s| match s {
                Statement::Variable(Variable::Declare { name: n, .. }) => n == name,
                Statement::If {
                    then_body, else_body, ..
                } => present(then_body, name) || present(else_body, name),
                Statement::TraceScope { body, .. } => present(body, name),
                _ => false,
            })
        }
        if results.iter().any(|(name, _)| !present(&function.body, name)) {
            return None;
        }
        fn promote(body: &mut [Statement], names: &HashSet<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, value, .. }) if names.contains(name) => {
                        *statement = Statement::Variable(Variable::Assign {
                            name: name.clone().into(),
                            value: value.clone(),
                        });
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        promote(then_body, names);
                        promote(else_body, names);
                    }
                    Statement::TraceScope { body, .. } => promote(body, names),
                    _ => {}
                }
            }
        }
        let names = results.iter().map(|(name, _)| name.clone()).collect();
        promote(&mut function.body, &names);
        let mut returned: Vec<_> = fields
            .keys()
            .map(|key| (key.clone(), Expr::from(frame.clone()).member(key)))
            .collect();
        returned.sort_by(|a, b| a.0.cmp(&b.0));
        returned.extend(results.iter().map(|(name, field)| (field.clone(), Expr::from(name.clone()))));
        if let Some(Statement::TraceScope { body, .. }) = function.body.get_mut(1) {
            if !matches!(body.last(), Some(Statement::Return(Expr::Name(n))) if n == &frame) {
                return None;
            }
            *body.last_mut().unwrap() = Statement::Return(if fields.is_empty() && returned.len() == 1 {
                returned.remove(0).1
            } else {
                Expr::Object(returned)
            });
            for (name, _) in results.into_iter().rev() {
                body.insert(
                    0,
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Null,
                        mutable: true,
                        annotation: None,
                    }),
                );
            }
        } else {
            return None;
        }
        if let Statement::Variable(Variable::Declare {
            value: Expr::New { args, .. },
            ..
        }) = &mut function.body[0]
        {
            args.push(Expr::string(&frame));
        }
        if lang == ScorerLang::Typescript {
            function.returns = Some("any".into());
        }
        Some(Branch {
            span: span.clone(),
            fields: fields.clone(),
            children: child_fields,
        })
    }

    fn assign_value(name: &str, value: Expr) -> Vec<Statement> {
        match value {
            Expr::Conditional { condition, yes, no } => vec![Statement::If {
                condition: *condition,
                then_body: assign_value(name, *yes),
                else_body: assign_value(name, *no),
            }],
            value => vec![Statement::Variable(Variable::Assign {
                name: name.into(),
                value,
            })],
        }
    }
    // Move a cached initializer to its first guaranteed evaluation, without
    // crossing an earlier fallible operation or an unselected branch.
    fn materialize(value: &Expr, env: &mut Environment, output: &mut Vec<Statement>, lang: ScorerLang) {
        fn first(value: &Expr, env: &Environment) -> Result<Option<String>, ()> {
            if let Expr::Call { function, args, .. } = value
                && args.is_empty()
                && let Expr::Name(name) = &**function
                && env.pending.contains_key(name)
                && !env.captured.contains(name)
            {
                return Ok(Some(name.clone()));
            }
            let children: Vec<&Expr> = match value {
                Expr::Call { args, keywords, .. } => args.iter().chain(keywords.iter().map(|(_, v)| v)).collect(),
                Expr::Object(fields) => fields.iter().map(|(_, v)| v).collect(),
                Expr::Array(items) => items.iter().collect(),
                Expr::Binary {
                    left,
                    op: BinOp::And | BinOp::Or,
                    ..
                } => vec![left],
                Expr::Binary { left, right, .. } | Expr::Index(left, right) | Expr::Identity(left, right) => vec![left, right],
                Expr::Conditional { condition, .. } => vec![condition],
                Expr::Unary { value, .. } | Expr::Read { value, .. } => vec![value],
                _ => vec![],
            };
            for child in children {
                if let Some(name) = first(child, env)? {
                    return Ok(Some(name));
                }
            }
            if infallible(value, env) { Ok(None) } else { Err(()) }
        }
        while let Ok(Some(name)) = first(value, env) {
            let Some(index) = output
                .iter()
                .position(|s| matches!(s,Statement::Variable(Variable::Declare{name:n,..}) if n==&name))
            else {
                break;
            };
            let initializer = env.pending.remove(&name).unwrap();
            materialize(&initializer, env, output, lang);
            let (initializer, kind) = expression(initializer, env, lang);
            let index = output
                .iter()
                .position(|s| matches!(s, Statement::Variable(Variable::Declare { name: n, .. }) if n == &name))
                .unwrap_or(index);
            output.remove(index);
            output.push(Statement::Variable(Variable::constant(&name, initializer)));
            env.names.insert(name.clone(), kind);
            env.eager.insert(name);
        }
    }
    fn constants(value: Expr, env: &Environment) -> Expr {
        match value {
            Expr::Call {
                ref function, ref args, ..
            } if matches!(&**function,Expr::Name(n) if n=="_lazy") => {
                if let Some(Expr::Lambda { value: inner, .. }) = args.first()
                    && infallible(inner, env)
                {
                    return constants((**inner).clone(), env);
                }
                value
            }
            Expr::Array(items) => Expr::Array(items.into_iter().map(|v| constants(v, env)).collect()),
            Expr::Object(fields) => Expr::Object(fields.into_iter().map(|(k, v)| (k, constants(v, env))).collect()),
            value => value,
        }
    }
    fn materialized(kind: &Type) -> bool {
        match kind {
            Type::Unknown | Type::Deferred(_) | Type::Span(..) | Type::Record(_) | Type::Product(_) => false,
            Type::List(kind) => materialized(kind),
            Type::Optional(kind) => materialized(kind),
            Type::Array(items) => items.iter().all(materialized),
            Type::Object(fields) => fields.values().all(materialized),
            _ => true,
        }
    }
    fn infallible(value: &Expr, env: &Environment) -> bool {
        match value {
            Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
            Expr::Name(name) => env.names.contains_key(name),
            Expr::Array(items) => items.iter().all(|v| infallible(v, env)),
            Expr::Object(fields) => fields.iter().all(|(_, v)| infallible(v, env)),
            Expr::Binary {
                left,
                op: BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or,
                right,
            } => infallible(left, env) && infallible(right, env),
            Expr::Unary { op: UnaryOp::Not, value } => infallible(value, env),
            _ => false,
        }
    }
    fn expression(value: Expr, env: &Environment, lang: ScorerLang) -> (Expr, Type) {
        let unknown = Type::Unknown;
        match value {
            Expr::Name(name) => {
                let t = env.names.get(&name).cloned().unwrap_or(unknown);
                (Expr::Name(name), t)
            }
            Expr::String(v) => (Expr::String(v), Type::String),
            Expr::Bool(v) => (Expr::Bool(v), Type::Bool),
            Expr::Null => (Expr::Null, Type::Null),
            Expr::Number(v) => {
                let t = v.parse::<i128>().map(|v| Type::Integer(v, v)).unwrap_or(unknown);
                (Expr::Number(v), t)
            }
            Expr::Read { dependency, value } => {
                if let Dependency::Child(_, child) = &dependency
                    && let Some((value, shape)) = env.collections.get(child)
                {
                    return (value.clone(), Type::List(Box::new(Type::Record(Box::new(shape.clone())))));
                }
                if let Dependency::Field(frame, key) = &dependency {
                    if let Some(record) = env.records.get(frame)
                        && let Some(kind) = record.shape.fields.get(key)
                    {
                        return (record.value.clone().index(Expr::string(key)), kind.clone());
                    }
                    if let Some(v) = env.fields.get(&(frame.clone(), key.clone())) {
                        return v.clone();
                    }
                    if key == "output"
                        && let Some(v) = env.outputs.get(frame)
                    {
                        return v.clone();
                    }
                }
                expression(*value, env, lang)
            }
            Expr::Call {
                function,
                args,
                keywords,
            } => {
                let callee = if let Expr::Name(name) = &*function {
                    Some(name.clone())
                } else {
                    None
                };
                fn unwrapped(value: &Expr) -> &Expr {
                    if let Expr::Read { value, .. } = value {
                        unwrapped(value)
                    } else {
                        value
                    }
                }
                if callee.as_deref() == Some("_at")
                    && args.len() == 2
                    && let Expr::Call {
                        function,
                        args: children,
                        ..
                    } = unwrapped(&args[0])
                    && matches!(&**function, Expr::Name(n) if n == "_child")
                    && let Some(Expr::String(key)) = children.get(1)
                    && let Some(record) = env.records.get(key)
                    && let Expr::Lambda { parameters, value } = &args[1]
                    && let Some(parameter) = parameters.first()
                {
                    if let Expr::Member(target, field) = unwrapped(value)
                        && matches!(&**target, Expr::Name(n) if n == &parameter.name)
                        && let Some(kind) = record.shape.fields.get(field)
                    {
                        return (record.value.clone().index(Expr::string(field)), kind.clone());
                    }
                    if let Expr::Call {
                        function,
                        args: selection,
                        ..
                    } = unwrapped(value)
                        && matches!(&**function, Expr::Name(n) if n == "_at")
                        && selection.len() == 2
                        && let Expr::Call {
                            function, args: child, ..
                        } = unwrapped(&selection[0])
                        && matches!(&**function, Expr::Name(n) if n == "_child")
                        && matches!(child.first(), Some(Expr::Name(n)) if n == &parameter.name)
                        && let Some(Expr::String(key)) = child.get(1)
                        && let Some((field, kind)) = record.shape.children.get(key)
                        && let Expr::Lambda { parameters, value } = &selection[1]
                        && let Some(parameter) = parameters.first()
                        && matches!(unwrapped(value), Expr::Member(target, field) if field == "output" && matches!(&**target, Expr::Name(n) if n == &parameter.name))
                    {
                        return (record.value.clone().index(Expr::string(field)), kind.clone());
                    }
                }
                if callee.as_deref() == Some("_at")
                    && args.len() == 2
                    && let Expr::Call {
                        function,
                        args: children,
                        ..
                    } = unwrapped(&args[0])
                    && let Expr::Lambda { parameters, value } = &args[1]
                    && matches!(&**function, Expr::Name(n) if n == "_child")
                    && let Some(Expr::String(key)) = children.get(1)
                    && let Some(parameter) = parameters.first()
                    && let Expr::Member(target, field) = unwrapped(value)
                    && matches!(&**target, Expr::Name(n) if n == &parameter.name)
                {
                    if let Some(result) = env.fields.get(&(key.clone(), field.clone())) {
                        return result.clone();
                    }
                    if field == "output"
                        && let Some(result) = env.outputs.get(key)
                    {
                        return result.clone();
                    }
                }
                let evaluated = args.into_iter().map(|arg| expression(arg, env, lang)).collect::<Vec<_>>();
                let kinds = evaluated.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>();
                let args = evaluated.into_iter().map(|(v, _)| v).collect::<Vec<_>>();
                let keywords = keywords.into_iter().map(|(k, v)| (k, expression(v, env, lang).0)).collect();
                let function = expression(*function, env, lang).0;
                let call = Expr::Call {
                    function: Box::new(function),
                    args: args.clone(),
                    keywords,
                };
                if let Some(name) = callee {
                    if env.eager.contains(&name) && args.is_empty() {
                        return (name.clone().into(), env.names.get(&name).cloned().unwrap_or(unknown));
                    }
                    if args.is_empty()
                        && let Some(Type::Deferred(kind)) = env.names.get(&name)
                    {
                        return (call, (**kind).clone());
                    }
                    match (name.as_str(), args.as_slice(), kinds.as_slice()) {
                        ("_lazy", [Expr::Lambda { value, .. }], _) => {
                            return (call, Type::Deferred(Box::new(expression((**value).clone(), env, lang).1)));
                        }
                        ("_value", [value], [kind]) if materialized(kind) => return (value.clone(), kind.clone()),
                        ("_value", [value], _) => {
                            let value = constants(value.clone(), env);
                            let (value, kind) = expression(value, env, lang);
                            if materialized(&kind) {
                                return (value, kind);
                            }
                            return (Expr::call("_value", vec![value]), normalized(&kind));
                        }
                        ("_boolean", [value], [Type::Bool]) => return (value.clone(), Type::Bool),
                        ("_boolean", _, _) | ("_equal", _, _) | ("_include", _, _) => {
                            if name == "_equal" && args.len() == 2 && compatible(&kinds[0], &kinds[1]) {
                                return (args[0].clone().binary(BinOp::Eq, args[1].clone()), Type::Bool);
                            }
                            return (call, Type::Bool);
                        }
                        ("_num", [value], [kind @ (Type::Integer(..) | Type::Number)]) => return (value.clone(), kind.clone()),
                        ("_integer", [value], [kind @ Type::Integer(min, max)])
                            if *min >= -9_007_199_254_740_991 && *max <= 9_007_199_254_740_991 =>
                        {
                            return (value.clone(), kind.clone());
                        }
                        ("_count", [value], [kind @ Type::Integer(min, max)]) if *min >= 0 && *max <= 9_007_199_254_740_991 => {
                            return (value.clone(), kind.clone());
                        }
                        ("_text", [value], [Type::String]) => return (value.clone(), Type::String),
                        ("_text", [value], [Type::Integer(min, max)])
                            if *min >= -9_007_199_254_740_991 && *max <= 9_007_199_254_740_991 =>
                        {
                            return (
                                Expr::call(if lang == ScorerLang::Python { "str" } else { "String" }, vec![value.clone()]),
                                Type::String,
                            );
                        }
                        ("_index", [value, Expr::String(key)], [Type::Optional(kind), _]) => {
                            if let Type::Object(fields) = &**kind
                                && fields.get(key).is_some_and(materialized)
                            {
                                return (value.clone().index(Expr::string(key)), fields[key].clone());
                            }
                        }
                        ("_index", [value, Expr::String(key)], [Type::Object(fields), _])
                            if fields.get(key).is_some_and(materialized) =>
                        {
                            return (value.clone().index(Expr::string(key)), fields[key].clone());
                        }
                        ("_index", [value, Expr::Number(index)], [Type::Array(items), _]) => {
                            if let Ok(i) = index.parse::<usize>()
                                && let Some(kind) = items.get(i)
                                && materialized(kind)
                            {
                                return (value.clone().index(Expr::number(i)), kind.clone());
                            }
                        }
                        ("_len", [value], [Type::String]) => {
                            return (
                                if lang == ScorerLang::Python {
                                    Expr::call("len", vec![value.clone()])
                                } else {
                                    Expr::call(Expr::from("Array").member("from"), vec![value.clone()]).member("length")
                                },
                                Type::Integer(0, 9_007_199_254_740_991),
                            );
                        }
                        ("_len", [value], [Type::List(_)]) => {
                            return (
                                if lang == ScorerLang::Python {
                                    Expr::call("len", vec![value.clone()])
                                } else {
                                    value.clone().member("length")
                                },
                                Type::Integer(0, 9_007_199_254_740_991),
                            );
                        }
                        ("_len", [value], [Type::Array(items)]) if items.iter().all(materialized) => {
                            return (
                                if lang == ScorerLang::Python {
                                    Expr::call("len", vec![value.clone()])
                                } else {
                                    value.clone().member("length")
                                },
                                Type::Integer(items.len() as i128, items.len() as i128),
                            );
                        }
                        ("_upper" | "_lower", [value], [Type::String]) => {
                            return (
                                Expr::call(
                                    value.clone().member(match (name.as_str(), lang) {
                                        ("_upper", ScorerLang::Python) => "upper",
                                        ("_upper", _) => "toUpperCase",
                                        (_, ScorerLang::Python) => "lower",
                                        _ => "toLowerCase",
                                    }),
                                    vec![],
                                ),
                                Type::String,
                            );
                        }
                        ("_arithmetic", [Expr::String(op), left, right], [_, a, b])
                            if matches!(a, Type::Number | Type::Integer(..))
                                && matches!(b, Type::Number | Type::Integer(..))
                                && (matches!(a, Type::Number) || matches!(b, Type::Number)) =>
                        {
                            let value = match op.as_str() {
                                "+" | "-" | "*" => Expr::call(
                                    "checked_number",
                                    vec![left.clone().binary(
                                        match op.as_str() {
                                            "+" => BinOp::Add,
                                            "-" => BinOp::Sub,
                                            _ => BinOp::Mul,
                                        },
                                        right.clone(),
                                    )],
                                ),
                                "%" => Expr::call("remainder", vec![left.clone(), right.clone()]),
                                _ => Expr::call("divide", vec![left.clone(), right.clone()]),
                            };
                            return (value, Type::Number);
                        }
                        ("_arithmetic", [Expr::String(op), left, right], [_, Type::Integer(a, b), Type::Integer(c, d)]) => {
                            let operator = match op.as_str() {
                                "+" => BinOp::Add,
                                "-" => BinOp::Sub,
                                "*" => BinOp::Mul,
                                "%" => BinOp::Rem,
                                _ => BinOp::Div,
                            };
                            let bounds = bounds(operator, *a, *b, *c, *d);
                            if let Some((min, max)) = bounds
                                && min >= -9_007_199_254_740_991
                                && max <= 9_007_199_254_740_991
                                && *a >= -9_007_199_254_740_991
                                && *b <= 9_007_199_254_740_991
                                && *c >= -9_007_199_254_740_991
                                && *d <= 9_007_199_254_740_991
                            {
                                return (
                                    left.clone().binary(
                                        match op.as_str() {
                                            "+" => BinOp::Add,
                                            "-" => BinOp::Sub,
                                            "*" => BinOp::Mul,
                                            _ => BinOp::Rem,
                                        },
                                        right.clone(),
                                    ),
                                    Type::Integer(min, max),
                                );
                            }
                        }
                        ("_iteration", [value, index], [Type::List(kind), _]) => {
                            return (Expr::call("_index", vec![value.clone(), index.clone()]), (**kind).clone());
                        }
                        ("_index", _, [Type::List(kind), _]) => return (call, (**kind).clone()),
                        ("_slice", [value, start, end], [kind @ Type::List(_), _, _])
                            if matches!(start, Expr::Null) && matches!(end, Expr::Null) =>
                        {
                            return (value.clone(), kind.clone());
                        }
                        ("_slice", _, [kind @ Type::List(_), ..]) => return (call, kind.clone()),
                        ("_child", [scope, Expr::String(key)], [Type::Record(branch), _]) => {
                            if let Some((field, kind)) = branch.children.get(key) {
                                return (
                                    if branch.fields.is_empty() && branch.children.len() == 1 {
                                        scope.clone()
                                    } else {
                                        scope.clone().index(Expr::string(field))
                                    },
                                    Type::Product(Box::new(kind.clone())),
                                );
                            }
                        }
                        ("_child", [scope, Expr::String(key)], [Type::Span(_, children), _]) => {
                            if let Some(kind) = children.get(key) {
                                let children = scope.clone().member("children");
                                let value = if matches!(kind, Type::Optional(_)) {
                                    if lang == ScorerLang::Python {
                                        Expr::call(children.member("get"), vec![Expr::string(key)])
                                    } else {
                                        Expr::Conditional {
                                            condition: Box::new(
                                                children
                                                    .clone()
                                                    .index(Expr::string(key))
                                                    .binary(BinOp::Eq, Expr::from("undefined")),
                                            ),
                                            yes: Box::new(Expr::Null),
                                            no: Box::new(children.index(Expr::string(key))),
                                        }
                                    }
                                } else {
                                    children.index(Expr::string(key))
                                };
                                return (value, kind.clone());
                            }
                        }
                        ("_at", [scope, Expr::Lambda { parameters, value }], [kind, _]) => {
                            if let Expr::Call { function, args, .. } = scope
                                && matches!(&**function, Expr::Name(n) if n == "_child")
                                && let Some(Expr::String(key)) = args.get(1)
                                && let Some(result) = env.outputs.get(key)
                                && let Some(parameter) = parameters.first()
                                && matches!(&**value, Expr::Member(target, field) if field == "output" && matches!(&**target, Expr::Name(n) if n == &parameter.name))
                            {
                                return result.clone();
                            }
                            let (span, optional) = match kind {
                                Type::Span(..) | Type::List(_) | Type::Record(_) | Type::Product(_) => (kind.clone(), false),
                                Type::Optional(inner) if matches!(&**inner, Type::Span(..)) => ((**inner).clone(), true),
                                _ => (Type::Unknown, false),
                            };
                            if !matches!(span, Type::Unknown)
                                && let Some(parameter) = parameters.first()
                            {
                                let mut local = env.clone();
                                local.names.insert(parameter.name.clone(), span);
                                let (mut result, kind) = expression((**value).clone(), &local, lang);
                                code::rewrite_expr(&mut result, &mut |value| {
                                    if matches!(value, Expr::Name(name) if name == &parameter.name) {
                                        *value = scope.clone();
                                    }
                                });
                                if optional {
                                    return (
                                        Expr::Conditional {
                                            condition: Box::new(Expr::Identity(Box::new(scope.clone()), Box::new(Expr::Null))),
                                            yes: Box::new(Expr::Null),
                                            no: Box::new(result),
                                        },
                                        join(Type::Null, kind),
                                    );
                                }
                                return (result, kind);
                            }
                        }
                        _ => {}
                    }
                    let kind = match name.as_str() {
                        "_text" | "_trim" | "_upper" | "_lower" | "_replace" | "_join" | "_random_text" => Type::String,
                        "_len" | "_count" | "_pick" => Type::Integer(0, 9_007_199_254_740_991),
                        "_integer" => Type::Integer(-9_007_199_254_740_991, 9_007_199_254_740_991),
                        "_value" => kinds.first().map(normalized).unwrap_or(Type::Value),
                        "_index" if kinds.first().is_some_and(materialized) => Type::Value,
                        "_at" => Type::Value,
                        "_duration" => Type::Value,
                        "checked_number" | "remainder" | "divide" => Type::Number,
                        _ => env.returns.get(&name).cloned().unwrap_or(Type::Unknown),
                    };
                    (call, kind)
                } else {
                    let kind = if matches!(&call, Expr::Call {function,..} if matches!(&**function, Expr::Member(_,method) if ["upper","lower","toUpperCase","toLowerCase"].contains(&method.as_str())))
                    {
                        Type::String
                    } else {
                        unknown
                    };
                    (call, kind)
                }
            }
            Expr::Array(items) => {
                let items = items.into_iter().map(|v| expression(v, env, lang)).collect::<Vec<_>>();
                (
                    Expr::Array(items.iter().map(|(v, _)| v.clone()).collect()),
                    Type::Array(items.into_iter().map(|(_, t)| t).collect()),
                )
            }
            Expr::Object(fields) => {
                let fields = fields
                    .into_iter()
                    .map(|(k, v)| (k, expression(v, env, lang)))
                    .collect::<Vec<_>>();
                (
                    Expr::Object(fields.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect()),
                    Type::Object(fields.into_iter().map(|(k, (_, t))| (k, t)).collect()),
                )
            }
            Expr::Binary { left, op, right } => {
                let (left, a) = expression(*left, env, lang);
                let (right, b) = expression(*right, env, lang);
                let t = match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or => {
                        Type::Bool
                    }
                    BinOp::Add if matches!((&a, &b), (Type::String, Type::String)) => Type::String,
                    _ => {
                        if let (Type::Integer(a, b), Type::Integer(c, d)) = (a, b) {
                            bounds(op, a, b, c, d).map(|(a, b)| Type::Integer(a, b)).unwrap_or(unknown)
                        } else {
                            unknown
                        }
                    }
                };
                (left.binary(op, right), t)
            }
            Expr::Unary { op, value } => {
                let (value, t) = expression(*value, env, lang);
                (
                    Expr::Unary {
                        op,
                        value: Box::new(value),
                    },
                    if op == UnaryOp::Not { Type::Bool } else { t },
                )
            }
            Expr::Conditional { condition, yes, no } => {
                let (condition, _) = expression(*condition, env, lang);
                let (yes, a) = expression(*yes, env, lang);
                let (no, b) = expression(*no, env, lang);
                (
                    Expr::Conditional {
                        condition: Box::new(condition),
                        yes: Box::new(yes),
                        no: Box::new(no),
                    },
                    join(a, b),
                )
            }
            Expr::Required(value) => {
                let (value, kind) = expression(*value, env, lang);
                let kind = if let Type::Optional(inner) = kind { *inner } else { kind };
                (Expr::Required(Box::new(value)), kind)
            }
            Expr::Member(value, key) => {
                let (value, t) = expression(*value, env, lang);
                if key == "iterations" && matches!(t, Type::List(_)) {
                    return (value, t);
                }
                if key == "output"
                    && let Type::Product(kind) = &t
                {
                    return (value, (**kind).clone());
                }
                let t = match t {
                    Type::Object(fields) | Type::Span(fields, _) => fields.get(&key).cloned().unwrap_or(unknown),
                    _ => unknown,
                };
                (value.member(key), t)
            }
            Expr::Index(value, index) => {
                let (value, t) = expression(*value, env, lang);
                let (index, _) = expression(*index, env, lang);
                let t = match (&t, &index) {
                    (Type::Object(fields), Expr::String(key)) => fields.get(key).cloned().unwrap_or(unknown),
                    _ => unknown,
                };
                (value.index(index), t)
            }
            Expr::Lambda { parameters, value } => {
                let mut local = env.clone();
                for p in &parameters {
                    local.names.remove(&p.name);
                    local.eager.remove(&p.name);
                }
                let (value, _) = expression(*value, &local, lang);
                (
                    Expr::Lambda {
                        parameters,
                        value: Box::new(value),
                    },
                    unknown,
                )
            }
            Expr::Map { item, iterable, value } => {
                let (iterable, kind) = expression(*iterable, env, lang);
                let mut local = env.clone();
                if let Type::List(kind) = kind {
                    local.names.insert(item.clone(), *kind);
                }
                let (value, kind) = expression(*value, &local, lang);
                if matches!(&value, Expr::Name(name) if name == &item) {
                    return (iterable, Type::List(Box::new(kind)));
                }
                (
                    Expr::Map {
                        item,
                        iterable: Box::new(iterable),
                        value: Box::new(value),
                    },
                    unknown,
                )
            }
            Expr::Slice { value, start, end } => (
                Expr::Slice {
                    value: Box::new(expression(*value, env, lang).0),
                    start: start.map(|v| Box::new(expression(*v, env, lang).0)),
                    end: end.map(|v| Box::new(expression(*v, env, lang).0)),
                },
                unknown,
            ),
            Expr::Identity(a, b) => (
                Expr::Identity(Box::new(expression(*a, env, lang).0), Box::new(expression(*b, env, lang).0)),
                Type::Bool,
            ),
            Expr::Contains(a, b) => (
                Expr::Contains(Box::new(expression(*a, env, lang).0), Box::new(expression(*b, env, lang).0)),
                Type::Bool,
            ),
            Expr::Spread(value) => (Expr::Spread(Box::new(expression(*value, env, lang).0)), unknown),
            Expr::Template(items) => {
                let items: Vec<_> = items.into_iter().map(|v| expression(v, env, lang).0).collect();
                if items.iter().all(|v| matches!(v, Expr::String(_))) {
                    return (
                        Expr::string(
                            items
                                .into_iter()
                                .map(|v| if let Expr::String(s) = v { s } else { unreachable!() })
                                .collect::<String>(),
                        ),
                        Type::String,
                    );
                }
                (Expr::Template(items), Type::String)
            }
            Expr::New { class, args } => {
                let kind = if class == "Span" {
                    Type::Span(HashMap::new(), HashMap::new())
                } else {
                    unknown
                };
                (
                    Expr::New {
                        class,
                        args: args.into_iter().map(|v| expression(v, env, lang).0).collect(),
                    },
                    kind,
                )
            }
        }
    }
    fn bounds(op: BinOp, a: i128, b: i128, c: i128, d: i128) -> Option<(i128, i128)> {
        match op {
            BinOp::Add => a.checked_add(c).zip(b.checked_add(d)),
            BinOp::Sub => a.checked_sub(d).zip(b.checked_sub(c)),
            BinOp::Mul => {
                let values = [a.checked_mul(c)?, a.checked_mul(d)?, b.checked_mul(c)?, b.checked_mul(d)?];
                Some((*values.iter().min()?, *values.iter().max()?))
            }
            BinOp::Rem if a >= 0 && c > 0 => Some((0, d - 1)),
            _ => None,
        }
    }
    fn compatible(a: &Type, b: &Type) -> bool {
        if let Type::Optional(inner) = a {
            return compatible(inner, b) && compatible(&Type::Null, b);
        }
        if let Type::Optional(inner) = b {
            return compatible(a, inner) && compatible(a, &Type::Null);
        }

        matches!(
            (a, b),
            (Type::Bool, Type::Bool)
                | (Type::String, Type::String)
                | (Type::Integer(..), Type::Integer(..))
                | (Type::Number, Type::Number | Type::Integer(..))
                | (Type::Integer(..), Type::Number)
                | (Type::Null, Type::Null)
        ) || matches!(a, Type::Null) && matches!(b, Type::Bool | Type::String | Type::Integer(..) | Type::Number)
            || matches!(b, Type::Null) && matches!(a, Type::Bool | Type::String | Type::Integer(..) | Type::Number)
    }
    fn normalized(t: &Type) -> Type {
        match t {
            Type::Unknown => Type::Value,
            Type::Optional(t) => Type::Optional(Box::new(normalized(t))),
            Type::Deferred(t) => normalized(t),
            Type::Array(v) => Type::Array(v.iter().map(normalized).collect()),
            Type::Object(v) => Type::Object(v.iter().map(|(k, v)| (k.clone(), normalized(v))).collect()),
            t => t.clone(),
        }
    }
    fn join(a: Type, b: Type) -> Type {
        match (a, b) {
            (Type::Span(a, ac), Type::Span(b, bc)) => {
                fn merge(mut a: HashMap<String, Type>, mut b: HashMap<String, Type>) -> HashMap<String, Type> {
                    let keys: HashSet<_> = a.keys().chain(b.keys()).cloned().collect();
                    keys.into_iter()
                        .map(|key| {
                            let kind = join(a.remove(&key).unwrap_or(Type::Null), b.remove(&key).unwrap_or(Type::Null));
                            (key, kind)
                        })
                        .collect()
                }
                Type::Span(merge(a, b), merge(ac, bc))
            }
            (Type::Null, b @ Type::Optional(_)) | (b @ Type::Optional(_), Type::Null) => b,
            (Type::Null, b) | (b, Type::Null) if !matches!(b, Type::Null) => Type::Optional(Box::new(b)),
            (Type::Optional(a), Type::Optional(b)) => Type::Optional(Box::new(join(*a, *b))),
            (Type::Optional(a), b) | (b, Type::Optional(a)) => Type::Optional(Box::new(join(*a, b))),
            (Type::Number, Type::Number | Type::Integer(..)) | (Type::Integer(..), Type::Number) => Type::Number,
            (Type::Bool, Type::Bool) => Type::Bool,
            (Type::String, Type::String) => Type::String,
            (Type::Null, Type::Null) => Type::Null,
            (Type::Integer(a, b), Type::Integer(c, d)) => Type::Integer(a.min(c), b.max(d)),
            (Type::Object(a), Type::Object(b)) => Type::Object(
                a.into_iter()
                    .filter_map(|(k, v)| b.get(&k).map(|w| (k, join(v, w.clone()))))
                    .collect(),
            ),
            (a, b) if materialized(&a) && materialized(&b) => Type::Value,
            _ => Type::Unknown,
        }
    }
}

mod expressions {
    use super::*;
    use crate::dsl::Range;
    pub(crate) fn emit(func: &Func, ctx: &Context<'_>, lang: ScorerLang) -> Result<Expr, String> {
        let expr = |v: &Value| expression(v, ctx, lang);
        let call = |name: &str, values: &[&Value]| -> Result<Expr, String> {
            Ok(Expr::call(name, values.iter().map(|v| expr(v)).collect::<Result<_, _>>()?))
        };
        let random = || Expr::from(random_name(lang));
        let number = |v: f64| Expr::Number(format!("{v:?}"));
        let sample = |name: &str, values: Vec<f64>| {
            Expr::call(
                "_sample",
                vec![
                    Expr::string(name),
                    Expr::Array(values.into_iter().map(number).collect()),
                    random(),
                ],
            )
        };
        Ok(match func {
            Func::Len { target } => call("_len", &[target])?,
            Func::Upper { text } => call("_upper", &[text])?,
            Func::Lower { text } => call("_lower", &[text])?,
            Func::Trim { text } => call("_trim", &[text])?,
            Func::Replace { text, from, to } => call("_replace", &[text, from, to])?,
            Func::Split { text, separator } => call("_split", &[text, separator])?,
            Func::Join { array, separator } => call("_join", &[array, separator])?,
            Func::Contains { target, needle } => call("_contains", &[target, needle])?,
            Func::StartsWith { text, prefix } => call("_starts_with", &[text, prefix])?,
            Func::EndsWith { text, suffix } => call("_ends_with", &[text, suffix])?,
            Func::Format { pieces, args } => {
                let mut parts = vec![Expr::string(&pieces[0])];
                for (value, piece) in args.iter().zip(&pieces[1..]) {
                    parts.push(Expr::call("_text", vec![expr(value)?]));
                    parts.push(Expr::string(piece));
                }
                parts.into_iter().reduce(|a, b| a.binary(BinOp::Add, b)).unwrap()
            }
            Func::Clamp { value, min, max } => call("_clamp", &[value, min, max])?,
            Func::Round { value } => call("_round", &[value])?,
            Func::Floor { value } => call("_floor", &[value])?,
            Func::Ceil { value } => call("_ceil", &[value])?,
            Func::Abs { value } => call("_abs", &[value])?,
            Func::Min(values) | Func::Max(values) => Expr::call(
                "_extreme",
                vec![
                    Expr::Array(values.iter().map(expr).collect::<Result<_, _>>()?),
                    Expr::number(if matches!(func, Func::Min(_)) { 0 } else { 1 }),
                ],
            ),
            Func::Choice(options) => Expr::call(
                "_select",
                vec![
                    Expr::call("_pick", vec![Expr::number(options.len()), random()]),
                    Expr::Array(options.iter().map(|v| expr(v).map(Expr::lazy)).collect::<Result<_, _>>()?),
                ],
            ),
            Func::Weighted(options) => Expr::call(
                "_weighted",
                vec![
                    Expr::Array(options.iter().map(|v| number(v.weight)).collect()),
                    Expr::Array(
                        options
                            .iter()
                            .map(|v| expr(&v.value).map(Expr::lazy))
                            .collect::<Result<_, _>>()?,
                    ),
                    random(),
                ],
            ),
            Func::Range(range) => {
                let (min, max, integer) = match range {
                    Range::Int { min, max } => (Expr::number(min), Expr::number(max), 1),
                    Range::Float { min, max } => (number(*min), number(*max), 0),
                };
                Expr::call("_range", vec![min, max, Expr::number(integer), random()])
            }
            Func::Chance { probability } => Expr::call("_include", vec![number(*probability), random()]),
            Func::Normal { mean, stddev } => sample("normal", vec![*mean, *stddev]),
            Func::Lognormal { median, sigma } => sample("lognormal", vec![*median, *sigma]),
            Func::Exponential { mean } => sample("exponential", vec![*mean]),
            Func::Pareto { min, shape } => sample("pareto", vec![*min, *shape]),
            Func::Beta { alpha, beta } => sample("beta", vec![*alpha, *beta]),
            Func::Poisson { mean } => sample("poisson", vec![*mean]),
            Func::Uuid => Expr::call("_uuid", vec![random()]),
            Func::Hex { length } | Func::Alphanum { length } => Expr::call(
                "_random_text",
                vec![
                    Expr::string(if matches!(func, Func::Hex { .. }) {
                        "0123456789abcdef"
                    } else {
                        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
                    }),
                    Expr::number(length),
                    random(),
                ],
            ),
            Func::Noise { size } => Expr::call("_noise", vec![expr(size)?, random()]),
            Func::Tokens { value } => call("_tokens", &[value])?,
            Func::Attachment { .. } => return Err("attachment packaging is not supported by agent generation".into()),
        })
    }
}

pub(crate) mod scoring {
    use crate::dsl::{Number, ScorerArg, ScorerLang, ScorerStep, Value};
    use std::fmt;

    use super::code::{Expr, Statement, Variable};
    use crate::dsl::{ArrayElem, BinOp, CtxRef, Func, Part, Range};
    use std::collections::BTreeSet;
    pub(in crate::mocking::code) fn function(
        item: &Item<'_>,
        body: Vec<Statement>,
        fallback: Expr,
        lang: ScorerLang,
    ) -> super::code::Function {
        use super::code::{Function, Parameter};
        let parameters = if lang == ScorerLang::Python {
            vec![
                Parameter::new("output"),
                Parameter::new("input").with_default(Expr::Null),
                Parameter::new("expected").with_default(Expr::Null),
                Parameter::new("metadata").with_default(Expr::Null),
            ]
        } else {
            vec![
                Parameter::object(&["input", "output", "expected", "metadata"])
                    .typed("{ input?: any; output: any; expected?: any; metadata?: any }"),
            ]
        };
        let mut function = Function::new(fn_name(item.slug), parameters);
        if lang == ScorerLang::Typescript {
            function.returns = Some("{ name: string; score: number; metadata: Record<string, unknown> }".into());
        }
        function.body = body;
        function.body.push(Statement::Return(result(item.name, fallback)));
        function
    }
    fn result(name: &str, score: Expr) -> Expr {
        Expr::Object(vec![
            ("name".into(), Expr::string(name)),
            ("score".into(), score),
            ("metadata".into(), Expr::Object(vec![])),
        ])
    }
    pub(in crate::mocking::code) struct Compiler {
        pub lang: ScorerLang,
        pub imports: BTreeSet<&'static str>,
        pub helpers: BTreeSet<Helper>,
        pub next_loop: usize,
        loops: Vec<(String, String)>,
    }
    impl Compiler {
        pub fn new(lang: ScorerLang) -> Self {
            Self {
                lang,
                imports: BTreeSet::new(),
                helpers: BTreeSet::new(),
                next_loop: 0,
                loops: vec![],
            }
        }
        fn need(&mut self, helper: Helper) {
            match helper {
                Helper::Lognormal | Helper::Gamma => self.need(Helper::Normal),
                Helper::Beta => self.need(Helper::Gamma),
                _ => {}
            }
            self.helpers.insert(helper);
        }
        pub fn steps(&mut self, steps: &[ScorerStep], name: &str) -> Result<Vec<Statement>, Error> {
            let mut body = Vec::new();
            for step in steps {
                match step {
                    ScorerStep::When(when) => {
                        let condition = self.lower(&when.cond)?;
                        let mut then_body = self.steps(&when.steps, name)?;
                        if let Some(score) = &when.score {
                            then_body.push(Statement::Return(result(name, self.lower(score)?)));
                        }
                        body.push(Statement::If {
                            condition,
                            then_body,
                            else_body: vec![],
                        });
                    }
                    ScorerStep::Repeat { count, steps } => {
                        let value = self.lower(count)?;
                        let id = self.next_loop;
                        self.next_loop += 1;
                        let python = self.lang == ScorerLang::Python;
                        let suffix = if id == 0 {
                            String::new()
                        } else if python {
                            format!("_{id}")
                        } else {
                            id.to_string()
                        };
                        let count = format!("{}{suffix}", if python { "repeat_count" } else { "repeatCount" });
                        let index = format!("{}{suffix}", if python { "repeat_index" } else { "repeatIndex" });
                        body.push(Statement::Variable(Variable::constant(&count, value)));
                        let invalid = if python {
                            Expr::call("isinstance", vec![count.clone().into(), "bool".into()]).binary(
                                BinOp::Or,
                                Expr::call("isinstance", vec![count.clone().into(), "int".into()]).not(),
                            )
                        } else {
                            Expr::call(Expr::from("Number").member("isSafeInteger"), vec![count.clone().into()]).not()
                        }
                        .binary(BinOp::Or, Expr::from(count.clone()).binary(BinOp::Lt, Expr::number(0)));
                        body.push(Statement::If {
                            condition: invalid,
                            then_body: vec![Statement::Raise(Expr::New {
                                class: if python { "ValueError" } else { "Error" }.into(),
                                args: vec![Expr::string("repeat count must be a non-negative integer")],
                            })],
                            else_body: vec![],
                        });
                        self.loops.push((index.clone(), count.clone()));
                        let nested = self.steps(steps, name)?;
                        self.loops.pop();
                        body.push(Statement::ForRange {
                            index,
                            count: count.into(),
                            body: nested,
                        });
                    }
                }
            }
            Ok(body)
        }
        #[cfg(test)]
        pub fn expr(&mut self, value: &Value, _context: u8) -> Result<String, Error> {
            Ok(crate::mocking::code::languages::rendering::expression(
                &self.lower(value)?,
                self.lang,
            ))
        }
        pub fn lower(&mut self, value: &Value) -> Result<Expr, Error> {
            Ok(match value {
                Value::Str(text) => Expr::string(text),
                Value::Num(number) => Expr::Number(num(number)),
                Value::Bool(value) => Expr::Bool(*value),
                Value::Null => Expr::Null,
                Value::ArgRef(arg) => Expr::from(arg_name(*arg)),
                Value::Template(template) => Expr::Template(
                    template
                        .parts
                        .iter()
                        .map(|part| match part {
                            Part::Lit(text) => Ok(Expr::string(text)),
                            Part::Dynamic(value) => self.lower(value),
                            _ => Err(Error::Unsupported {
                                what: "a context reference",
                            }),
                        })
                        .collect::<Result<_, _>>()?,
                ),
                Value::Array(array) => Expr::Array(
                    array
                        .elem
                        .iter()
                        .map(|item| match item {
                            ArrayElem::Item(value) => self.lower(value),
                            ArrayElem::Spread(value) => self.lower(value).map(|v| Expr::Spread(Box::new(v))),
                        })
                        .collect::<Result<_, _>>()?,
                ),
                Value::Object(object) => Expr::Object(
                    object
                        .elem
                        .iter()
                        .map(|field| Ok((field.key.clone(), self.lower(&field.value)?)))
                        .collect::<Result<_, Error>>()?,
                ),
                Value::CtxRef(ctx) => {
                    let pair = self.loops.last().ok_or(Error::Unsupported {
                        what: "repeat context outside repeat",
                    })?;
                    Expr::from(match ctx {
                        CtxRef::RepeatIndex => pair.0.clone(),
                        CtxRef::RepeatCount => pair.1.clone(),
                        _ => return Err(Error::Unsupported { what: "trace.index" }),
                    })
                }
                Value::Func { func, .. } => self.func(func)?,
                Value::Unary { op, operand, .. } => Expr::Unary {
                    op: *op,
                    value: Box::new(self.lower(operand)?),
                },
                Value::Binary { op, lhs, rhs, .. } => self.lower(lhs)?.binary(*op, self.lower(rhs)?),
                Value::Cond { cond, then, otherwise } => Expr::Conditional {
                    condition: Box::new(self.lower(cond)?),
                    yes: Box::new(self.lower(then)?),
                    no: Box::new(self.lower(otherwise)?),
                },
                Value::Index { target, index, .. } => self.lower(target)?.index(self.lower(index)?),
                Value::Slice { target, start, end, .. } => Expr::Slice {
                    value: Box::new(self.lower(target)?),
                    start: start.as_deref().map(|v| self.lower(v).map(Box::new)).transpose()?,
                    end: end.as_deref().map(|v| self.lower(v).map(Box::new)).transpose()?,
                },
                Value::VarRef(_) => {
                    return Err(Error::Unsupported {
                        what: "a variable binding",
                    });
                }
                Value::BlockRef { .. } => {
                    return Err(Error::Unsupported {
                        what: "a block reference",
                    });
                }
                Value::Attachment { .. } => return Err(Error::Unsupported { what: "`attachment`" }),
            })
        }
        fn func(&mut self, func: &Func) -> Result<Expr, Error> {
            let python = self.lang == ScorerLang::Python;
            let number = |v: f64| Expr::Number(format!("{v:?}"));
            let method = |value: Expr, name: &str, args: Vec<Expr>| Expr::call(value.member(name), args);
            let native = |py: &str, ts: &str| {
                if python {
                    Expr::from(py)
                } else {
                    Expr::from("Math").member(ts)
                }
            };
            Ok(match func {
                Func::Choice(options) => {
                    let args = vec![Expr::Array(options.iter().map(|v| self.lower(v)).collect::<Result<_, _>>()?)];
                    if python {
                        self.imports.insert("random");
                        Expr::call(Expr::from("random").member("choice"), args)
                    } else {
                        self.need(Helper::Choice);
                        Expr::call("choice", args)
                    }
                }
                Func::Range(Range::Int { min, max }) => {
                    let target = if python {
                        self.imports.insert("random");
                        Expr::from("random").member("randint")
                    } else {
                        self.need(Helper::RandInt);
                        "randomInt".into()
                    };
                    Expr::call(target, vec![Expr::number(min), Expr::number(max)])
                }
                Func::Range(Range::Float { min, max }) => {
                    if python {
                        self.imports.insert("random");
                        Expr::call(Expr::from("random").member("uniform"), vec![number(*min), number(*max)])
                    } else {
                        number(*min).binary(
                            BinOp::Add,
                            Expr::call(Expr::from("Math").member("random"), vec![])
                                .binary(BinOp::Mul, number(*max).binary(BinOp::Sub, number(*min))),
                        )
                    }
                }
                Func::Weighted(options) => {
                    if python {
                        self.imports.insert("random");
                        Expr::Call {
                            function: Box::new(Expr::from("random").member("choices")),
                            args: vec![Expr::Array(
                                options.iter().map(|v| self.lower(&v.value)).collect::<Result<_, _>>()?,
                            )],
                            keywords: vec![(
                                "weights".into(),
                                Expr::Array(options.iter().map(|v| number(v.weight)).collect()),
                            )],
                        }
                        .index(Expr::number(0))
                    } else {
                        self.need(Helper::Weighted);
                        Expr::call(
                            "weighted",
                            vec![Expr::Array(
                                options
                                    .iter()
                                    .map(|v| Ok(Expr::Array(vec![self.lower(&v.value)?, number(v.weight)])))
                                    .collect::<Result<_, Error>>()?,
                            )],
                        )
                    }
                }
                Func::Normal { mean, stddev }
                | Func::Lognormal {
                    median: mean,
                    sigma: stddev,
                }
                | Func::Beta {
                    alpha: mean,
                    beta: stddev,
                }
                | Func::Pareto {
                    min: mean,
                    shape: stddev,
                } => {
                    let (py, ts, helper) = match func {
                        Func::Normal { .. } => ("gauss", "normal", Helper::Normal),
                        Func::Lognormal { .. } => ("lognormvariate", "lognormal", Helper::Lognormal),
                        Func::Beta { .. } => ("betavariate", "beta", Helper::Beta),
                        _ => ("paretovariate", "pareto", Helper::Pareto),
                    };
                    if python {
                        self.imports.insert("random");
                        if matches!(func, Func::Pareto { .. }) {
                            number(*mean).binary(BinOp::Mul, Expr::call(Expr::from("random").member(py), vec![number(*stddev)]))
                        } else {
                            let first = if matches!(func, Func::Lognormal { .. }) {
                                self.imports.insert("math");
                                Expr::call(Expr::from("math").member("log"), vec![number(*mean)])
                            } else {
                                number(*mean)
                            };
                            Expr::call(Expr::from("random").member(py), vec![first, number(*stddev)])
                        }
                    } else {
                        self.need(helper);
                        Expr::call(ts, vec![number(*mean), number(*stddev)])
                    }
                }
                Func::Exponential { mean } => {
                    if python {
                        self.imports.insert("random");
                        Expr::call(
                            Expr::from("random").member("expovariate"),
                            vec![Expr::number(1).binary(BinOp::Div, number(*mean))],
                        )
                    } else {
                        self.need(Helper::Exponential);
                        Expr::call("exponential", vec![number(*mean)])
                    }
                }
                Func::Poisson { mean } => {
                    if python {
                        self.imports.insert("math");
                        self.imports.insert("random");
                    }
                    self.need(Helper::Poisson);
                    Expr::call("poisson", vec![number(*mean)])
                }
                Func::Chance { probability } => {
                    let target = if python {
                        self.imports.insert("random");
                        Expr::from("random").member("random")
                    } else {
                        Expr::from("Math").member("random")
                    };
                    Expr::call(target, vec![]).binary(BinOp::Lt, number(*probability))
                }
                Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => {
                    let name = match (func, python) {
                        (Func::Upper { .. }, true) => "upper",
                        (Func::Upper { .. }, false) => "toUpperCase",
                        (Func::Lower { .. }, true) => "lower",
                        (Func::Lower { .. }, false) => "toLowerCase",
                        (_, true) => "strip",
                        _ => "trim",
                    };
                    method(self.lower(text)?, name, vec![])
                }
                Func::Replace { text, from, to } => method(
                    self.lower(text)?,
                    if python { "replace" } else { "replaceAll" },
                    vec![self.lower(from)?, self.lower(to)?],
                ),
                Func::Split { text, separator } => method(self.lower(text)?, "split", vec![self.lower(separator)?]),
                Func::Join { array, separator } => {
                    if python {
                        method(self.lower(separator)?, "join", vec![self.lower(array)?])
                    } else {
                        method(self.lower(array)?, "join", vec![self.lower(separator)?])
                    }
                }
                Func::Contains { target, needle } => {
                    Expr::Contains(Box::new(self.lower(needle)?), Box::new(self.lower(target)?))
                }
                Func::StartsWith { text, prefix } => method(
                    self.lower(text)?,
                    if python { "startswith" } else { "startsWith" },
                    vec![self.lower(prefix)?],
                ),
                Func::EndsWith { text, suffix } => method(
                    self.lower(text)?,
                    if python { "endswith" } else { "endsWith" },
                    vec![self.lower(suffix)?],
                ),
                Func::Len { target } => {
                    let value = self.lower(target)?;
                    if python {
                        Expr::call("len", vec![value])
                    } else {
                        value.member("length")
                    }
                }
                Func::Format { pieces, args } => {
                    let mut parts = vec![];
                    for (i, piece) in pieces.iter().enumerate() {
                        if !piece.is_empty() {
                            parts.push(Expr::string(piece));
                        }
                        if let Some(arg) = args.get(i) {
                            parts.push(self.lower(arg)?);
                        }
                    }
                    Expr::Template(parts)
                }
                Func::Clamp { value, min, max } => Expr::call(
                    native("max", "max"),
                    vec![
                        self.lower(min)?,
                        Expr::call(native("min", "min"), vec![self.lower(max)?, self.lower(value)?]),
                    ],
                ),
                Func::Round { value } => {
                    if python {
                        self.imports.insert("math");
                    }
                    self.need(Helper::Round);
                    Expr::call(
                        if python { "round_away_from_zero" } else { "roundAwayFromZero" },
                        vec![self.lower(value)?],
                    )
                }
                Func::Floor { value } | Func::Ceil { value } | Func::Abs { value } => {
                    let name = match func {
                        Func::Floor { .. } => "floor",
                        Func::Ceil { .. } => "ceil",
                        _ => "abs",
                    };
                    let target = if python && name == "abs" {
                        Expr::from("abs")
                    } else if python {
                        self.imports.insert("math");
                        Expr::from("math").member(name)
                    } else {
                        Expr::from("Math").member(name)
                    };
                    Expr::call(target, vec![self.lower(value)?])
                }
                Func::Min(values) | Func::Max(values) => {
                    let name = if matches!(func, Func::Min(_)) { "min" } else { "max" };
                    Expr::call(
                        native(name, name),
                        values.iter().map(|v| self.lower(v)).collect::<Result<_, _>>()?,
                    )
                }
                Func::Uuid => {
                    if python {
                        self.imports.insert("uuid");
                        Expr::call("str", vec![Expr::call(Expr::from("uuid").member("uuid4"), vec![])])
                    } else {
                        Expr::call(Expr::from("crypto").member("randomUUID"), vec![])
                    }
                }
                Func::Hex { length } | Func::Alphanum { length } => {
                    let hex = matches!(func, Func::Hex { .. });
                    if python {
                        self.imports.insert("random");
                        let pool = if hex {
                            Expr::string("0123456789abcdef")
                        } else {
                            self.imports.insert("string");
                            Expr::from("string")
                                .member("ascii_letters")
                                .binary(BinOp::Add, Expr::from("string").member("digits"))
                        };
                        method(
                            Expr::string(""),
                            "join",
                            vec![Expr::Call {
                                function: Box::new(Expr::from("random").member("choices")),
                                args: vec![pool],
                                keywords: vec![("k".into(), Expr::number(length))],
                            }],
                        )
                    } else {
                        self.need(if hex { Helper::Hex } else { Helper::Alphanum });
                        Expr::call(if hex { "randomHex" } else { "randomAlphanum" }, vec![Expr::number(length)])
                    }
                }
                Func::Attachment { .. } => return Err(Error::Unsupported { what: "`attachment`" }),
                Func::Tokens { .. } => return Err(Error::Unsupported { what: "`tokens`" }),
                Func::Noise { .. } => return Err(Error::Unsupported { what: "`noise`" }),
            })
        }
    }

    // language-neutral runtime helpers a generated scorer may need; collected
    // during emission so only used snippets appear, in declaration order (normal
    // first, its dependents after)
    #[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
    pub(in crate::mocking::code) enum Helper {
        Normal,
        Gamma,
        Lognormal,
        Exponential,
        Pareto,
        Beta,
        Poisson,
        Choice,
        Weighted,
        RandInt,
        Hex,
        Alphanum,
        Round,
    }

    // one code scorer's render inputs; several items pack into one module
    pub(crate) struct Item<'m> {
        pub(crate) name: &'m str,
        pub(crate) slug: &'m str,
        pub(crate) score: &'m Value,
        pub(crate) steps: &'m [ScorerStep],
    }

    // Render named scorer functions with their shared imports and helpers.
    pub(crate) fn module(lang: ScorerLang, items: &[Item]) -> Result<String, Error> {
        match lang {
            ScorerLang::Python => crate::mocking::code::languages::python::source(items),
            ScorerLang::Typescript => crate::mocking::code::languages::typescript::source(items),
        }
    }

    // json-style escaping; both languages read the result inside double quotes
    pub(in crate::mocking::code) fn escape(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for part in text.chars() {
            match part {
                '\\' => escaped.push_str("\\\\"),
                '"' => escaped.push_str("\\\""),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                part => escaped.push(part),
            }
        }
        escaped
    }

    // {:?} keeps a trailing .0 on whole floats, valid in both languages
    pub(in crate::mocking::code) fn num(number: &Number) -> String {
        match number {
            Number::Int(value) => value.to_string(),
            Number::Float(value) => format!("{value:?}"),
        }
    }

    pub(in crate::mocking::code) fn arg_name(arg: ScorerArg) -> &'static str {
        match arg {
            ScorerArg::Input => "input",
            ScorerArg::Output => "output",
            ScorerArg::Expected => "expected",
            ScorerArg::Metadata => "metadata",
        }
    }

    // Slugs contain only lowercase ASCII letters, digits, and hyphens. This
    // mapping stays unique while producing an identifier safe in both languages.
    pub(in crate::mocking::code) fn fn_name(slug: &str) -> String {
        format!("scorer_{}", slug.replace('-', "_"))
    }

    #[derive(Debug)]
    pub(crate) enum Error {
        // the modeler rejects everything untranspilable, so this only fires on a
        // contract break between the language model and code generation
        Unsupported { what: &'static str },
    }

    impl fmt::Display for Error {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Unsupported { what } => {
                    write!(
                        formatter,
                        "scorer expression contains {what}, which cannot emit as scorer code"
                    )
                }
            }
        }
    }

    impl std::error::Error for Error {}
}

pub(crate) fn literal(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Str(v) => json!(v),
        Value::Num(Number::Int(v)) => json!(v),
        Value::Num(Number::Float(v)) => json!(v),
        Value::Bool(v) => json!(v),
        Value::Null => Json::Null,
        Value::Object(v) => {
            let mut object = serde_json::Map::new();
            for field in &v.elem {
                object.insert(field.key.clone(), literal(&field.value)?);
            }
            Json::Object(object)
        }
        Value::Array(v) => {
            let mut values = Vec::new();
            for item in &v.elem {
                match item {
                    ArrayElem::Item(v) => values.push(literal(v)?),
                    ArrayElem::Spread(v) => {
                        let Json::Array(items) = literal(v)? else {
                            return Err("eval dataset spread must be an array".into());
                        };
                        values.extend(items);
                    }
                }
            }
            Json::Array(values)
        }
        _ => return Err("generated eval cases require literal values; dynamic datasets are not supported yet".into()),
    })
}

pub(crate) fn cases(dataset: &crate::dsl::Dataset) -> Result<Vec<Json>, String> {
    use crate::dsl::DatasetSource;
    let mut rows = Vec::new();
    for case in &dataset.cases {
        let DatasetSource::Inline(input) = &case.source else {
            return Err(format!("generated eval dataset {:?} requires inline cases", dataset.name));
        };
        let mut row = json!({"input": literal(input)?});
        for (key, value) in [
            ("expected", &case.expected),
            ("metadata", &case.metadata),
            ("tags", &case.tags),
        ] {
            if let Some(value) = value {
                row[key] = literal(value)?;
            }
        }
        rows.push(row);
    }
    if rows.is_empty() {
        return Err(format!("generated eval dataset {:?} has no cases", dataset.name));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use crate::dsl::ScorerLang;
    use crate::mocking::code::{
        languages::rendering as emission,
        lowering::code::{Expr, Function, Parameter, Statement, Variable},
    };

    #[test]
    fn function_scopes_own_parameters_and_locals_without_leaking_nested_bindings() {
        let mut outer = Function::new("outer", vec![Parameter::object(&["input", "output"])]);
        assert!(outer.declare("input", "0").is_err());
        outer.declare("result", "output").unwrap();
        assert!(outer.declare("result", "1").is_err());
        let mut inner = Function::new("inner", vec![Parameter::new("input")]);
        inner.declare("result", "input").unwrap();
        inner.declare("private_value", "1").unwrap();
        outer.body.push(Statement::Function(inner));
        assert_eq!(outer.local_names(), ["input", "output", "result", "inner"]);
        assert!(outer.declare("inner", "0").is_err());
        outer.declare("private_value", "2").unwrap();
    }

    #[test]
    fn emits_nested_functions_mutable_declarations_and_reassignments() {
        let mut outer = Function::new("outer", vec![Parameter::new("input").with_default(Expr::number(1))]);
        outer.body.push(Statement::Variable(Variable::Declare {
            name: "result".into(),
            value: "input".into(),
            mutable: true,
            annotation: None,
        }));
        let mut inner = Function::new("inner", vec![Parameter::new("value")]);
        inner.body.push(Statement::Return("value".into()));
        outer.body.push(Statement::Function(inner));
        outer.body.push(Statement::Variable(Variable::Assign {
            name: "result".into(),
            value: Expr::call("inner", vec!["result".into()]),
        }));
        outer.body.push(Statement::Return("result".into()));
        assert_eq!(
            emission::function(&outer, ScorerLang::Python, 4),
            "def outer(input=1):\n    result = input\n    def inner(value):\n        return value\n    result = inner(result)\n    return result\n"
        );
        assert_eq!(
            emission::function(&outer, ScorerLang::Typescript, 2),
            "function outer(input = 1) {\n  let result = input;\n  function inner(value) {\n    return value;\n  }\n  result = inner(result);\n  return result;\n}\n"
        );
    }

    #[test]
    fn preserves_signature_features_and_empty_bodies() {
        let mut function = Function::new(
            "task",
            vec![
                Parameter::new("input"),
                Parameter::new("limit").with_default(Expr::number(1)).keyword(),
            ],
        );
        function.asynchronous = true;
        assert_eq!(
            emission::function(&function, ScorerLang::Python, 4),
            "async def task(input, *, limit=1):\n    pass\n"
        );
        function.parameters = vec![Parameter::object(&["input", "output"]).typed("{ input: number; output: number }")];
        function.exported = true;
        function.returns = Some("Promise<void>".into());
        assert_eq!(
            emission::function(&function, ScorerLang::Typescript, 2),
            "export async function task({ input, output }: { input: number; output: number }): Promise<void> {\n}\n"
        );
    }
    #[test]
    fn expression_precedence_preserves_grouping_and_short_circuiting() {
        use crate::dsl::{BinOp, UnaryOp};
        let subtraction = Expr::from("a").binary(BinOp::Sub, Expr::from("b").binary(BinOp::Sub, "c".into()));
        let comparison = Expr::from("a").binary(BinOp::Lt, "b".into()).binary(BinOp::Eq, "c".into());
        let conditional = Expr::Conditional {
            condition: Box::new("enabled".into()),
            yes: Box::new(Expr::call("work", vec![])),
            no: Box::new(Expr::number(0)),
        };
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            assert_eq!(emission::expression(&subtraction, lang), "a - (b - c)");
            assert_eq!(
                emission::expression(
                    &Expr::Unary {
                        op: UnaryOp::Neg,
                        value: Box::new(Expr::number(-1))
                    },
                    lang
                ),
                "-(-1)"
            );
            let grouped = emission::expression(&Expr::number(2).binary(BinOp::Mul, conditional.clone()), lang);
            assert!(grouped.starts_with("2 * ("));
        }
        assert_eq!(emission::expression(&comparison, ScorerLang::Python), "(a < b) == c");
        assert_eq!(emission::expression(&comparison, ScorerLang::Typescript), "(a < b) === c");
        assert_eq!(
            emission::expression(
                &Expr::Bool(false).binary(BinOp::And, Expr::call("fail", vec![])),
                ScorerLang::Python
            ),
            "False and fail()"
        );
        assert_eq!(
            emission::expression(
                &Expr::Bool(false).binary(BinOp::And, Expr::call("fail", vec![])),
                ScorerLang::Typescript
            ),
            "false && fail()"
        );
    }

    #[test]
    fn dependency_reads_ignore_literals_and_respect_closure_scopes() {
        use crate::mocking::code::lowering::code::{Dependency, references};
        use std::collections::HashSet;
        let literal = Expr::string("_read(root, \"output\") captured()");
        assert!(literal.references().is_empty());
        let local_read = Expr::from("local")
            .member("output")
            .read(Dependency::Field("local".into(), "output".into()));
        let expression = Expr::Array(vec![local_read, Expr::from("captured"), literal]).lambda(vec![Parameter::new("local")]);
        assert_eq!(
            expression.references(),
            HashSet::from([Dependency::Variable("captured".into())])
        );
        let mut nested = Function::new("nested", vec![Parameter::new("argument").with_default("default_value")]);
        nested.declare("local", "argument").unwrap();
        nested
            .body
            .push(Statement::Return(Expr::Array(vec!["local".into(), "captured".into()])));
        assert_eq!(
            references(&[Statement::Function(nested)]),
            HashSet::from([
                Dependency::Variable("captured".into()),
                Dependency::Variable("default_value".into())
            ])
        );
    }

    #[test]
    fn dependency_order_uses_semantic_reads_and_rejects_cycles() {
        use crate::mocking::code::{
            lowering::code::Dependency,
            planning::{Operation, order},
        };
        use std::collections::HashSet;
        let input = Dependency::Field("root".into(), "input".into());
        let output = Dependency::Field("root".into(), "output".into());
        let operation = |key, reads, value| Operation {
            key,
            reads,
            code: vec![Statement::Evaluate(Expr::string(value))],
        };
        let body = order(
            vec![
                operation(output.clone(), HashSet::from([input.clone()]), "output"),
                operation(input.clone(), HashSet::new(), "input"),
            ],
            "root",
        )
        .unwrap();
        assert!(matches!(&body[0],Statement::Evaluate(Expr::String(value)) if value=="input"));
        assert!(
            order(
                vec![
                    operation(output.clone(), HashSet::from([input.clone()]), "output"),
                    operation(input, HashSet::from([output]), "input")
                ],
                "root"
            )
            .is_err()
        );
    }
}
