//! Lower domain expressions into shared code structures.

use crate::dsl::{Part, Value};

/// Code structure shared by the target emitters.
#[allow(dead_code)] // Shared IR operations are also used by the agent backend.
pub(super) mod code {
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

pub(super) mod scoring {
    use crate::dsl::{Number, ScorerArg, ScorerLang, ScorerStep, Value};
    use std::fmt;

    use super::code::{Expr, Statement, Variable};
    use crate::dsl::{ArrayElem, BinOp, CtxRef, Func, Part, Range};
    use std::collections::BTreeSet;
    pub(in crate::scg) fn function(
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
    pub(in crate::scg) struct Compiler {
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
            Ok(crate::scg::emission::expression(&self.lower(value)?, self.lang))
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
    pub(in crate::scg) enum Helper {
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
            ScorerLang::Python => crate::scg::emission::python::module(items),
            ScorerLang::Typescript => crate::scg::emission::typescript::module(items),
        }
    }

    // json-style escaping; both languages read the result inside double quotes
    pub(in crate::scg) fn escape(text: &str) -> String {
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
    pub(in crate::scg) fn num(number: &Number) -> String {
        match number {
            Number::Int(value) => value.to_string(),
            Number::Float(value) => format!("{value:?}"),
        }
    }

    pub(in crate::scg) fn arg_name(arg: ScorerArg) -> &'static str {
        match arg {
            ScorerArg::Input => "input",
            ScorerArg::Output => "output",
            ScorerArg::Expected => "expected",
            ScorerArg::Metadata => "metadata",
        }
    }

    // Slugs contain only lowercase ASCII letters, digits, and hyphens. This
    // mapping stays unique while producing an identifier safe in both languages.
    pub(in crate::scg) fn fn_name(slug: &str) -> String {
        format!("scorer_{}", slug.replace('-', "_"))
    }

    #[derive(Debug)]
    pub(crate) enum Error {
        // the modeler rejects everything untranspilable, so this only fires on a
        // contract break between the dsl and scg
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

// a judge prompt renders to braintrust's template syntax: argument-path holes
// become {{path}} slots; the modeler validated every hole is such a path
pub(super) fn mustache(prompt: &Value) -> String {
    match prompt {
        Value::Str(text) => text.clone(),
        Value::Template(template) => template
            .parts
            .iter()
            .map(|part| match part {
                Part::Lit(text) => text.clone(),
                Part::Dynamic(value) => format!("{{{{{}}}}}", slot_path(value)),
                Part::Ref(_) | Part::VarRef(_) => unreachable!("the modeler validated judge prompt holes"),
            })
            .collect(),
        _ => unreachable!("the modeler validated the judge prompt shape"),
    }
}

fn slot_path(value: &Value) -> String {
    match value {
        Value::ArgRef(arg) => scoring::arg_name(*arg).to_owned(),
        Value::Index { target, index, .. } => match &**index {
            Value::Str(key) => format!("{}.{key}", slot_path(target)),
            _ => unreachable!("the modeler validated judge prompt holes"),
        },
        _ => unreachable!("the modeler validated judge prompt holes"),
    }
}
