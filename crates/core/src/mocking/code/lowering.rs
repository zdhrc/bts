//! Lower domain expressions and execution scopes into generated functions.

use crate::dsl::{
    Accessor, ArrayElem, BinOp, Child, CtxRef, Field, Func, Model, NodeId, Number, Part, ScorerLang, Selection, SpanFields,
    Step, Trace, UnaryOp, Value,
};
use code::{Dependency, Expr, Statement, Variable};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, HashSet};

pub(crate) struct Agent {
    pub functions: Vec<code::Function>,
    // the provider helpers the agent calls
    pub helpers: Vec<crate::mocking::Helper>,
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
                Statement::Break => {}
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
                Statement::Break => {}
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
        Break,
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
    // values declared inside a step instead of beside it: the value, the step that holds it, the scope it left
    moved: Vec<(String, NodeId, NodeId)>,
    // moved values nothing outside their step reads, which the step does not hand back
    unkept: Vec<String>,
    // values that are another value under a second name, and the value they stand for
    aliases: Vec<(String, String)>,
    // calls to provider functions an agent in this language cannot run
    unsupported: &'a Unsupported,
    // how the agent reads which trace of a run it is, when it can be told
    trace_index: Option<Expr>,
}

/// Provider function calls a consumer met and cannot run, each at its location.
pub(crate) type Unsupported = std::cell::RefCell<bts_lang::Diags>;

fn unsupported_call(
    function: &crate::mocking::Function,
    consumer: crate::mocking::Consumer,
    range: bts_lang::SrcRange,
) -> bts_lang::Diag {
    bts_lang::Diag {
        when: bts_lang::DiagPhase::Validation,
        what: format!("function `{}` is not supported by {}", function.name, consumer.name()),
        r#where: range,
    }
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

// the repeats of a plan that run as loops
fn looping(children: &[Child], found: &mut Vec<NodeId>) {
    for child in children {
        match child {
            Child::Span(span) => looping(&span.children, found),
            Child::Choice(choice) => looping(&choice.children, found),
            Child::Maybe(maybe) => looping(&maybe.children, found),
            Child::Scorer(_) => {}
            Child::Repeat(repeat) => {
                if repeat.again.is_some() {
                    found.push(repeat.node);
                }
                looping(&repeat.children, found);
            }
        }
    }
}

fn random_name(lang: ScorerLang) -> &'static str {
    match lang {
        ScorerLang::Python => "random_source",
        ScorerLang::Typescript => "randomSource",
    }
}

fn collect_names(children: &[Child], lang: ScorerLang, used: &mut HashSet<String>) -> Vec<(NodeId, Names)> {
    // how many steps print under each name
    fn count(children: &[Child], shared: &mut HashMap<String, usize>) {
        for child in children {
            match child {
                Child::Span(span) => {
                    let stem = crate::mocking::code::planning::identifier_stem(&span.name).unwrap_or_default();
                    *shared.entry(stem).or_default() += 1;
                    count(&span.children, shared);
                }
                Child::Repeat(repeat) => count(&repeat.children, shared),
                Child::Choice(choice) => count(&choice.children, shared),
                Child::Maybe(maybe) => count(&maybe.children, shared),
                Child::Scorer(_) => {}
            }
        }
    }
    // steps that share a name are told apart by the identifiers they were declared under
    fn readable<'a>(span: &'a crate::dsl::Span, shared: &HashMap<String, usize>) -> &'a str {
        let stem = crate::mocking::code::planning::identifier_stem(&span.name).unwrap_or_default();
        match &span.label {
            Some(label) if shared.get(&stem).copied().unwrap_or(0) > 1 => label,
            _ => &span.name,
        }
    }
    fn visit(
        children: &[Child],
        shared: &HashMap<String, usize>,
        lang: ScorerLang,
        used: &mut HashSet<String>,
        names: &mut Vec<(NodeId, Names)>,
    ) {
        for child in children {
            match child {
                Child::Span(span) => {
                    let name = readable(span, shared);
                    names.push((
                        span.node,
                        Names {
                            function: allocate(name, "", lang, used),
                            state: allocate(name, "span", lang, used),

                            ..Names::default()
                        },
                    ));
                    visit(&span.children, shared, lang, used, names);
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
                    visit(&repeat.children, shared, lang, used, names);
                }
                Child::Choice(choice) => {
                    branch_names(choice.node, choice.name.as_deref().unwrap_or("choice"), lang, used, names);
                    visit(&choice.children, shared, lang, used, names);
                }
                Child::Maybe(maybe) => {
                    // a path that may not run is named for the step it guards
                    let guarded = match maybe.children.as_slice() {
                        [Child::Span(span)] => Some(format!("maybe_{}", readable(span, shared))),
                        _ => None,
                    };
                    let name = maybe.name.clone().or(guarded).unwrap_or_else(|| "optional".into());
                    branch_names(maybe.node, &name, lang, used, names);
                    visit(&maybe.children, shared, lang, used, names);
                }
                Child::Scorer(v) => {
                    names.push((
                        v.node,
                        Names {
                            function: allocate(&v.name, "", lang, used),
                            state: allocate(&v.name, "span", lang, used),
                            ..Names::default()
                        },
                    ));
                }
            }
        }
    }
    fn branch_names(node: NodeId, name: &str, lang: ScorerLang, used: &mut HashSet<String>, names: &mut Vec<(NodeId, Names)>) {
        names.push((
            node,
            Names {
                function: allocate(name, "", lang, used),
                state: allocate(name, "span", lang, used),

                ..Names::default()
            },
        ));
    }
    let mut shared = HashMap::new();
    count(children, &mut shared);
    let mut names = Vec::new();
    visit(children, &shared, lang, used, &mut names);
    names
}

pub(crate) fn agent(
    model: &Model,
    trace: &Trace,
    output: &Value,
    lang: ScorerLang,
    unsupported: &Unsupported,
) -> Result<Agent, String> {
    execution::emit(model, trace, output, lang, unsupported)
}

fn expression(value: &Value, ctx: &Context<'_>, lang: ScorerLang) -> Result<Expr, String> {
    let expr = |v: &Value| expression(v, ctx, lang);
    Ok(match value {
        Value::CtxRef(reference) => context_ref(*reference, ctx)?,
        // the call is reported with its location and the rest of the agent is still checked
        Value::Func {
            func: Func::Provider { function, .. },
            range,
        } if function.helper(lang).is_none() => {
            let diag = unsupported_call(function, crate::mocking::Consumer::Agent(lang), *range);
            let mut unsupported = ctx.unsupported.borrow_mut();
            if !unsupported.contains(&diag) {
                unsupported.push(diag);
            }
            Expr::Null
        }
        Value::Func { func, .. } => expressions::emit(func, ctx, lang)?,
        Value::Index { target, index, .. } => Expr::call("_index", vec![expr(target)?, expr(index)?]),
        Value::Slice { target, start, end, .. } => {
            slice_expression(expr(target)?, start.as_deref(), end.as_deref(), ctx, lang)?
        }
        Value::VarRef(name) => {
            let mut name = name;
            for _ in 0..ctx.aliases.len() {
                match ctx.aliases.iter().find(|(alias, _)| alias == name) {
                    Some((_, target)) => name = target,
                    None => break,
                }
            }
            // a value held by a step that is not running here is read from what that step kept
            let held =
                (ctx.moved.iter()).find(|(moved, holder, _)| moved == name && !ctx.frames.iter().any(|(id, _)| id == holder));
            if let Some((_, holder, scope)) = held {
                let step = Step::Child {
                    candidates: vec![*holder],
                    position: None,
                };
                return reference_expression(*scope, &[step], &Accessor::Binding(name.clone()), &[], None, ctx, lang);
            }
            Expr::call(
                ctx.local_names
                    .iter()
                    .find(|(n, _)| n == name)
                    .ok_or_else(|| format!("unknown agent variable {name:?}"))?
                    .1
                    .clone(),
                vec![],
            )
        }
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
                            // statements in order evaluate every item where the array is built
                            if ctx.direct { e } else { e.lazy() }
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
            let mut loops = Vec::new();
            for trace in &ctx.model.traces {
                looping(&trace.children, &mut loops);
            }
            let through_loop = reference.steps.windows(2).any(|steps| {
                matches!((&steps[0], &steps[1]), (Step::Child { candidates, .. }, Step::Iterations { .. }) if loops.contains(&candidates[0]))
            });
            if matches!(reference.accessor, Accessor::Record) && !through_loop {
                let group = reference
                    .steps
                    .windows(2)
                    .rev()
                    .find_map(|steps| match (&steps[0], &steps[1]) {
                        (Step::Child { candidates, .. }, Step::Iterations { .. }) => candidates.first().copied(),
                        _ => None,
                    })
                    .ok_or("history requires a repetition group")?;
                let target = reference
                    .steps
                    .iter()
                    .rev()
                    .find_map(|step| match step {
                        Step::Child { candidates, .. } => candidates.first().copied(),
                        _ => None,
                    })
                    .ok_or("history requires a target")?;
                let frame = ctx
                    .frames
                    .iter()
                    .rev()
                    .find(|(node, _)| *node == ctx.site)
                    .ok_or("history requires an active node")?;
                return Ok(Expr::call(
                    "_history",
                    vec![
                        Expr::Name(frame.1.clone()),
                        Expr::string(&ctx.names(target).state),
                        Expr::string(&ctx.names(group).state),
                    ],
                ));
            }
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
        // an agent that cannot be told is the first trace
        return Ok(ctx.trace_index.clone().unwrap_or_else(|| Expr::number(0)));
    }
    if let CtxRef::RepeatIndexAt(node) = reference {
        return ctx
            .iterations
            .iter()
            .find(|(id, _, _)| *id == node)
            .map(|(_, index, _)| Expr::Name(index.clone()))
            .ok_or("repeat context is unavailable".into());
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
        && ctx.frames.iter().any(|(id, _)| *id == candidates[0])
    {
        let own =
            matches!(rest.first(), Some(Step::Iteration(Value::CtxRef(CtxRef::RepeatIndexAt(id)))) if *id == candidates[0]);
        let other = matches!(rest.first(), Some(Step::Iteration(_) | Step::Iterations { .. } | Step::Last));
        // the frame of a repeat is the iteration that is running, another one is read from the collection
        if own || !other {
            let rest = if own { &rest[1..] } else { rest };
            return reference_expression(candidates[0], rest, accessor, path, None, ctx, lang);
        }
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
            Step::Last => reference_expression(node, rest, accessor, path, Some(Expr::call("_last", vec![frame])), ctx, lang),
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
        Accessor::Record => Expr::call("_occurrence", vec![frame]),
        Accessor::Field(field) => read_field(frame, field_key(field), ctx.direct),
        Accessor::Chosen => read_field(frame, "chosen", ctx.direct),
        Accessor::Included => read_field(frame, "included", ctx.direct),
        Accessor::Binding(name) => {
            let local = &ctx
                .local_names
                .iter()
                .find(|(found, _)| found == name)
                .ok_or("unknown kept value")?
                .1;
            read_field(frame, local, false)
        }
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

    pub(crate) fn emit(
        model: &Model,
        trace: &Trace,
        output: &Value,
        lang: ScorerLang,
        unsupported: &Unsupported,
    ) -> Result<Agent, String> {
        emit_form(model, trace, output, lang, true, false, unsupported)
    }
    // `ordered` prints statements in the order they run, and is tried before the deferred form
    fn emit_form(
        model: &Model,
        trace: &Trace,
        output: &Value,
        lang: ScorerLang,
        ordered: bool,
        // whether the agent reads which trace of a run it is
        told: bool,
        unsupported: &Unsupported,
    ) -> Result<Agent, String> {
        let try_ordered = ordered;
        let mut parents = Vec::new();
        let mut bindings = model.bindings.iter().chain(&trace.bindings).collect::<Vec<_>>();
        // the step each of its own values belongs to
        let mut owners = HashMap::<&str, NodeId>::new();
        fn collect<'a>(
            children: &'a [Child],
            parent: NodeId,
            parents: &mut Vec<(NodeId, NodeId)>,
            bindings: &mut Vec<&'a Binding>,
            owners: &mut HashMap<&'a str, NodeId>,
        ) -> Result<(), String> {
            for child in children {
                let (node, locals, children) = match child {
                    Child::Span(v) => (v.node, &v.bindings, &v.children),
                    Child::Repeat(v) => (v.node, &v.bindings, &v.children),
                    Child::Choice(v) => (v.node, &v.bindings, &v.children),
                    Child::Maybe(v) => (v.node, &v.bindings, &v.children),
                    Child::Scorer(v) => {
                        parents.push((v.node, parent));
                        bindings.extend(&v.bindings);
                        continue;
                    }
                };
                parents.push((node, parent));
                bindings.extend(locals);
                if let Child::Span(span) = child {
                    owners.extend(locals.iter().map(|binding| (binding.name.as_str(), node)));
                    // how long a step takes or waits is its own, wherever the value is declared
                    for (value, _) in span.fields.duration.iter().chain(&span.fields.wait) {
                        if let Value::VarRef(name) = value {
                            owners.insert(name.as_str(), node);
                        }
                    }
                }
                collect(children, node, parents, bindings, owners)?;
            }
            Ok(())
        }
        collect(&trace.children, trace.node, &mut parents, &mut bindings, &mut owners)?;
        let mut used: HashSet<String> = "context ExecutionContext input run run_details runDetails run_traced runTraced logger random_source randomSource root scope range len str int bool float type isinstance abs object print None True False def class return for while if else elif in is not and or lambda yield import from as with try except finally pass break continue global nonlocal assert del raise await async match case function const let var new this null true false switch default export extends super typeof void delete throw catch do of enum implements interface package private protected public static arguments eval item_at trim_text format_value duration_seconds checked_number validate_input count_tokens as_list new_uuid sample_range step_history fold_items agent_result slice_items length_of as_count values_equal calculate require_arrivals group_arrivals select_option weighted_option as_number as_boolean as_integer itemAt trimText formatValue durationSeconds checkedNumber validateInput countTokens asList newUuid sampleRange stepHistory foldItems agentResult sliceItems lengthOf asCount valuesEqual requireArrivals groupArrivals selectOption weightedOption asNumber asBoolean asInteger remainder divide parse_input parseInput round_down round_up round_number absolute clamp roundDown roundUp roundNumber seed seeded trace trace_of traceOf".split_whitespace().map(str::to_owned).collect();
        let reserved = used.clone();
        let names = collect_names(&trace.children, lang, &mut used);
        // values that took a number only because another scope already used their name
        let mut numbered = Vec::new();
        let plain = |binding: &Binding| {
            allocate(
                binding.label.as_deref().unwrap_or(&binding.name),
                "",
                lang,
                &mut HashSet::new(),
            )
        };
        // the step a value belongs to: the one that declares it, or the one its provider names
        let owned = |binding: &Binding| -> Option<String> {
            (owners.get(binding.name.as_str()))
                .and_then(|owner| names.iter().find(|(node, _)| node == owner))
                .map(|(_, names)| names.function.clone())
                .or_else(|| {
                    let owner = binding.owner?;
                    if owner == trace.node {
                        return Some(allocate(&trace.name, "", lang, &mut HashSet::new()));
                    }
                    (names.iter().find(|(node, _)| *node == owner)).map(|(_, names)| names.function.clone())
                })
        };
        // how many steps keep a value under each name
        let mut kept = HashMap::<String, usize>::new();
        for binding in bindings.iter().filter(|binding| owned(binding).is_some()) {
            *kept.entry(plain(binding)).or_default() += 1;
        }
        let local_names = bindings
            .iter()
            .map(|binding| {
                let readable = binding.label.as_deref().unwrap_or(&binding.name);
                let plain = plain(binding);
                // a step's value whose name is taken is told apart by its step, which says more than a number
                let step = owned(binding).filter(|_| used.contains(&plain) || kept[&plain] > 1);
                let name = match step.as_deref() {
                    Some(step) => {
                        let own = match lang {
                            ScorerLang::Python => format!("{step}_{plain}"),
                            ScorerLang::Typescript => format!("{step}{}{}", plain[..1].to_uppercase(), &plain[1..]),
                        };
                        let mut name = own.clone();
                        let mut suffix = 2;
                        while !used.insert(name.clone()) {
                            name = match lang {
                                ScorerLang::Python => format!("{own}_{suffix}"),
                                ScorerLang::Typescript => format!("{own}{suffix}"),
                            };
                            suffix += 1;
                        }
                        name
                    }
                    // the agent's own value that a step is also called takes the agent's name before it
                    None if names.iter().any(|(_, names)| names.function == plain)
                        && trace.bindings.iter().any(|own| own.name == binding.name) =>
                    {
                        allocate(&format!("{}_{readable}", trace.name), "", lang, &mut used)
                    }
                    None => allocate(readable, "", lang, &mut used),
                };
                if plain != name && !reserved.contains(&plain) {
                    numbered.push((name.clone(), plain));
                }
                (binding.name.clone(), name)
            })
            .collect::<Vec<(String, String)>>();
        // A value that is only another value is printed as that value. One that another scope
        // reads by name keeps its own declaration, since that read names it.
        let aliases: Vec<(String, String)> = bindings
            .iter()
            .filter_map(|binding| match &binding.value {
                Value::VarRef(target)
                    if !model
                        .refs
                        .iter()
                        .any(|reference| matches!(&reference.accessor, Accessor::Binding(name) if *name == binding.name)) =>
                {
                    Some((binding.name.clone(), target.clone()))
                }
                _ => None,
            })
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
            moved: Vec::new(),
            unkept: Vec::new(),
            aliases,
            unsupported,
            trace_index: told.then(|| {
                let name = if lang == ScorerLang::Python { "trace_of" } else { "traceOf" };
                Expr::call(name, vec![Expr::Name(random_name(lang).into())])
            }),
        };
        let all_bindings = model.bindings.iter().chain(&trace.bindings).cloned().collect::<Vec<_>>();
        let mut direct_ctx = ctx.clone();
        direct_ctx.direct = true;
        let mut loops = Vec::new();
        looping(&trace.children, &mut loops);
        let mut body = {
            let ordered = if try_ordered {
                scope(
                    Some(&trace.fields),
                    &all_bindings,
                    &trace.children,
                    "root",
                    &direct_ctx,
                    lang,
                    true,
                )
            } else {
                Err("the plan has no ordered form".to_owned())
            };
            match ordered {
                Ok(mut work) => {
                    work.push(Statement::Return(Expr::call(
                        "_agent_result",
                        vec![
                            Expr::call("_details", vec!["root".into()]),
                            expression(output, &direct_ctx, lang)?,
                        ],
                    )));
                    vec![Statement::TraceScope {
                        scope: "root".into(),
                        body: work,
                    }]
                }
                // statements that cannot be put in one order are emitted lazily instead
                Err(_) => {
                    let mut work = scope(Some(&trace.fields), &all_bindings, &trace.children, "root", &ctx, lang, true)?;
                    work.push(Statement::Return(Expr::call(
                        "_agent_result",
                        vec![Expr::call("_details", vec!["root".into()]), expression(output, &ctx, lang)?],
                    )));
                    work
                }
            }
        };
        let references = code::references(&body);
        let mut helpers: Vec<crate::mocking::Helper> = Vec::new();
        for helper in model.functions.iter().filter_map(|function| function.helper(lang)) {
            // one helper may serve several functions, and each says what its own call returns
            let called = references.contains(&Dependency::Variable(helper.name.into()));
            let known = |known: &crate::mocking::Helper| known.name == helper.name && known.operation == helper.operation;
            if called && !helpers.iter().any(known) {
                helpers.push(*helper);
            }
        }
        // a helper that needs the run's seed gets it from the random source, and the agent takes one
        let seeded = helpers.iter().any(|helper| helper.seeded);
        // such an agent can be any trace of the run, so it is printed again reading which one it is
        if seeded && !told {
            return emit_form(model, trace, output, lang, ordered, true, unsupported);
        }
        let seed: Vec<code::Parameter> = match (seeded, lang) {
            (false, _) => vec![],
            (true, ScorerLang::Python) => vec![
                code::Parameter::new("seed").with_default(Expr::Null).keyword(),
                code::Parameter::new("trace").with_default(Expr::number(0)).keyword(),
            ],
            (true, ScorerLang::Typescript) => vec![
                code::Parameter::new("seed")
                    // a seed past what a number holds exactly is given as a bigint
                    .typed("number | bigint | undefined")
                    .with_default(Expr::from("undefined")),
                code::Parameter::new("trace").typed("number").with_default(Expr::number(0)),
            ],
        };
        let mut parameters = match lang {
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
        parameters.extend(seed.clone());
        let mut function = code::Function::new("_execute", parameters);
        function.declare("root", scope_expr(&trace.name, "task", Expr::Null))?;
        function.body.append(&mut body);
        let mut parameters = function.parameters[..2].to_vec();
        parameters.extend(seed);
        let details_name = if lang == ScorerLang::Python {
            "run_details"
        } else {
            "runDetails"
        };
        let invoke = |name: &str, traced: bool| {
            let random = Expr::Name(random_name(lang).into());
            if lang == ScorerLang::Python {
                let mut keywords = if traced {
                    vec![("random_source".into(), random), ("logger".into(), "logger".into())]
                } else {
                    vec![("random_source".into(), random)]
                };
                if seeded {
                    keywords.push(("seed".into(), "seed".into()));
                    keywords.push(("trace".into(), "trace".into()));
                }
                Expr::Call {
                    function: Box::new(name.into()),
                    args: vec!["input".into()],
                    keywords,
                }
            } else {
                let mut args = if traced {
                    vec!["input".into(), random, "logger".into()]
                } else {
                    vec!["input".into(), random]
                };
                if seeded {
                    // the seed follows the logger where there is one
                    if !traced && name == "_execute" {
                        args.push(Expr::Null);
                    }
                    args.push("seed".into());
                    args.push("trace".into());
                }
                Expr::call(name, args)
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
            match native::functions(&mut functions, lang, &numbered, &helpers) {
                Some(contracts) => contracts,
                None => return emit_form(model, trace, output, lang, false, told, unsupported),
            }
        } else {
            vec![]
        };
        if seeded {
            let seeded = |source: Expr| Expr::call("seeded", vec![source, "seed".into(), "trace".into()]);
            let mut given = false;
            let execute = (functions.iter_mut())
                .find(|function| function.name == "_execute")
                .ok_or("the agent has no entry function")?;
            for statement in &mut execute.body {
                if let Statement::Variable(Variable::Declare {
                    value: Expr::New { class, args },
                    ..
                }) = statement
                    && class == "ExecutionContext"
                    && let Some(source) = args.get_mut(1)
                {
                    *source = seeded(source.clone());
                    given = true;
                }
            }
            // without a context the steps read the random source by name
            if !given {
                let source = Expr::Name(random_name(lang).into());
                execute.body.insert(
                    0,
                    Statement::Variable(Variable::Assign {
                        name: source.clone(),
                        value: seeded(source),
                    }),
                );
            }
        }
        Ok(Agent {
            helpers,
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
        scope_holding(fields_value, bindings, &[], children, frame, ctx, lang, root)
    }
    // `held` are values this scope declares for the scope above and keeps for it to read
    #[allow(clippy::too_many_arguments)]
    fn scope_holding(
        fields_value: Option<&SpanFields>,
        own: &[Binding],
        held: &[Binding],
        children: &[Child],
        frame: &str,
        ctx: &Context<'_>,
        lang: ScorerLang,
        root: bool,
    ) -> Result<Vec<Statement>, String> {
        let bindings: Vec<Binding> = own.iter().chain(held).cloned().collect();
        let bindings = bindings.as_slice();
        let first = operations(fields_value, bindings, held, children, frame, ctx, lang, root, &[])?;
        if !ctx.direct {
            return Ok(first.into_iter().flat_map(|op| op.code).collect());
        }
        let reads: Vec<_> = first.iter().map(|op| (op.key.clone(), op.reads.clone())).collect();
        let unordered = match crate::mocking::code::planning::order(first, frame) {
            Ok(ordered) => return Ok(ordered),
            Err(unordered) => unordered,
        };
        // A value that reads one step, and that the step reads back, has no place beside the step.
        // It is declared inside the step and read from what the step returns.
        let local = |binding: &Binding| {
            ctx.local_names
                .iter()
                .find(|(name, _)| *name == binding.name)
                .unwrap()
                .1
                .clone()
        };
        let read_by = |name: &str| {
            reads
                .iter()
                .find(|(key, _)| *key == Dependency::Variable(name.to_owned()))
                .map(|(_, reads)| reads)
        };
        let keys: Vec<_> = children
            .iter()
            .map(|child| ctx.names(child_node(child)).state.clone())
            .collect();
        let mut homes = vec![None::<usize>; bindings.len()];
        loop {
            let mut changed = false;
            for (index, binding) in bindings.iter().enumerate() {
                if homes[index].is_some() {
                    continue;
                }
                let Some(reads) = read_by(&local(binding)) else { continue };
                let mut found = HashSet::new();
                for read in reads {
                    match read {
                        Dependency::Child(parent, key) if parent == frame => {
                            found.extend(keys.iter().position(|found| found == key));
                        }
                        Dependency::Variable(name) => {
                            let other = bindings.iter().position(|other| local(other) == *name);
                            found.extend(other.and_then(|other| homes[other]));
                        }
                        _ => {}
                    }
                }
                if found.len() == 1 {
                    homes[index] = found.into_iter().next();
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if homes.iter().all(Option::is_none) {
            return Err(unordered);
        }
        let mut moved_ctx = ctx.clone();
        let mut moved = Vec::new();
        let mut kept = Vec::new();
        for (binding, home) in bindings.iter().zip(&homes) {
            match home {
                Some(home) => {
                    let holder = child_node(&children[*home]);
                    moved_ctx.moved.push((binding.name.clone(), holder, ctx.site));
                    moved.push((holder, binding.clone()));
                }
                None => kept.push(binding.clone()),
            }
        }
        let mut second = operations(fields_value, &kept, held, children, frame, &moved_ctx, lang, root, &moved)?;
        // a step hands back only what something outside it reads, the entry scope keeps everything for the result
        if !root {
            let first_child = second.len() - children.len();
            for (holder, binding) in &moved {
                if held.iter().any(|held| held.name == binding.name) {
                    continue;
                }
                let name = local(binding);
                let own = children
                    .iter()
                    .position(|child| child_node(child) == *holder)
                    .map(|index| first_child + index);
                let mut read = false;
                for (index, operation) in second.iter().enumerate() {
                    if Some(index) == own {
                        continue;
                    }
                    code::rewrite(&mut operation.code.clone(), &mut |found| {
                        read |= matches!(found, Expr::Read { dependency: Dependency::Field(_, key), .. } if *key == name);
                    });
                }
                if !read {
                    moved_ctx.unkept.push(binding.name.clone());
                }
            }
            if !moved_ctx.unkept.is_empty() {
                second = operations(fields_value, &kept, held, children, frame, &moved_ctx, lang, root, &moved)?;
            }
        }
        crate::mocking::code::planning::order(second, frame)
    }
    #[allow(clippy::too_many_arguments)]
    fn operations(
        fields_value: Option<&SpanFields>,
        bindings: &[Binding],
        held: &[Binding],
        children: &[Child],
        frame: &str,
        ctx: &Context<'_>,
        lang: ScorerLang,
        root: bool,
        moved: &[(NodeId, Binding)],
    ) -> Result<Vec<Operation>, String> {
        let mut operations = Vec::new();
        for binding in held {
            if ctx.unkept.contains(&binding.name) {
                continue;
            }
            // kept for the scope above, wherever below this scope the value is declared
            let name = &ctx.local_names.iter().find(|(name, _)| *name == binding.name).unwrap().1;
            let value = expression(&Value::VarRef(binding.name.clone()), ctx, lang)?;
            operations.push(Operation {
                key: Dependency::Field(frame.into(), name.clone()),
                reads: value.references(),
                code: vec![assign(Expr::from(frame).member("fields").index(Expr::string(name)), value)],
            });
        }
        for binding in bindings {
            if ctx.aliases.iter().any(|(alias, _)| *alias == binding.name) {
                continue;
            }
            let name = &ctx.local_names.iter().find(|(name, _)| *name == binding.name).unwrap().1;
            let value = expression(&binding.value, ctx, lang)?;
            operations.push(Operation {
                key: Dependency::Variable(name.clone()),
                reads: value.references(),
                code: vec![Statement::Variable(Variable::constant(name, value.lazy()))],
            });
            // a value another scope reads is kept on the frame that declares it
            let shared = !held.iter().any(|held| held.name == binding.name)
                && (ctx.model.refs.iter())
                    .any(|reference| matches!(&reference.accessor, Accessor::Binding(found) if *found == binding.name));
            if shared {
                operations.push(Operation {
                    key: Dependency::Field(frame.into(), name.clone()),
                    reads: HashSet::from([Dependency::Variable(name.clone())]),
                    code: vec![assign(
                        Expr::from(frame).member("fields").index(Expr::string(name)),
                        if ctx.direct {
                            Expr::call(name.as_str(), vec![])
                        } else {
                            Expr::Name(name.clone())
                        },
                    )],
                });
            }
        }
        if let Some(fields_value) = fields_value {
            for (key, mut value) in fields(fields_value, ctx, lang, root)? {
                // an entry that another entry of the same object reads is computed first, under its own name
                let own = Dependency::Field(frame.into(), key.clone());
                if ctx.direct
                    && value.references().contains(&own)
                    && let Expr::Object(entries) = &mut value
                {
                    let mut wanted = Vec::new();
                    for (_, entry) in entries.iter_mut() {
                        code::rewrite_expr(entry, &mut |found| {
                            if let Expr::Call { function, args, .. } = found
                                && matches!(&**function, Expr::Name(name) if name == "_index")
                                && let [Expr::Read { dependency, .. }, Expr::String(part)] = args.as_slice()
                                && *dependency == own
                            {
                                let name = if lang == ScorerLang::Python {
                                    format!("{key}_{part}")
                                } else {
                                    let mut name = key.clone();
                                    for word in part.split('_').filter(|word| !word.is_empty()) {
                                        name.push_str(&word[..1].to_uppercase());
                                        name.push_str(&word[1..]);
                                    }
                                    name
                                };
                                // a name already in use leaves the read as it was, and the scope unordered
                                if !ctx.local_names.iter().any(|(_, local)| *local == name) {
                                    wanted.push((part.clone(), name.clone()));
                                    *found = Expr::Name(name);
                                }
                            }
                        });
                    }
                    for (part, name) in wanted {
                        let Some((_, entry)) = entries.iter_mut().find(|(entry, _)| *entry == part) else {
                            continue;
                        };
                        if matches!(entry, Expr::Name(found) if *found == name) {
                            continue;
                        }
                        let computed = std::mem::replace(entry, Expr::Name(name.clone()));
                        operations.push(Operation {
                            key: Dependency::Variable(name.clone()),
                            reads: computed.references(),
                            code: vec![Statement::Variable(Variable::constant(&name, computed))],
                        });
                    }
                }
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
        let mut previous = None;
        for child in children {
            let node = child_node(child);
            let held: Vec<_> = (moved.iter())
                .filter(|(holder, _)| *holder == node)
                .map(|(_, binding)| binding.clone())
                .collect();
            let mut operation = child_operation(child, &held, frame, ctx, lang)?;
            if ctx.model.ordered
                && let Some(previous) = previous
            {
                operation.reads.insert(previous);
            }
            previous = Some(operation.key.clone());
            operations.push(operation);
        }
        Ok(operations)
    }
    fn child_operation(
        child: &Child,
        held: &[Binding],
        parent: &str,
        ctx: &Context<'_>,
        lang: ScorerLang,
    ) -> Result<Operation, String> {
        let node = child_node(child);
        let names = ctx.names(node);
        let frame = &names.state;
        let mut child_ctx = ctx.clone();
        child_ctx.site = node;
        child_ctx.frames.push((node, frame.clone()));
        let empty = Vec::new();
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
            Child::Scorer(v) => (v.name.as_str(), "scorer", &v.bindings, &empty, None),
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
        if matches!(child, Child::Choice(_)) && !held.is_empty() {
            return Err("a value cannot be held by a choice of paths".into());
        }
        // an optional path declares what it holds where the path runs
        let outer_held = if branching { &[] } else { held };
        let mut work = scope_holding(
            fields,
            bindings,
            outer_held,
            if branching { &[] } else { children },
            frame,
            &child_ctx,
            lang,
            false,
        )?;
        match child {
            Child::Scorer(v) => {
                work.push(Statement::Evaluate(Expr::call(
                    "_score_span",
                    vec![
                        frame.clone().into(),
                        parent.into(),
                        Expr::string(&v.name),
                        expression(&v.score, &child_ctx, lang)?,
                        v.reason
                            .as_ref()
                            .map(|(value, _)| expression(value, &child_ctx, lang))
                            .transpose()?
                            .unwrap_or(Expr::Null),
                    ],
                )));
            }
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
            Child::Repeat(v) => {
                if let Some(again) = &v.again {
                    let condition = expression(&again.condition, &child_ctx, lang)?;
                    work.push(assign(
                        Expr::from(frame.clone()).member("fields").index(Expr::string("again")),
                        if ctx.direct {
                            Expr::call("_boolean", vec![condition])
                        } else {
                            condition.lazy()
                        },
                    ));
                }
            }
            Child::Maybe(v) => {
                work.push(assign(
                    Expr::from(frame.clone()).member("fields").index(Expr::string("included")),
                    Expr::call("_include", vec![expression(&v.chance, ctx, lang)?, random_name(lang).into()]),
                ));
                work.push(Statement::If {
                    condition: Expr::call("_boolean", vec![read_field(frame.clone().into(), "included", ctx.direct)]),
                    then_body: scope_holding(None, &[], held, children, frame, &child_ctx, lang, false)?,
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
            match &v.again {
                // a loop is known to its parent before its rounds run, so a round can read the one before it
                Some(again) => Expr::call(
                    "_loop",
                    vec![
                        parent.into(),
                        Expr::string(frame),
                        names.function.clone().into(),
                        Expr::string(name),
                        match &again.limit {
                            Some(limit) => expression(limit, ctx, lang)?,
                            None => Expr::Null,
                        },
                    ],
                ),
                None => Expr::call(
                    "_repeat",
                    vec![
                        expression(&v.count, ctx, lang)?,
                        names.function.clone().into(),
                        Expr::string(name),
                    ],
                ),
            }
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
        // a loop reads its own earlier rounds, which is not a dependency on itself
        let key = Dependency::Child(parent.into(), frame.clone());
        let mut reads = code::references(&code);
        reads.remove(&key);
        Ok(Operation { key, reads, code })
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
        Product(Box<Step>),
        Value,
        Optional(Box<Type>),
        Null,
        Bool,
        Integer(i128, i128),
        // a finite number within bounds, whole or not
        Real(f64, f64),
        Number,
        String,
        // a list written item by item, where a `Rest` stands for any number of items
        Array(Vec<Type>),
        Rest(Box<Type>),
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
        // the steps inside a step that are read from outside it
        nested: HashMap<String, Vec<String>>,
        // what each step that ran here hands back
        steps: HashMap<String, Type>,
        // what decides whether a scope runs, by its frame
        gates: HashMap<String, Expr>,
        // the records a scope is sure to find filled when it runs, by its frame
        guarded: HashMap<String, Vec<String>>,
        // the records whose steps are sure to have run
        present: HashSet<String>,
        collections: HashMap<String, (Expr, Branch)>,
        input: Option<Type>,
        types: std::rc::Rc<std::cell::RefCell<HashMap<String, Type>>>,
        // what the provider helpers return
        counts: Counts,
    }
    type Counts = std::rc::Rc<Results>;
    // the provider helpers that return a count, and the operations of a helper that return text
    #[derive(Default)]
    struct Results {
        counts: HashSet<&'static str>,
        texts: HashSet<(&'static str, &'static str)>,
        // the provider helpers that return plain data
        plain: HashSet<&'static str>,
        // every helper the agent calls, for what each says of its own calls
        helpers: Vec<crate::mocking::Helper>,
    }
    impl Results {
        // the helper a call is to, and the arguments the function it serves was called with
        fn called<'a>(&self, name: &str, args: &'a [Expr]) -> Option<(&crate::mocking::Helper, &'a [Expr])> {
            let source = |value: &Expr| {
                matches!(value, Expr::Name(name) if name == "random_source" || name == "randomSource")
                    || matches!(value, Expr::Member(target, field) if field == "random" && matches!(&**target, Expr::Name(name) if name == "context"))
            };
            self.helpers.iter().find_map(|helper| {
                if helper.name != name {
                    return None;
                }
                // the random source follows the arguments
                let given = match args.split_last() {
                    Some((last, given)) if helper.random && source(last) => given,
                    _ if helper.random => return None,
                    _ => args,
                };
                match helper.operation {
                    None => Some((helper, given)),
                    Some(operation) => match given {
                        [Expr::String(found), Expr::Array(given)] if found == operation => Some((helper, given.as_slice())),
                        _ => None,
                    },
                }
            })
        }
    }
    const SAFE: i128 = 9_007_199_254_740_991;
    // the bounds of a number that has them
    fn span(kind: &Type) -> Option<(f64, f64)> {
        match kind {
            Type::Integer(low, high) => Some((*low as f64, *high as f64)),
            Type::Real(low, high) => Some((*low, *high)),
            _ => None,
        }
    }
    // whether a number stays where both languages hold it exactly
    fn safe(kind: &Type) -> bool {
        span(kind).is_some_and(|(low, high)| low >= -(SAFE as f64) && high <= SAFE as f64)
    }
    // what a helper is told of a value, which a written value says more of than its type
    fn fact(value: &Expr, kind: &Type) -> crate::mocking::Known {
        use crate::mocking::Known;
        match (value, kind) {
            (Expr::String(text), _) => Known::Text(Some(text.clone())),
            (Expr::Number(_) | Expr::Call { .. }, _) if written(value).is_some() => {
                let number = written(value).unwrap();
                Known::Number {
                    low: number,
                    high: number,
                    whole: matches!(value, Expr::Number(text) if text.parse::<i128>().is_ok()),
                }
            }
            (Expr::Array(items), Type::Array(kinds))
                if items.len() == kinds.len() && !items.iter().any(|item| matches!(item, Expr::Spread(_))) =>
            {
                Known::Items(items.iter().zip(kinds).map(|(item, kind)| fact(item, kind)).collect())
            }
            (Expr::Object(fields), Type::Object(kinds)) => Known::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), fact(value, kinds.get(key).unwrap_or(&Type::Unknown))))
                    .collect(),
            ),
            (_, kind) => told(kind),
        }
    }
    fn told(kind: &Type) -> crate::mocking::Known {
        use crate::mocking::Known;
        match kind {
            Type::Unknown | Type::Deferred(_) | Type::Span(..) | Type::Record(_) | Type::Product(_) => Known::Unknown,
            Type::Value => Known::Plain,
            Type::Null => Known::Null,
            Type::Bool => Known::Bool,
            Type::String => Known::Text(None),
            Type::Integer(low, high) => Known::whole(*low as f64, *high as f64),
            Type::Real(low, high) => Known::Number {
                low: *low,
                high: *high,
                whole: false,
            },
            Type::Number => Known::NUMBER,
            Type::Array(items) => Known::Items(items.iter().map(told).collect()),
            Type::Rest(item) => Known::Run(Box::new(told(item))),
            Type::List(item) => Known::List(Box::new(told(item))),
            Type::Object(fields) => Known::Object(fields.iter().map(|(key, kind)| (key.clone(), told(kind))).collect()),
            Type::Optional(inner) => Known::Maybe(Box::new(told(inner))),
        }
    }
    // the type of what a helper says it returns
    fn learned(known: &crate::mocking::Known) -> Type {
        use crate::mocking::Known;
        match known {
            Known::Unknown => Type::Unknown,
            Known::Plain => Type::Value,
            Known::Null => Type::Null,
            Known::Bool => Type::Bool,
            Known::Text(_) => Type::String,
            Known::Number { low, high, whole } => {
                let held = low.is_finite() && high.is_finite() && low.abs() < 1e300 && high.abs() < 1e300;
                match (held, whole) {
                    (true, true) if low.abs() < 1e30 && high.abs() < 1e30 => Type::Integer(*low as i128, *high as i128),
                    (true, _) => Type::Real(*low, *high),
                    _ => Type::Number,
                }
            }
            Known::Items(items) => Type::Array(items.iter().map(learned).collect()),
            Known::Run(item) => Type::Rest(Box::new(learned(item))),
            Known::List(item) => Type::List(Box::new(learned(item))),
            Known::Object(fields) => Type::Object(fields.iter().map(|(key, known)| (key.clone(), learned(known))).collect()),
            Known::Maybe(inner) => match learned(inner) {
                Type::Unknown => Type::Unknown,
                inner => Type::Optional(Box::new(inner)),
            },
        }
    }
    #[derive(Clone, Debug)]
    struct Branch {
        span: Type,
        fields: HashMap<String, Type>,
        children: HashMap<String, (String, Type)>,
        // what decided whether the scope ran
        gate: Option<Expr>,
    }
    // what a step hands back, which is its output alone unless something else of it is read
    #[derive(Clone, Debug)]
    struct Step {
        output: Type,
        fields: HashMap<String, Type>,
        children: HashMap<String, (String, Type)>,
    }
    impl Step {
        fn plain(output: Type) -> Self {
            Step {
                output,
                fields: HashMap::new(),
                children: HashMap::new(),
            }
        }
        fn bundled(&self) -> bool {
            self.fields.keys().any(|key| key != "output") || !self.children.is_empty()
        }
        fn shape(&self) -> Type {
            if !self.bundled() {
                return self.output.clone();
            }
            Type::Object(
                (self.fields.iter())
                    .map(|(key, kind)| (export_name(key).to_string(), kind.clone()))
                    .chain(self.children.values().cloned())
                    .collect(),
            )
        }
    }
    impl Branch {
        // a step that hands nothing back has no place in what its scope returns
        fn silent(&self, kind: &Type) -> bool {
            !self.fields.is_empty() && matches!(kind, Type::Null)
        }
        fn results(&self) -> impl Iterator<Item = &(String, Type)> {
            self.children.values().filter(|(_, kind)| !self.silent(kind))
        }
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
        span: Type,
        step: Step,
    }
    // recorded among the types when a plan has no ordered form after all
    const UNPRINTABLE: &str = "!unprintable";
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
    pub(crate) fn functions(
        functions: &mut Vec<code::Function>,
        lang: ScorerLang,
        numbered: &[(String, String)],
        helpers: &[crate::mocking::Helper],
    ) -> Option<Vec<(String, Json)>> {
        let counts: Counts = std::rc::Rc::new(Results {
            counts: (helpers.iter())
                .filter(|helper| helper.counts)
                .map(|helper| helper.name)
                .collect(),
            texts: (helpers.iter())
                .filter(|helper| helper.texts)
                .filter_map(|helper| Some((helper.name, helper.operation?)))
                .collect(),
            plain: (helpers.iter())
                .filter(|helper| helper.plain)
                .map(|helper| helper.name)
                .collect(),
            helpers: helpers.to_vec(),
        });
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
        // A value read only by one that moved follows it, so this runs until nothing moves.
        fn localize(body: &mut Vec<Statement>) {
            while localize_once(body) {}
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
        fn localize_once(body: &mut Vec<Statement>) -> bool {
            let mut moved = false;
            // moved values keep the order they were declared in, and those moved later are read by those moved before
            let mut placed: HashMap<String, usize> = HashMap::new();
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
                let place = placed.entry(consumer.clone()).or_default();
                if let Statement::Function(f) = &mut body[index]
                    && let Statement::TraceScope { body, .. } = &mut f.body[1]
                {
                    body.insert(*place, declaration);
                    *place += 1;
                }
                body.retain(|s| !matches!(s, Statement::Variable(Variable::Declare { name: n, .. }) if n == &name));
                moved = true;
            }
            moved
        }
        // a deferred value nothing reads is never computed, so it is not printed
        fn unread(body: &mut Vec<Statement>, read: &HashSet<String>) {
            body.retain(|statement| {
                !matches!(statement, Statement::Variable(Variable::Declare { name, value: Expr::Call { function, .. }, .. })
                    if matches!(&**function, Expr::Name(callee) if callee == "_lazy") && !read.contains(name))
            });
            for statement in body {
                match statement {
                    Statement::Function(function) => unread(&mut function.body, read),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => unread(body, read),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        unread(then_body, read);
                        unread(else_body, read);
                    }
                    _ => {}
                }
            }
        }
        loop {
            let mut read = HashSet::new();
            let mut declared = 0;
            for function in functions.iter_mut() {
                code::rewrite(&mut function.body, &mut |value| {
                    if let Expr::Name(name) = value {
                        read.insert(name.clone());
                    }
                });
            }
            let count = |functions: &[code::Function]| {
                let mut total = 0;
                for function in functions {
                    let mut body = function.body.clone();
                    code::rewrite(&mut body, &mut |value| {
                        if matches!(value, Expr::Call { function, .. } if matches!(&**function, Expr::Name(name) if name == "_lazy"))
                        {
                            total += 1;
                        }
                    });
                }
                total
            };
            declared += count(functions);
            for function in functions.iter_mut() {
                unread(&mut function.body, &read);
            }
            if count(functions) == declared {
                break;
            }
        }
        for function in functions.iter_mut() {
            localize(&mut function.body);
        }
        fn exported_fields(
            body: &[Statement],
            ancestors: &[String],
            exports: &mut HashMap<String, Vec<String>>,
            nested: &mut HashMap<String, Vec<String>>,
        ) {
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
                        exported_fields(&function.body, &ancestors, exports, nested);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => {
                        exported_fields(body, ancestors, exports, nested)
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        exported_fields(then_body, ancestors, exports, nested);
                        exported_fields(else_body, ancestors, exports, nested);
                    }
                    other => code::rewrite(&mut [other.clone()], &mut |value| {
                        let (found, frame, key) = match value {
                            Expr::Read {
                                dependency: Dependency::Field(frame, field),
                                ..
                            } => (&mut *exports, frame, field),
                            Expr::Read {
                                dependency: Dependency::Child(frame, child),
                                ..
                            } => (&mut *nested, frame, child),
                            _ => return,
                        };
                        let frame = frame.strip_suffix("_selected").unwrap_or(frame).to_string();
                        if !ancestors.contains(&frame) {
                            let keys = found.entry(frame).or_default();
                            if !keys.contains(key) {
                                keys.push(key.clone());
                                keys.sort();
                            }
                        }
                    }),
                }
            }
        }
        let mut exports = HashMap::new();
        let mut nested = HashMap::new();
        for function in functions.iter() {
            exported_fields(&function.body, &["root".into()], &mut exports, &mut nested);
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
                nested: nested.clone(),
                input: input_type.clone(),
                types: types.clone(),
                counts: counts.clone(),
                ..Environment::default()
            };
            // what a caller passes in is plain data
            env.names.insert("input".into(), Type::Value);
            function.body = block(std::mem::take(&mut function.body), &mut env, lang, None);
        }
        if types.borrow().contains_key(UNPRINTABLE) {
            return None;
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
        eager_values(functions, &types.borrow(), &counts, lang);
        settle(functions, &types, &counts, lang);
        pure_functions(functions);
        hoist(functions, lang);
        let mut contracts = Vec::new();
        if let Some(schema) = &input_schema {
            contracts.push(("AgentInput".into(), schema.clone()));
        }
        annotate(functions, &types.borrow(), &counts, &mut contracts, lang);
        expand_results(functions, lang, &types.borrow(), &counts);
        local_names(functions, lang, numbered);
        native_locals(functions, &types.borrow(), &contracts, &counts, lang);
        if lang == ScorerLang::Typescript {
            written_durations(functions);
        }
        idioms(functions, lang);
        request_parameters(functions, lang);
        aliases(functions);
        arithmetic_steps(functions, lang);
        silent_steps(functions, lang);
        // A step reached along more than one path is one function, however many places call it.
        loop {
            let same = |a: &code::Function, b: &code::Function| {
                let shape = |function: &code::Function| format!("{:?}", (&function.parameters, &function.body));
                !a.exported && !b.exported && !a.name.starts_with('_') && !b.name.starts_with('_') && shape(a) == shape(b)
            };
            let mut merged = None;
            'search: for first in 0..functions.len() {
                for second in first + 1..functions.len() {
                    if same(&functions[first], &functions[second]) {
                        merged = Some((functions[first].name.clone(), second));
                        break 'search;
                    }
                }
            }
            let Some((kept, dropped)) = merged else { break };
            let dropped = functions.remove(dropped).name;
            for function in functions.iter_mut() {
                code::rewrite(&mut function.body, &mut |value| {
                    if matches!(value, Expr::Name(name) if *name == dropped) {
                        *value = Expr::Name(kept.clone());
                    }
                });
            }
        }
        // A step that only holds values does nothing when it runs, since its values are computed
        // where they are read. Its function and the calls nobody reads the result of are dropped.
        fn holds_values(function: &code::Function) -> bool {
            matches!(
                function.body.as_slice(),
                [
                    Statement::Variable(Variable::Declare { name, value: Expr::Call { function: callee, args, keywords }, .. }),
                    Statement::If { condition: Expr::Name(tested), then_body, else_body },
                    Statement::Return(Expr::Object(fields)),
                ] if matches!(&**callee, Expr::Member(_, member) if member == "chance")
                    && matches!(args.as_slice(), [Expr::Number(always)] if always == "1")
                    && keywords.is_empty()
                    && tested == name
                    && then_body.is_empty()
                    && else_body.is_empty()
                    && matches!(fields.as_slice(), [(_, Expr::Name(returned))] if returned == name)
            )
        }
        fn drop_calls(body: &mut Vec<Statement>, holder: &str, read: &HashSet<String>) {
            body.retain(|statement| {
                !matches!(
                    statement,
                    Statement::Variable(Variable::Declare { name, value: Expr::Call { function, .. }, .. })
                        if matches!(&**function, Expr::Name(called) if called == holder) && !read.contains(name)
                )
            });
            for statement in body.iter_mut() {
                match statement {
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        drop_calls(then_body, holder, read);
                        drop_calls(else_body, holder, read);
                    }
                    Statement::ForRange { body, .. } | Statement::TraceScope { body, .. } => drop_calls(body, holder, read),
                    _ => {}
                }
            }
        }
        let holders: Vec<String> = (functions.iter())
            .filter(|function| !function.exported && !function.name.starts_with('_') && holds_values(function))
            .map(|function| function.name.clone())
            .collect();
        for holder in holders {
            for function in functions.iter_mut() {
                // a result is read when its name appears anywhere but where it is declared
                let mut read = HashSet::new();
                code::rewrite(&mut function.body.clone(), &mut |value| {
                    if let Expr::Name(name) = value {
                        read.insert(name.clone());
                    }
                });
                drop_calls(&mut function.body, &holder, &read);
            }
            let called = functions.iter().any(|function| {
                function.name != holder && code::references(&function.body).contains(&Dependency::Variable(holder.clone()))
            });
            if !called {
                // the result type goes with the function, unless something else still names it
                let result = (functions.iter())
                    .find(|function| function.name == holder)
                    .and_then(|function| function.returns.clone());
                functions.retain(|function| function.name != holder);
                if let Some(result) = result {
                    let named = functions.iter().any(|function| {
                        function.returns.as_deref() == Some(result.as_str())
                            || format!("{:?}", (&function.parameters, &function.body)).contains(&format!("\"{result}\""))
                    });
                    if !named {
                        contracts.retain(|(name, _)| *name != result);
                    }
                }
            }
        }
        // A parameter its function never reads is not passed.
        fn inert(value: &Expr) -> bool {
            match value {
                Expr::Name(_) | Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Index(target, index) => inert(target) && inert(index),
                Expr::Member(target, _) => inert(target),
                Expr::Binary { left, right, .. } => inert(left) && inert(right),
                _ => false,
            }
        }
        loop {
            let mut unused = None;
            'search: for function in functions.iter() {
                if function.exported || function.name.starts_with('_') {
                    continue;
                }
                let mut read = HashSet::new();
                code::rewrite(&mut function.body.clone(), &mut |value| {
                    if let Expr::Name(name) = value {
                        read.insert(name.clone());
                    }
                });
                for (index, parameter) in function.parameters.iter().enumerate() {
                    if !parameter.members.is_empty() || parameter.keyword_only || read.contains(&parameter.name) {
                        continue;
                    }
                    // every caller must pass something that costs nothing to leave out
                    let mut removable = true;
                    for caller in functions.iter() {
                        code::rewrite(&mut caller.body.clone(), &mut |value| match value {
                            Expr::Call {
                                function: callee,
                                args,
                                keywords,
                            } if matches!(&**callee, Expr::Name(name) if *name == function.name) => {
                                removable &= keywords.is_empty() && args.get(index).is_some_and(inert);
                            }
                            _ => {}
                        });
                    }
                    // a function handed on by name is called with arguments this pass cannot see
                    let mut calls = 0;
                    let mut mentions = 0;
                    for caller in functions.iter() {
                        code::rewrite(&mut caller.body.clone(), &mut |value| match value {
                            Expr::Call { function: callee, .. } if matches!(&**callee, Expr::Name(name) if *name == function.name) =>
                            {
                                calls += 1;
                            }
                            Expr::Name(name) if *name == function.name => mentions += 1,
                            _ => {}
                        });
                    }
                    if removable && mentions == calls {
                        unused = Some((function.name.clone(), index));
                        break 'search;
                    }
                }
            }
            let Some((name, index)) = unused else { break };
            for function in functions.iter_mut() {
                if function.name == name {
                    function.parameters.remove(index);
                }
                code::rewrite(&mut function.body, &mut |value| {
                    if let Expr::Call {
                        function: callee, args, ..
                    } = value
                        && matches!(&**callee, Expr::Name(callee) if *callee == name)
                        && index < args.len()
                    {
                        args.remove(index);
                    }
                });
            }
        }
        // a helper nothing calls any more is not printed
        loop {
            let called: HashSet<_> = functions
                .iter()
                .flat_map(|function| code::references(&function.body))
                .collect();
            let before = functions.len();
            functions.retain(|function| {
                function.exported
                    || function.name.starts_with('_')
                    || called.contains(&Dependency::Variable(function.name.clone()))
            });
            if functions.len() == before {
                break;
            }
        }
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
            return Some(contracts);
        }
        Some(contracts)
    }
    fn type_schema(kind: &Type) -> Json {
        match kind {
            Type::Null => json!({"type":"null"}),
            Type::Bool => json!({"type":"boolean"}),
            Type::String => json!({"type":"string"}),
            Type::Integer(..) | Type::Real(..) | Type::Number => json!({"type":"number"}),
            Type::Optional(inner) => json!({"anyOf":[type_schema(inner),{"type":"null"}]}),
            Type::Object(fields) => {
                json!({"type":"object", "properties": fields.iter().map(|(k,v)|(k.clone(),type_schema(v))).collect::<serde_json::Map<_,_>>()})
            }
            Type::Product(step) => type_schema(&step.shape()),
            Type::Record(branch) => {
                if branch.fields.is_empty() && branch.children.len() == 1 {
                    return type_schema(&branch.children.values().next().unwrap().1);
                }
                let mut fields = branch.fields.clone();
                fields.extend(branch.results().cloned());
                type_schema(&Type::Object(fields))
            }
            Type::List(item) => json!({"type":"array","items":type_schema(item)}),
            Type::Rest(item) => type_schema(item),
            Type::Array(items) => {
                json!({"type":"array","items":type_schema(&items.iter().cloned().map(one).reduce(join).unwrap_or(Type::Unknown))})
            }
            _ => json!({}),
        }
    }
    fn annotate(
        functions: &mut [code::Function],
        types: &HashMap<String, Type>,
        counts: &Counts,
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
                Type::Real(..) | Type::Number => {
                    return if lang == ScorerLang::Python { "int | float" } else { "number" }.into();
                }
                Type::Bool => return if lang == ScorerLang::Python { "bool" } else { "boolean" }.into(),
                Type::Span(..) => return "Span".into(),
                // nothing is not worth a name
                Type::Null => return if lang == ScorerLang::Python { "None" } else { "null" }.into(),
                Type::Unknown | Type::Value => {
                    return if lang == ScorerLang::Python { "Any" } else { "unknown" }.into();
                }
                Type::Deferred(_) => {
                    return if lang == ScorerLang::Python { "Any" } else { "any" }.into();
                }
                // a value that may be absent is its type or null, so the type alone takes the name
                Type::Optional(inner) => {
                    let inner = annotation(inner, label, contracts, lang);
                    return match (inner.as_str(), lang) {
                        ("Any" | "any" | "unknown", _) => inner,
                        (_, ScorerLang::Python) => format!("{inner} | None"),
                        (_, ScorerLang::Typescript) => format!("{inner} | null"),
                    };
                }
                _ => {}
            }
            // a list of strings or numbers is written out, since a name for it says no more
            let item = match kind {
                Type::List(item) => Some((**item).clone()),
                Type::Array(items) => items.iter().cloned().map(one).reduce(join),
                _ => None,
            };
            if let Some(item @ (Type::String | Type::Bool | Type::Integer(..) | Type::Real(..) | Type::Number)) = item {
                let item = annotation(&item, label, contracts, lang);
                return if lang == ScorerLang::Python {
                    format!("list[{item}]")
                } else {
                    format!("Array<{item}>")
                };
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
        // An object a helper builds has the shape its call gives it, which the helper cannot say
        // for itself. The name that takes the result says it.
        fn shaped(
            body: &mut [Statement],
            types: &HashMap<String, Type>,
            counts: &Counts,
            contracts: &mut Vec<(String, Json)>,
            lang: ScorerLang,
        ) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Call { function, .. },
                        annotation: found @ None,
                        ..
                    }) => {
                        if let Expr::Name(callee) = &**function
                            && (counts.helpers.iter())
                                .any(|helper| helper.name == callee && helper.plain && helper.returns.is_some())
                            && let Some(kind @ Type::Object(_)) = types.get(name)
                        {
                            *found = Some(annotation(kind, name, contracts, lang));
                        }
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => {
                        shaped(body, types, counts, contracts, lang)
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        shaped(then_body, types, counts, contracts, lang);
                        shaped(else_body, types, counts, contracts, lang);
                    }
                    _ => {}
                }
            }
        }
        for function in functions.iter_mut() {
            shaped(&mut function.body, types, counts, contracts, lang);
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

    // TypeScript marks a float so that 3.0 is not taken for 3. A duration is read as seconds,
    // where the two are the same, so one written as a float and read only as a duration is a number.
    fn written_durations(functions: &mut [code::Function]) {
        fn seconds(value: &Expr, name: &str) -> bool {
            matches!(value, Expr::Call { function, args, keywords }
                if matches!(&**function, Expr::Name(n) if n == "_duration")
                    && keywords.is_empty()
                    && matches!(args.as_slice(), [Expr::Name(n)] if n == name))
        }
        // how often a name is read, and how many of those reads are of a duration
        fn reads(body: &[Statement], name: &str, timed: &HashSet<(String, usize)>) -> (usize, usize) {
            let (mut all, mut durations) = (0, 0);
            code::rewrite(&mut body.to_vec(), &mut |value| match value {
                Expr::Name(n) if n == name => all += 1,
                value if seconds(value, name) => durations += 1,
                Expr::Call {
                    function,
                    args,
                    keywords,
                } if keywords.is_empty() => {
                    if let Expr::Name(callee) = &**function {
                        durations += (args.iter().enumerate())
                            .filter(|(index, arg)| {
                                matches!(arg, Expr::Name(n) if n == name) && timed.contains(&(callee.clone(), *index))
                            })
                            .count();
                    }
                }
                _ => {}
            });
            (all, durations)
        }
        fn written(body: &mut [Statement], names: &HashSet<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, value, .. }) if names.contains(name) => {
                        if let Expr::Call { args, .. } = value {
                            *value = args[0].clone();
                        }
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        written(then_body, names);
                        written(else_body, names);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => written(body, names),
                    _ => {}
                }
            }
        }
        fn floats(body: &[Statement], found: &mut Vec<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare {
                        name,
                        value: Expr::Call { function, args, .. },
                        mutable: false,
                        ..
                    }) if matches!(&**function, Expr::Name(n) if n == "_float")
                        && matches!(args.as_slice(), [Expr::Number(_)]) =>
                    {
                        found.push(name.clone());
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        floats(then_body, found);
                        floats(else_body, found);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => floats(body, found),
                    _ => {}
                }
            }
        }
        // the parameters a function reads only as a duration, itself or through a step it calls
        let mut timed = HashSet::new();
        loop {
            let before = timed.len();
            for function in functions.iter() {
                for (index, parameter) in function.parameters.iter().enumerate() {
                    let (all, durations) = reads(&function.body, &parameter.name, &timed);
                    if all > 0 && all == durations {
                        timed.insert((function.name.clone(), index));
                    }
                }
            }
            if timed.len() == before {
                break;
            }
        }
        for function in functions {
            let mut found = Vec::new();
            floats(&function.body, &mut found);
            let names: HashSet<String> = (found.into_iter())
                .filter(|name| {
                    let (all, durations) = reads(&function.body, name, &timed);
                    all > 0 && all == durations
                })
                .collect();
            written(&mut function.body, &names);
        }
    }

    // A step that hands nothing back returns nothing, and is called for what it does.
    fn silent_steps(functions: &mut [code::Function], lang: ScorerLang) {
        fn mentions(body: &[Statement], name: &str) -> usize {
            let mut count = 0;
            code::rewrite(&mut body.to_vec(), &mut |value| {
                if matches!(value, Expr::Name(n) if n == name) {
                    count += 1;
                }
            });
            count
        }
        fn calls(value: &Expr, name: &str) -> bool {
            matches!(value, Expr::Call { function, .. } if matches!(&**function, Expr::Name(n) if n == name))
        }
        // the calls made for what the step does, as a statement or under a name nothing reads
        fn statements(body: &[Statement], name: &str, read: &dyn Fn(&str) -> bool) -> usize {
            (body.iter())
                .map(|statement| match statement {
                    Statement::Evaluate(value) if calls(value, name) => 1,
                    Statement::Variable(Variable::Declare { name: held, value, .. }) if calls(value, name) && !read(held) => 1,
                    Statement::If {
                        then_body, else_body, ..
                    } => statements(then_body, name, read) + statements(else_body, name, read),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => statements(body, name, read),
                    _ => 0,
                })
                .sum()
        }
        fn run(body: &mut [Statement], name: &str, read: &dyn Fn(&str) -> bool) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name: held, value, .. }) if calls(value, name) && !read(held) => {
                        *statement = Statement::Evaluate(value.clone());
                    }
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        run(then_body, name, read);
                        run(else_body, name, read);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => run(body, name, read),
                    _ => {}
                }
            }
        }
        // whether every return hands back null, and there is one
        fn returns_nothing(body: &[Statement], found: &mut bool) -> bool {
            body.iter().all(|statement| match statement {
                Statement::Return(Expr::Null) => {
                    *found = true;
                    true
                }
                Statement::Return(_) => false,
                Statement::If {
                    then_body, else_body, ..
                } => returns_nothing(then_body, found) && returns_nothing(else_body, found),
                Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => returns_nothing(body, found),
                _ => true,
            })
        }
        fn drop_returns(body: &mut Vec<Statement>) {
            body.retain(|statement| !matches!(statement, Statement::Return(_)));
            for statement in body {
                match statement {
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        drop_returns(then_body);
                        drop_returns(else_body);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => drop_returns(body),
                    _ => {}
                }
            }
        }
        let nothing = if lang == ScorerLang::Python { "None" } else { "null" };
        let candidates: Vec<String> = (functions.iter())
            .filter(|function| {
                let mut found = false;
                !function.exported
                    && !function.name.starts_with('_')
                    && function.returns.as_deref() == Some(nothing)
                    // a return ends the function only where it is the last thing in it
                    && matches!(
                        function.body.last(),
                        Some(Statement::Return(Expr::Null)) | Some(Statement::TraceScope { .. })
                    )
                    && returns_nothing(&function.body, &mut found)
                    && found
            })
            .map(|function| function.name.clone())
            .collect();
        for name in candidates {
            let every = functions.iter().all(|function| {
                let read = |held: &str| mentions(&function.body, held) > 0;
                mentions(&function.body, &name) == statements(&function.body, &name, &read)
            });
            if !every {
                continue;
            }
            for function in functions.iter_mut() {
                let body = function.body.clone();
                let read = |held: &str| mentions(&body, held) > 0;
                run(&mut function.body, &name, &read);
                if function.name == name {
                    drop_returns(&mut function.body);
                    if lang == ScorerLang::Typescript {
                        function.returns = Some("void".into());
                    }
                }
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

    fn eager_values(functions: &mut [code::Function], types: &HashMap<String, Type>, counts: &Counts, lang: ScorerLang) {
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
            // a value whose first reader was itself deferred can be computed once that reader is
            while visit_once(body, env, lang) {}
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
        fn visit_once(body: &mut Vec<Statement>, env: &Environment, lang: ScorerLang) -> bool {
            let mut changed = false;
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
                let Some(mut first) = first else { continue };
                // A value that can fail is computed early only where its first reader always reads it.
                if !total(&value, env, lang) {
                    // a function that reads it does so when called, so the first statement that reads it decides
                    let reader = body.iter().position(|s| {
                        !matches!(s, Statement::Function(_))
                            && !matches!(s, Statement::Variable(Variable::Declare {name:n,..}) if n == &name)
                            && code::references(std::slice::from_ref(s)).contains(&Dependency::Variable(name.clone()))
                    });
                    let Some(reader) = reader else { continue };
                    first = reader;
                    let certain = match &body[first] {
                        Statement::Variable(Variable::Declare { value, .. }) | Statement::Evaluate(value) => {
                            let mut plain = true;
                            code::rewrite_expr(&mut value.clone(), &mut |found| {
                                plain &= !matches!(
                                    found,
                                    Expr::Conditional { .. }
                                        | Expr::Lambda { .. }
                                        | Expr::Map { .. }
                                        | Expr::Binary {
                                            op: BinOp::And | BinOp::Or,
                                            ..
                                        }
                                );
                            });
                            plain
                        }
                        _ => false,
                    };
                    // a function defined earlier may read the value, which matters only if it is called earlier
                    let mut readers = HashSet::from([name.clone()]);
                    loop {
                        let before = readers.len();
                        for statement in &body[..first] {
                            if let Statement::Function(function) = statement
                                && (code::references(std::slice::from_ref(statement)).iter())
                                    .any(|read| matches!(read, Dependency::Variable(read) if readers.contains(read)))
                            {
                                readers.insert(function.name.clone());
                            }
                        }
                        if readers.len() == before {
                            break;
                        }
                    }
                    let earlier = body[..first].iter().any(|statement| {
                        !matches!(statement, Statement::Function(_))
                            && !matches!(statement, Statement::Variable(Variable::Declare { name: declared, .. }) if *declared == name)
                            && (code::references(std::slice::from_ref(statement)).iter())
                                .any(|read| matches!(read, Dependency::Variable(read) if readers.contains(read)))
                    });
                    if !certain || earlier {
                        continue;
                    }
                }
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
                changed = true;
            }
            changed
        }
        let env = Environment {
            names: types.clone(),
            counts: counts.clone(),
            ..Environment::default()
        };
        for function in functions {
            visit(&mut function.body, &env, lang);
        }
    }

    // Whether a value is a deferred one, and what it computes when it is read.
    fn deferred(statement: &Statement) -> Option<(&str, &Expr)> {
        if let Statement::Variable(Variable::Declare {
            name,
            value: Expr::Call { function, args, .. },
            ..
        }) = statement
            && matches!(&**function, Expr::Name(callee) if callee == "_lazy")
            && let [Expr::Lambda { parameters, value }] = args.as_slice()
            && parameters.is_empty()
        {
            Some((name.as_str(), &**value))
        } else {
            None
        }
    }

    // Every value still deferred is given a place. One that cannot raise, or that some statement is
    // sure to read, is computed once before the first statement that may read it. One that can raise
    // and is read on some paths only is computed inside the branch or the step that reads it, so
    // that a path that does not read it never raises for it. Where more than one path reads it, what
    // it draws is drawn once beside it and the rest is computed on each path. What is left deferred
    // can raise, is read on more than one path, and draws in a call that can itself raise.
    fn settle(
        functions: &mut [code::Function],
        types: &std::rc::Rc<std::cell::RefCell<HashMap<String, Type>>>,
        counts: &Counts,
        lang: ScorerLang,
    ) {
        type Steps = HashMap<String, code::Function>;
        // the steps defined anywhere below a body, by name
        fn steps(body: &[Statement], found: &mut Steps) {
            for statement in body {
                match statement {
                    Statement::Function(function) => {
                        found.insert(function.name.clone(), function.clone());
                        steps(&function.body, found);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => steps(body, found),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        steps(then_body, found);
                        steps(else_body, found);
                    }
                    _ => {}
                }
            }
        }
        fn reads(refs: &HashSet<Dependency>, name: &str) -> bool {
            refs.contains(&Dependency::Variable(name.to_owned()))
        }
        // whether running a step may read the value, itself or through a step it calls
        fn step_may(step: &str, name: &str, table: &Steps, seen: &mut HashSet<String>) -> bool {
            let Some(function) = table.get(step) else { return false };
            if !seen.insert(step.to_owned()) {
                return false;
            }
            let refs = code::references(&function.body);
            reads(&refs, name)
                || refs
                    .iter()
                    .any(|read| matches!(read, Dependency::Variable(called) if step_may(called, name, table, seen)))
        }
        // whether a statement may read the value when it runs. Defining a step reads nothing.
        fn may(statement: &Statement, name: &str, table: &Steps) -> bool {
            if matches!(statement, Statement::Function(_)) {
                return false;
            }
            let refs = code::references(std::slice::from_ref(statement));
            reads(&refs, name)
                || refs.iter().any(
                    |read| matches!(read, Dependency::Variable(called) if step_may(called, name, table, &mut HashSet::new())),
                )
        }
        // whether evaluating an expression is sure to read the value
        fn sure_in(value: &Expr, name: &str, table: &Steps, seen: &mut HashSet<String>) -> bool {
            let mut sure = |value: &Expr| sure_in(value, name, table, seen);
            match value {
                Expr::Call {
                    function,
                    args,
                    keywords,
                } => {
                    if let Expr::Name(called) = &**function {
                        if called == name && args.is_empty() {
                            return true;
                        }
                        // a value that waits is not read where it is declared
                        if called == "_lazy" {
                            return false;
                        }
                    }
                    if args.iter().chain(keywords.iter().map(|(_, value)| value)).any(&mut sure) {
                        return true;
                    }
                    match &**function {
                        Expr::Name(called) => step_sure(called, name, table, seen),
                        other => sure(other),
                    }
                }
                Expr::New { args, .. } | Expr::Array(args) | Expr::Template(args) => args.iter().any(sure),
                Expr::Binary {
                    left,
                    op: BinOp::And | BinOp::Or,
                    ..
                } => sure(left),
                Expr::Binary { left, right, .. }
                | Expr::Index(left, right)
                | Expr::Identity(left, right)
                | Expr::Contains(left, right) => sure(left) || sure(right),
                Expr::Conditional { condition, yes, no } => sure(condition) || (sure(yes) && sure(no)),
                Expr::Unary { value, .. }
                | Expr::Member(value, _)
                | Expr::Spread(value)
                | Expr::Required(value)
                | Expr::Read { value, .. } => sure(value),
                Expr::Slice { value, start, end } => {
                    sure(value) || start.as_deref().is_some_and(&mut sure) || end.as_deref().is_some_and(&mut sure)
                }
                Expr::Object(fields) => fields.iter().any(|(_, value)| sure(value)),
                Expr::Map { iterable, .. } => sure(iterable),
                _ => false,
            }
        }
        fn step_sure(step: &str, name: &str, table: &Steps, seen: &mut HashSet<String>) -> bool {
            let Some(function) = table.get(step) else { return false };
            if !seen.insert(step.to_owned()) {
                return false;
            }
            let found = body_sure(&function.body, name, table, seen);
            seen.remove(step);
            found
        }
        // whether a statement leaves its body early on some path
        fn leaves(statement: &Statement) -> bool {
            match statement {
                Statement::Return(_) | Statement::Raise(_) | Statement::Break => true,
                Statement::If {
                    then_body, else_body, ..
                } => then_body.iter().chain(else_body).any(leaves),
                Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => body.iter().any(leaves),
                _ => false,
            }
        }
        fn statement_sure(statement: &Statement, name: &str, table: &Steps, seen: &mut HashSet<String>) -> bool {
            match statement {
                Statement::Variable(Variable::Declare { value, .. })
                | Statement::Return(value)
                | Statement::Evaluate(value)
                | Statement::Raise(value) => sure_in(value, name, table, seen),
                Statement::Variable(Variable::Assign { name: target, value }) => {
                    sure_in(target, name, table, seen) || sure_in(value, name, table, seen)
                }
                Statement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    sure_in(condition, name, table, seen)
                        || (body_sure(then_body, name, table, seen) && body_sure(else_body, name, table, seen))
                }
                Statement::TraceScope { scope, body } => {
                    sure_in(scope, name, table, seen) || body_sure(body, name, table, seen)
                }
                Statement::ForRange { count, .. } => sure_in(count, name, table, seen),
                Statement::Function(_) | Statement::Break => false,
            }
        }
        // whether running a body is sure to read the value before it can leave
        fn body_sure(body: &[Statement], name: &str, table: &Steps, seen: &mut HashSet<String>) -> bool {
            for statement in body {
                if statement_sure(statement, name, table, seen) {
                    return true;
                }
                if leaves(statement) {
                    return false;
                }
            }
            false
        }
        // whether computing a value twice gives the same value twice, which a draw from the run's
        // random stream does not. A helper says whether its own call draws from it.
        fn replays(value: &Expr, counts: &Counts) -> bool {
            let source = |value: &Expr| {
                matches!(value, Expr::Name(name) if name == "random_source" || name == "randomSource")
                    || matches!(value, Expr::Member(target, field) if field == "random" && matches!(&**target, Expr::Name(name) if name == "context"))
            };
            let mut replays = true;
            code::rewrite_expr(&mut value.clone(), &mut |found| {
                let Expr::Call { function, args, .. } = found else { return };
                let draws = match &**function {
                    Expr::Name(name) => match counts.called(name, args) {
                        Some((helper, given)) => match helper.draws {
                            Some(draws) => {
                                let facts: Vec<_> = given.iter().map(|value| fact(value, &Type::Unknown)).collect();
                                draws(&facts)
                            }
                            None => helper.random,
                        },
                        None => args.iter().any(&source),
                    },
                    // the context draws for the agent
                    Expr::Member(target, _) => matches!(&**target, Expr::Name(name) if name == "context"),
                    _ => false,
                };
                replays &= !draws;
            });
            replays
        }
        // The parts of a value that draw and cannot raise, each replaced by a name. What is left
        // computes the same however often it runs, once the parts are drawn under those names.
        fn drawn(
            value: &mut Expr,
            env: &Environment,
            counts: &Counts,
            lang: ScorerLang,
            name: &mut dyn FnMut() -> String,
            parts: &mut Vec<(String, Expr)>,
        ) {
            if replays(value, counts) {
                return;
            }
            if infallible(value, env, lang) {
                let part = name();
                parts.push((part.clone(), std::mem::replace(value, Expr::Name(part))));
                return;
            }
            let mut each = |value: &mut Expr| drawn(value, env, counts, lang, name, parts);
            match value {
                Expr::Call { args, keywords, .. } => {
                    args.iter_mut().for_each(&mut each);
                    keywords.iter_mut().for_each(|(_, value)| each(value));
                }
                Expr::Array(items) | Expr::Template(items) => items.iter_mut().for_each(each),
                Expr::Object(fields) => fields.iter_mut().for_each(|(_, value)| each(value)),
                Expr::Binary { left, right, .. } | Expr::Index(left, right) => {
                    each(left);
                    each(right);
                }
                Expr::Conditional { condition, yes, no } => {
                    each(condition);
                    each(yes);
                    each(no);
                }
                Expr::Unary { value, .. } | Expr::Member(value, _) | Expr::Spread(value) | Expr::Required(value) => each(value),
                // what runs once for each item draws once for each item
                _ => {}
            }
        }
        // how often a step is mentioned below a body
        fn mentions(body: &[Statement], step: &str) -> usize {
            let mut count = 0;
            code::rewrite(&mut body.to_vec(), &mut |value| {
                if matches!(value, Expr::Name(name) if name == step) {
                    count += 1;
                }
            });
            count
        }
        // runs something on the body of a step defined below a body
        fn in_step(body: &mut [Statement], step: &str, run: &mut dyn FnMut(&mut Vec<Statement>)) -> bool {
            body.iter_mut().any(|statement| match statement {
                Statement::Function(function) if function.name == step => {
                    run(&mut function.body);
                    true
                }
                Statement::Function(function) => in_step(&mut function.body, step, run),
                Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => in_step(body, step, run),
                Statement::If {
                    then_body, else_body, ..
                } => in_step(then_body, step, run) || in_step(else_body, step, run),
                _ => false,
            })
        }
        // the steps an expression calls that may read the value, and how often it reads the value itself
        fn readers(value: &Expr, name: &str, table: &Steps, called: &mut Vec<String>) -> usize {
            let mut direct = 0;
            code::rewrite_expr(&mut value.clone(), &mut |found| match found {
                Expr::Call { function, args, .. }
                    if args.is_empty() && matches!(&**function, Expr::Name(read) if read == name) =>
                {
                    direct += 1;
                }
                Expr::Name(step) if step_may(step, name, table, &mut HashSet::new()) && !called.contains(step) => {
                    called.push(step.clone());
                }
                _ => {}
            });
            direct
        }
        struct Pass<'a> {
            types: &'a std::rc::Rc<std::cell::RefCell<HashMap<String, Type>>>,
            counts: &'a Counts,
            lang: ScorerLang,
            // every value still deferred, for how sure the ones that read it are
            waiting: HashMap<String, Expr>,
        }
        impl Pass<'_> {
            fn env(&self) -> Environment {
                Environment {
                    names: self.types.borrow().clone(),
                    counts: self.counts.clone(),
                    pending: self.waiting.clone(),
                    ..Environment::default()
                }
            }
            fn body(&mut self, body: &mut Vec<Statement>) {
                for statement in body.iter_mut() {
                    match statement {
                        Statement::Function(function) => self.body(&mut function.body),
                        Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => self.body(body),
                        Statement::If {
                            then_body, else_body, ..
                        } => {
                            self.body(then_body);
                            self.body(else_body);
                        }
                        _ => {}
                    }
                }
                let mut kept = HashSet::new();
                loop {
                    // a value another deferred value reads is placed after that one, which is then a statement that reads it
                    let waiting: Vec<(String, Expr)> = (body.iter())
                        .filter_map(deferred)
                        .filter(|(name, _)| !kept.contains(*name))
                        .map(|(name, value)| (name.to_owned(), value.clone()))
                        .collect();
                    let next = waiting.iter().find(|(name, _)| {
                        !waiting
                            .iter()
                            .any(|(other, value)| other != name && reads(&value.references(), name))
                    });
                    let Some((name, value)) = next.cloned() else { break };
                    if !self.place(body, &name, value) {
                        kept.insert(name);
                    }
                }
                // a value left deferred is declared before anything that may read it
                let (deferred, rest): (Vec<_>, Vec<_>) = std::mem::take(body)
                    .into_iter()
                    .partition(|statement| deferred(statement).is_some());
                if !deferred.is_empty() {
                    let after = rest
                        .iter()
                        .position(|statement| {
                            !matches!(
                                statement,
                                Statement::Variable(Variable::Declare {
                                    value: Expr::New { .. },
                                    ..
                                })
                            )
                        })
                        .unwrap_or(rest.len());
                    let mut rest = rest;
                    let tail = rest.split_off(after);
                    rest.extend(deferred);
                    rest.extend(tail);
                    *body = rest;
                } else {
                    *body = rest;
                }
            }
            // gives one deferred value its place, or says that it has none
            fn place(&mut self, body: &mut Vec<Statement>, name: &str, value: Expr) -> bool {
                let mut table = Steps::new();
                steps(body, &mut table);
                let own = |statement: &Statement| deferred(statement).is_some_and(|(found, _)| found == name);
                let Some(declared) = body.iter().position(own) else {
                    return false;
                };
                let first = body
                    .iter()
                    .position(|statement| !own(statement) && may(statement, name, &table));
                // nothing reads it, so it is never computed
                let Some(first) = first else {
                    body.remove(declared);
                    self.waiting.remove(name);
                    return true;
                };
                let env = self.env();
                let read_for_sure = body_sure(&body[first..], name, &table, &mut HashSet::new());
                // a choice keeps the name it assigns directly above it
                let first = match (&body[first], first.checked_sub(1).map(|above| &body[above])) {
                    (
                        Statement::If { .. },
                        Some(Statement::Variable(Variable::Declare {
                            value: Expr::Null,
                            mutable: true,
                            ..
                        })),
                    ) => first - 1,
                    _ => first,
                };
                if read_for_sure || infallible(&value, &env, self.lang) {
                    // what it reads has to be there already
                    let needs = value.references();
                    let late = body[first..].iter().any(|statement| {
                        matches!(statement, Statement::Variable(Variable::Declare { name: found, .. })
                            if deferred(statement).is_none() && reads(&needs, found))
                    });
                    if late {
                        return false;
                    }
                    let (value, kind) = expression(value, &env, self.lang);
                    body.remove(declared);
                    let at = if declared < first { first - 1 } else { first };
                    body.insert(at, Statement::Variable(Variable::constant(name, value)));
                    self.taken(body, name, kind);
                    return true;
                }
                // it can raise and no statement here is sure to read it, so it goes where it is read

                let same = replays(&value, self.counts);
                if !same {
                    // what it draws is drawn here, once, and the rest is computed where it is read
                    let mut parts = Vec::new();
                    let mut used: HashSet<String> = self.types.borrow().keys().cloned().collect();
                    let mut part = || allocate(name, "draw", self.lang, &mut used);
                    let mut rest = value.clone();
                    drawn(&mut rest, &env, self.counts, self.lang, &mut part, &mut parts);
                    if !parts.is_empty() && replays(&rest, self.counts) {
                        for (offset, (part, drawn)) in parts.into_iter().enumerate() {
                            let (drawn, kind) = expression(drawn, &env, self.lang);
                            self.types.borrow_mut().insert(part.clone(), normalized(&kind));
                            body.insert(declared + offset, Statement::Variable(Variable::constant(part, drawn)));
                        }
                        let declared = body.iter().position(own).unwrap();
                        if let Statement::Variable(Variable::Declare { value: held, .. }) = &mut body[declared] {
                            *held = rest.clone().lazy();
                        }
                        self.waiting.insert(name.to_owned(), rest);
                        // the value is placed again, now that it computes the same wherever it is read
                        return self.place(body, name, self.waiting[name].clone());
                    }
                }
                let declaration = body[declared].clone();
                let mut places = 0;
                let mut into = Vec::new();
                for (index, statement) in body.iter().enumerate() {
                    if index == declared || !may(statement, name, &table) {
                        continue;
                    }
                    places += 1;
                    let mut called = Vec::new();
                    let direct = match statement {
                        Statement::Variable(Variable::Declare { value, .. })
                        | Statement::Return(value)
                        | Statement::Evaluate(value)
                        | Statement::Raise(value) => readers(value, name, &table, &mut called),
                        Statement::Variable(Variable::Assign { name: target, value }) => {
                            readers(target, name, &table, &mut called) + readers(value, name, &table, &mut called)
                        }
                        Statement::If { condition, .. } => readers(condition, name, &table, &mut called),
                        Statement::TraceScope { scope, .. } => readers(scope, name, &table, &mut called),
                        // a loop may run its body many times
                        Statement::ForRange { count, .. } => {
                            if !same {
                                return false;
                            }
                            readers(count, name, &table, &mut called)
                        }
                        _ => 0,
                    };
                    // a value read in the middle of an expression is written out there, once
                    if direct > 1 && !same {
                        return false;
                    }
                    for step in called {
                        // a step that runs more than once would compute it more than once
                        if !same && (mentions(body, &step) != 1 || into.contains(&step)) {
                            return false;
                        }
                        if !into.contains(&step) {
                            into.push(step);
                        }
                    }
                }
                if places + into.len().saturating_sub(1) > 1 && !same {
                    return false;
                }
                let read = |found: &mut Expr| {
                    if matches!(found, Expr::Call { function, args, .. }
                        if args.is_empty() && matches!(&**function, Expr::Name(read) if read == name))
                    {
                        *found = value.clone();
                    }
                };
                for (index, statement) in body.iter_mut().enumerate() {
                    if index == declared || matches!(statement, Statement::Function(_)) {
                        continue;
                    }
                    let mut read = read;
                    match statement {
                        Statement::Variable(Variable::Declare { value, .. })
                        | Statement::Return(value)
                        | Statement::Evaluate(value)
                        | Statement::Raise(value) => code::rewrite_expr(value, &mut read),
                        Statement::Variable(Variable::Assign { name: target, value }) => {
                            code::rewrite_expr(target, &mut read);
                            code::rewrite_expr(value, &mut read);
                        }
                        Statement::If {
                            condition,
                            then_body,
                            else_body,
                        } => {
                            code::rewrite_expr(condition, &mut read);
                            for branch in [then_body, else_body] {
                                if branch.iter().any(|statement| may(statement, name, &table)) {
                                    branch.insert(0, declaration.clone());
                                    self.body(branch);
                                }
                            }
                        }
                        Statement::TraceScope { scope, body } => {
                            code::rewrite_expr(scope, &mut read);
                            if body.iter().any(|statement| may(statement, name, &table)) {
                                body.insert(0, declaration.clone());
                                self.body(body);
                            }
                        }
                        Statement::ForRange { count, body, .. } => {
                            code::rewrite_expr(count, &mut read);
                            if body.iter().any(|statement| may(statement, name, &table)) {
                                body.insert(0, declaration.clone());
                                self.body(body);
                            }
                        }
                        _ => {}
                    }
                }
                body.remove(declared);
                for step in into {
                    in_step(body, &step, &mut |inner| {
                        inner.insert(0, declaration.clone());
                        self.body(inner);
                    });
                }
                true
            }
            // a value that is now computed where it is declared is read by name, and typed as what it is
            fn taken(&mut self, body: &mut [Statement], name: &str, kind: Type) {
                code::rewrite(body, &mut |found| {
                    if matches!(found, Expr::Call { function, args, keywords }
                        if args.is_empty() && keywords.is_empty() && matches!(&**function, Expr::Name(read) if read == name))
                    {
                        *found = Expr::Name(name.to_owned());
                    }
                });
                self.waiting.remove(name);
                self.types.borrow_mut().insert(name.to_owned(), normalized(&kind));
            }
        }
        fn collect(body: &[Statement], waiting: &mut HashMap<String, Expr>, declared: &mut HashSet<String>) {
            for statement in body {
                match statement {
                    Statement::Variable(Variable::Declare { name, .. }) => match deferred(statement) {
                        Some((name, value)) => {
                            waiting.insert(name.to_owned(), value.clone());
                        }
                        None => {
                            declared.insert(name.clone());
                        }
                    },
                    Statement::Function(function) => collect(&function.body, waiting, declared),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => collect(body, waiting, declared),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        collect(then_body, waiting, declared);
                        collect(else_body, waiting, declared);
                    }
                    _ => {}
                }
            }
        }
        let mut waiting = HashMap::new();
        let mut declared = HashSet::new();
        for function in functions.iter() {
            collect(&function.body, &mut waiting, &mut declared);
        }
        // a value an earlier pass already computes where it is declared has the type of what it computes
        for name in declared.iter().filter(|name| !waiting.contains_key(*name)) {
            let mut types = types.borrow_mut();
            if let Some(Type::Deferred(kind)) = types.get(name).cloned() {
                types.insert(name.clone(), normalized(&kind));
            }
        }
        let mut pass = Pass {
            types,
            counts,
            lang,
            waiting,
        };
        for function in functions.iter_mut() {
            pass.body(&mut function.body);
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
                        // a list written out names its item, a named one has a name for it
                        p.annotation.as_ref().map(|a| {
                            let written = (a.strip_prefix("Array<").and_then(|a| a.strip_suffix('>')))
                                .or_else(|| a.strip_prefix("list[").and_then(|a| a.strip_suffix(']')));
                            written.map(str::to_owned).unwrap_or_else(|| format!("{a}Item"))
                        })
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
        // `known` are the helpers already found pure, which another pure helper may call
        fn convert(body: &mut [Statement], known: &mut HashSet<String>) {
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
                                Expr::Call { function, .. } if !matches!(&**function, Expr::Name(n) if known.contains(n) || ["_num", "_integer", "_index", "_equal", "_boolean", "_len", "_text", "_trim", "_float", "_value", "checked_number", "remainder", "divide"].contains(&n.as_str())) => {
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
                                known.insert(name.clone());
                                let mut f = code::Function::new(name.clone(), vec![]);
                                f.body.push(Statement::Return((**value).clone()));
                                *statement = Statement::Function(f);
                            }
                        }
                    }
                    Statement::Function(f) => convert(&mut f.body, &mut known.clone()),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => convert(body, known),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        convert(then_body, &mut known.clone());
                        convert(else_body, &mut known.clone());
                    }
                    _ => {}
                }
            }
        }
        for function in functions {
            convert(&mut function.body, &mut HashSet::new());
        }
    }

    fn idioms(functions: &mut Vec<code::Function>, lang: ScorerLang) {
        fn literal(value: &Expr) -> bool {
            match value {
                Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Array(items) => items.iter().all(literal),
                Expr::Object(fields) => fields.iter().all(|(_, value)| literal(value)),
                _ => false,
            }
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
        let mut traverses = functions
            .iter()
            .any(|function| code::references(&function.body).contains(&Dependency::Variable("_history".into())));
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
                    // a path that is either certain or impossible needs no draw
                    if name == "_include"
                        && let Some(Expr::Conditional { condition, yes, no }) = args.first()
                        && matches!(&**yes, Expr::Number(n) if n == "1")
                        && matches!(&**no, Expr::Number(n) if n == "0")
                    {
                        *value = (**condition).clone();
                        return;
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
        counts: &Counts,
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
                            Type::Number | Type::Real(..) | Type::Integer(..) => {
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
                        } else if lang == ScorerLang::Typescript
                            && annotation.is_none()
                            && matches!(kind, Type::Unknown | Type::Value)
                        {
                            // nothing is known of it, so nothing is read from it
                            *annotation = Some("unknown".into());
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
                counts: counts.clone(),
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
            known(&mut function.body);
            apply(&mut function.body, &env, contracts, lang);
        }
    }
    // a value just compared with null is known where it is read, so it is read as it is
    fn known(body: &mut [Statement]) {
        fn plain(tested: &Expr) -> impl FnMut(&mut Expr) {
            let tested = format!("{tested:?}");
            move |value| {
                if let Expr::Required(inner) = value
                    && format!("{inner:?}") == tested
                {
                    *value = (**inner).clone();
                }
            }
        }
        fn within(body: &mut [Statement]) {
            for statement in body {
                match statement {
                    Statement::If {
                        condition,
                        then_body,
                        else_body,
                    } => {
                        if let Expr::Unary { op: UnaryOp::Not, value } = condition
                            && let Expr::Identity(tested, nothing) = &**value
                            && matches!(**nothing, Expr::Null)
                        {
                            code::rewrite(then_body, &mut plain(tested));
                        }
                        within(then_body);
                        within(else_body);
                    }
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => within(body),
                    _ => {}
                }
            }
        }
        within(body);
        code::rewrite(body, &mut |value| {
            if let Expr::Conditional { condition, no, .. } = value
                && let Expr::Identity(tested, nothing) = &**condition
                && matches!(**nothing, Expr::Null)
            {
                code::rewrite_expr(no, &mut plain(tested));
            }
        });
    }

    // Trace records observe the execution; application results never come from
    // the collector. Structural control-flow scopes are not SDK spans.
    fn observe(functions: &mut [code::Function], lang: ScorerLang) {
        let history = functions
            .iter()
            .any(|function| code::references(&function.body).contains(&Dependency::Variable("_history".into())));
        fn scopes(body: &mut Vec<Statement>, history: bool, lang: ScorerLang) {
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
            // a scope that stays still hands its values back as locals
            let kept: HashSet<String> = (structural.iter())
                .filter(|name| history || needed.contains(*name))
                .cloned()
                .collect();
            structural.retain(|name| !history && !needed.contains(name));
            let mut output = Vec::new();
            for mut statement in std::mem::take(body) {
                match &mut statement {
                    Statement::Variable(Variable::Declare { name, .. }) if structural.contains(name) => continue,
                    Statement::TraceScope {
                        scope: Expr::Name(name),
                        body,
                    } if structural.contains(name) || kept.contains(name) => {
                        let frame = name.clone();
                        fn fields(body: &mut [Statement], frame: &str, lang: ScorerLang) {
                            for statement in body {
                                match statement {
                                    Statement::Variable(Variable::Assign {
                                        name: Expr::Index(target, field),
                                        value,
                                    }) if matches!(&**target, Expr::Member(target, member) if member == "fields" && matches!(&**target, Expr::Name(n) if n == frame)) => {
                                        if let Expr::String(field) = &**field {
                                            *statement = Statement::Variable(Variable::constant(
                                                field_local(frame, field, lang),
                                                value.clone(),
                                            ));
                                        }
                                    }
                                    Statement::If {
                                        then_body, else_body, ..
                                    } => {
                                        fields(then_body, frame, lang);
                                        fields(else_body, frame, lang);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        fields(body, &frame, lang);
                        code::rewrite(body, &mut |value| {
                            if let Expr::Member(target, field) = value
                                && matches!(&**target, Expr::Name(n) if n == &frame)
                                && !["children", "iterations", "fields", "log"].contains(&field.as_str())
                            {
                                *value = Expr::Name(field_local(&frame, field, lang));
                            }
                        });
                        scopes(body, history, lang);
                        if kept.contains(&frame) {
                            output.push(Statement::TraceScope {
                                scope: Expr::Name(frame),
                                body: std::mem::take(body),
                            });
                        } else {
                            output.append(body);
                        }
                        continue;
                    }
                    Statement::Function(f) => scopes(&mut f.body, history, lang),
                    Statement::TraceScope { body, .. } | Statement::ForRange { body, .. } => scopes(body, history, lang),
                    Statement::If {
                        then_body, else_body, ..
                    } => {
                        scopes(then_body, history, lang);
                        scopes(else_body, history, lang);
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
            scopes(&mut function.body, history, lang);
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

    fn expand_results(functions: &mut [code::Function], lang: ScorerLang, types: &HashMap<String, Type>, counts: &Counts) {
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
                    counts: counts.clone(),
                    ..Environment::default()
                },
            );
        }
    }

    fn local_names(functions: &mut [code::Function], lang: ScorerLang, numbered: &[(String, String)]) {
        let defined: HashSet<String> = functions.iter().map(|function| function.name.clone()).collect();
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
                    continue;
                }
                // a function numbered apart from another has its number at the end of its span's name
                let digits = function.name.len() - function.name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
                if digits == 0 {
                    continue;
                }
                let (stem, number) = function.name.split_at(function.name.len() - digits);
                let old = if lang == ScorerLang::Python {
                    let stem = stem.trim_end_matches('_');
                    if suffix == "span" {
                        format!("{stem}_span_{number}")
                    } else {
                        format!("{stem}_span_{number}_{suffix}")
                    }
                } else if suffix == "span" {
                    format!("{stem}Span{number}")
                } else {
                    format!("{stem}Span{number}{}{}", suffix[..1].to_uppercase(), &suffix[1..])
                };
                if used.contains(&old) && !used.contains(suffix) {
                    names.insert(old, suffix.to_string());
                }
            }
            for (field, local) in [("included", "included"), ("chosen", "selection")] {
                let old = field_local(&function.name, field, lang);
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
            // a name numbered to keep it apart from another function's local needs no number here
            // a helper numbered apart is a function, which keeps its name wherever it is called
            for (name, plain) in numbered {
                // two that share a plain name both keep the names that tell them apart
                let alone = !numbered
                    .iter()
                    .any(|(other, shared)| other != name && shared == plain && used.contains(other));
                if used.contains(name)
                    && alone
                    && !defined.contains(name)
                    && !names.contains_key(name)
                    && !used.contains(plain)
                    && !defined.contains(plain)
                    && !names.values().any(|taken| taken == plain)
                {
                    used.insert(plain.clone());
                    names.insert(name.clone(), plain.clone());
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

    // The local that holds a value a control-flow scope keeps for its caller.
    fn field_local(frame: &str, field: &str, lang: ScorerLang) -> String {
        let stem = frame
            .strip_suffix("_span")
            .or_else(|| frame.strip_suffix("Span"))
            .unwrap_or(frame);
        if lang == ScorerLang::Python {
            format!("{stem}_{field}")
        } else {
            format!("{stem}{}{}", field[..1].to_uppercase(), &field[1..])
        }
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
    fn stem(frame: &str) -> &str {
        frame
            .strip_suffix("_span")
            .or_else(|| frame.strip_suffix("Span"))
            .unwrap_or(frame)
    }
    // The fields of a step that are read outside it, and the steps inside it that are. A later step
    // reads those through what this one returns, so it carries them with its output.
    type Carried = (String, String, Expr, Type);
    fn handed_back(env: &Environment, frame: &str, lang: ScorerLang) -> (Vec<String>, Vec<Carried>) {
        let mut fields = env.exports.get(frame).cloned().unwrap_or_default();
        let mut children = Vec::new();
        for key in env.nested.get(frame).into_iter().flatten() {
            let field = stem(key).to_string();
            if field == "request" || field == "result" {
                continue;
            }
            let (value, kind) = if let Some(record) = env.records.get(key) {
                (record.value.clone(), Type::Record(Box::new(record.shape.clone())))
            } else if let Some((value, shape)) = env.collections.get(key) {
                (value.clone(), Type::List(Box::new(Type::Record(Box::new(shape.clone())))))
            } else if let Some(kind) = env.steps.get(key) {
                let result = label(env, key, "output", lang);
                // a step that hands nothing back, or ran where its result cannot be seen from here, is not carried
                if matches!(kind, Type::Product(step) if !step.bundled() && matches!(step.output, Type::Null))
                    || !env.names.contains_key(&result)
                {
                    continue;
                }
                (Expr::Name(result), kind.clone())
            } else {
                continue;
            };
            children.push((key.clone(), field, value, kind));
        }
        let returns =
            (env.fields.get(&(frame.to_string(), "output".to_string()))).is_some_and(|(_, kind)| !matches!(kind, Type::Null));
        if !children.is_empty() && returns && !fields.iter().any(|key| key == "output") {
            fields.push("output".into());
            fields.sort();
        }
        (fields, children)
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
        // the function that runs one round of a loop, by the frame its rounds are kept under
        let rounds: HashMap<String, String> = statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Variable(Variable::Declare {
                    name,
                    value: Expr::Call { function, args, .. },
                    ..
                }) if matches!(&**function, Expr::Name(n) if n == "_loop") => match args.get(2) {
                    Some(Expr::Name(callee)) => Some((callee.clone(), name.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let mut seeded = HashMap::<String, Branch>::new();
        let mut pending = statements.into_iter().collect::<std::collections::VecDeque<_>>();
        while let Some(statement) = pending.pop_front() {
            match statement {
                Statement::Function(mut function) => {
                    for dependency in code::references(&function.body) {
                        if let Dependency::Variable(name) = dependency {
                            env.captured.insert(name);
                        }
                    }
                    let mut child_env = env.clone();
                    // a round reads the rounds before it, so its shape is found first and then used
                    let original =
                        (rounds.contains_key(&function.name) && !seeded.contains_key(&function.name)).then(|| function.clone());
                    if let Some(frame) = rounds.get(&function.name)
                        && let Some(shape) = seeded.get(&function.name)
                    {
                        let results = label(env, frame, "output", lang);
                        child_env
                            .names
                            .insert(results.clone(), Type::List(Box::new(Type::Record(Box::new(shape.clone())))));
                        child_env.collections.insert(frame.clone(), (results.into(), shape.clone()));
                    }
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
                        let (fields, children) = handed_back(&child_env, &frame, lang);
                        lifted.insert(
                            function.name.clone(),
                            Lifted {
                                result: label(env, &frame, "output", lang),
                                frame: frame.clone(),
                                constructor,
                                span: child_env.names.get(&frame).cloned().unwrap_or(Type::Unknown),
                                step: Step {
                                    output: kind,
                                    fields: fields
                                        .into_iter()
                                        .map(|key| {
                                            let kind = child_env
                                                .fields
                                                .get(&(frame.clone(), key.clone()))
                                                .map(|(_, t)| t.clone())
                                                .unwrap_or(Type::Null);
                                            (key, kind)
                                        })
                                        .collect(),
                                    children: children
                                        .into_iter()
                                        .map(|(key, field, _, kind)| (key, (field, kind)))
                                        .collect(),
                                },
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
                                    .chain(branch.results().map(|(k, v)| (k.clone(), v.clone())))
                                    .collect(),
                            )
                        };
                        env.returns.insert(function.name.clone(), kind);
                    }
                    if let Some(original) = original
                        && let Some(shape) = env.branches.get(&function.name)
                        && shape.fields.contains_key("again")
                    {
                        seeded.insert(function.name.clone(), shape.clone());
                        pending.push_front(Statement::Function(original));
                        continue;
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
                        let bundled = lift.step.bundled();
                        let result = Expr::from(lift.result.clone());
                        let output_value = if bundled {
                            result.clone().index(Expr::string("result"))
                        } else {
                            result.clone()
                        };
                        env.names.insert(lift.result.clone(), lift.step.shape());
                        env.steps
                            .insert(lift.frame.clone(), Type::Product(Box::new(lift.step.clone())));
                        env.outputs
                            .insert(lift.frame.clone(), (output_value, lift.step.output.clone()));
                        for (field, kind) in &lift.step.fields {
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
                    // A loop-back is a native loop that stops when a round does not ask for another.
                    if let Expr::Call { function, args, .. } = &value
                        && matches!(&**function,Expr::Name(n) if n=="_loop")
                        && args.len() == 5
                    {
                        let python = lang == ScorerLang::Python;
                        let stem = name
                            .strip_suffix("_span")
                            .or_else(|| name.strip_suffix("Span"))
                            .unwrap_or(&name);
                        let index = if python {
                            format!("{stem}_index")
                        } else {
                            format!("{stem}Index")
                        };
                        let latest = if python {
                            format!("{stem}_latest")
                        } else {
                            format!("{stem}Latest")
                        };
                        let failure = |message: &str| {
                            Statement::Raise(Expr::New {
                                class: if python { "ValueError" } else { "Error" }.into(),
                                args: vec![Expr::string(message)],
                            })
                        };
                        let position = Expr::from(index.clone());
                        let (bound, stop) = match &args[4] {
                            Expr::Null => (
                                Expr::number(10000),
                                Statement::If {
                                    condition: position.clone().binary(BinOp::Ge, Expr::number(9999)),
                                    then_body: vec![failure("graph execution budget exceeded (10000 traversals)")],
                                    else_body: vec![],
                                },
                            ),
                            limit => {
                                let limit = expression(Expr::call("_count", vec![limit.clone()]), env, lang).0;
                                let bound = match &limit {
                                    Expr::Number(n) => n.parse::<i128>().map(|n| Expr::number(n + 1)).ok(),
                                    _ => None,
                                }
                                .unwrap_or_else(|| limit.clone().binary(BinOp::Add, Expr::number(1)));
                                (
                                    bound,
                                    Statement::If {
                                        condition: position.clone().binary(BinOp::Ge, limit),
                                        then_body: vec![failure("cycle max_traversals exceeded")],
                                        else_body: vec![],
                                    },
                                )
                            }
                        };
                        let round = Expr::call(
                            args[2].clone(),
                            vec![position.clone(), position.clone().binary(BinOp::Add, Expr::number(1))],
                        );
                        let append = if python { "append" } else { "push" };
                        if let Expr::Name(callee) = &args[2]
                            && let Some(branch) = env.branches.get(callee).cloned()
                            && branch.fields.contains_key("again")
                        {
                            let results = label(env, &name, "output", lang);
                            output.push(Statement::Variable(Variable::constant(&results, Expr::Array(vec![]))));
                            output.push(Statement::ForRange {
                                index,
                                count: bound,
                                body: vec![
                                    Statement::Variable(Variable::constant(&latest, round)),
                                    Statement::Evaluate(Expr::call(
                                        Expr::from(results.clone()).member(append),
                                        vec![latest.clone().into()],
                                    )),
                                    Statement::If {
                                        condition: Expr::from(latest).index(Expr::string("again")).not(),
                                        then_body: vec![Statement::Break],
                                        else_body: vec![],
                                    },
                                    stop,
                                ],
                            });
                            env.names
                                .insert(results.clone(), Type::List(Box::new(Type::Record(Box::new(branch.clone())))));
                            env.collections.insert(name, (results.into(), branch));
                            continue;
                        }
                        // A round that cannot hand back what later steps read has no ordered form.
                        // The whole agent is printed in the deferred form instead.
                        env.types.borrow_mut().insert(UNPRINTABLE.into(), Type::Null);
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
                        && infallible(inner, env, lang)
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
                            if matches!(value, Expr::Null) {
                                // a step with nothing to hand back holds nothing under a name
                                if let Some(Type::Span(fields, _)) = env.names.get_mut(frame) {
                                    fields.insert(key.clone(), Type::Null);
                                }
                                env.fields.insert((frame.clone(), key.clone()), (Expr::Null, Type::Null));
                                fields.push((key, Expr::Null));
                                continue;
                            }
                            if matches!(value, Expr::Conditional { .. }) {
                                output.push(Statement::Variable(Variable::Declare {
                                    name: name.clone(),
                                    value: Expr::Null,
                                    mutable: true,
                                    annotation: None,
                                }));
                                // the name starts as null, so only a value is assigned to it
                                let assign = |value| assign_value(&name, value).remove(0);
                                output.extend(where_present(&value, &assign).unwrap_or_else(|| assign_value(&name, value)));
                            } else {
                                output.push(Statement::Variable(Variable::constant(&name, value)));
                            }
                            env.names.insert(name.clone(), kind.clone());
                            if let Some(Type::Span(fields, _)) = env.names.get_mut(frame) {
                                fields.insert(key.clone(), kind.clone());
                            }
                            env.fields.insert((frame.clone(), key.clone()), (name.clone().into(), kind));
                            fields.push((key, name.into()));
                        } else if key == "error"
                            && let Some(logged) = where_present(&value, &|value| {
                                Statement::Evaluate(Expr::Call {
                                    function: function.clone(),
                                    args: args.clone(),
                                    keywords: vec![(key.clone(), value)],
                                })
                            })
                        {
                            // an error is logged only where there is one
                            output.extend(logged);
                        } else {
                            fields.push((key, value));
                        }
                    }
                    if !fields.is_empty() {
                        output.push(Statement::Evaluate(Expr::Call {
                            function,
                            args,
                            keywords: fields,
                        }));
                    }
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
                            children.insert(key.clone(), kind.clone());
                        } else if member == "fields" {
                            fields.insert(key.clone(), kind.clone());
                        }
                        if member == "fields"
                            && key == "included"
                            && let Some(conditions) = conjuncts(&value)
                        {
                            let sure = (env.records.values())
                                .filter(|record| {
                                    (record.shape.gate.as_ref().and_then(conjuncts))
                                        .is_some_and(|needed| needed.iter().all(|one| conditions.contains(one)))
                                })
                                .filter_map(|record| match &record.value {
                                    Expr::Name(name) => Some(name.clone()),
                                    _ => None,
                                })
                                .collect();
                            env.guarded.insert(frame.clone(), sure);
                            env.gates.insert(frame.clone(), value.clone());
                        }
                    }
                    if matches!(&value, Expr::Name(n) if env.records.contains_key(n) || env.collections.contains_key(n)) {
                        continue;
                    }
                    // what a step keeps for its caller is part of what it returns, not a field of its span
                    if let Expr::Index(target, key) = &name
                        && let Expr::Member(frame, member) = &**target
                        && member == "fields"
                        && let Expr::Name(frame) = &**frame
                        && Some(frame.as_str()) == return_frame
                        && let Expr::String(key) = &**key
                    {
                        env.fields.insert((frame.clone(), key.clone()), (value, kind));
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
                        let (fields, children) = handed_back(env, frame, lang);
                        if fields.iter().any(|key| key != "output") || !children.is_empty() {
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
                                    .chain(children.into_iter().map(|(_, field, value, _)| (field, value)))
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
                Statement::Break => output.push(Statement::Break),
                Statement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let mut yes = env.clone();
                    let mut no = env.clone();
                    // what a scope runs after has run whenever the scope does
                    if let Expr::Call { function, args, .. } = &condition
                        && matches!(&**function, Expr::Name(n) if n == "_boolean")
                        && let Some(Expr::Read {
                            dependency: Dependency::Field(frame, field),
                            ..
                        }) = args.first()
                        && field == "included"
                        && let Some(records) = env.guarded.get(frame)
                    {
                        yes.present.extend(records.iter().cloned());
                    }
                    let condition = expression(condition, env, lang).0;
                    let then_body = block(then_body, &mut yes, lang, return_frame);
                    let else_body = block(else_body, &mut no, lang, return_frame);
                    for (name, kind) in &mut env.names {
                        if let (Some(a), Some(b)) = (yes.names.get(name), no.names.get(name)) {
                            *kind = join(a.clone(), b.clone());
                        }
                    }
                    env.steps.extend(yes.steps);
                    env.steps.extend(no.steps);
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
        declare_before_reads(&mut output, &env.eager);
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

    // A value is taken where a statement is sure to read it. A statement before that may read it
    // on one side of a choice, itself or through a deferred value, so it is declared before that one.
    fn declare_before_reads(output: &mut Vec<Statement>, eager: &HashSet<String>) {
        let deferred = |statement: &Statement| {
            matches!(statement, Statement::Variable(Variable::Declare { value: Expr::Call { function, .. }, .. })
                if matches!(&**function, Expr::Name(name) if name == "_lazy"))
        };
        loop {
            let through: HashMap<String, HashSet<Dependency>> = output
                .iter()
                .filter(|statement| deferred(statement))
                .filter_map(|statement| match statement {
                    Statement::Variable(Variable::Declare { name, value, .. }) => Some((name.clone(), value.references())),
                    _ => None,
                })
                .collect();
            let reads = |statement: &Statement| {
                let mut found = code::references(std::slice::from_ref(statement));
                let mut pending: Vec<_> = found.iter().cloned().collect();
                while let Some(read) = pending.pop() {
                    if let Dependency::Variable(name) = &read
                        && let Some(more) = through.get(name)
                    {
                        pending.extend(more.iter().filter(|more| found.insert((*more).clone())).cloned());
                    }
                }
                found
            };
            let late = output.iter().enumerate().find_map(|(index, statement)| {
                let Statement::Variable(Variable::Declare { name, .. }) = statement else {
                    return None;
                };
                if !eager.contains(name) || deferred(statement) {
                    return None;
                }
                let own = Dependency::Variable(name.clone());
                output[..index]
                    .iter()
                    .position(|earlier| {
                        !deferred(earlier) && !matches!(earlier, Statement::Function(_)) && reads(earlier).contains(&own)
                    })
                    .map(|first| (index, first))
            });
            let Some((index, mut first)) = late else { break };
            // a choice keeps the name it assigns directly above it
            if first > 0
                && matches!(&output[first], Statement::If { .. })
                && matches!(
                    &output[first - 1],
                    Statement::Variable(Variable::Declare {
                        value: Expr::Null,
                        mutable: true,
                        ..
                    })
                )
            {
                first -= 1;
            }
            let declaration = output.remove(index);
            output.insert(first, declaration);
        }
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
        // the steps here that hand nothing back, which are run and not kept
        let mut silent = Vec::new();
        let mut child_fields = HashMap::new();
        let mut keys: Vec<_> = children.keys().collect();
        keys.sort();
        for key in keys {
            // Only direct business children have ordinary result bindings. Nested
            // structural selections keep their established representation.
            let result = label(env, key, "output", lang);
            let kind = match &children[key] {
                _ if env.records.contains_key(key) => Type::Record(Box::new(env.records[key].shape.clone())),
                _ if env.collections.contains_key(key) => {
                    Type::List(Box::new(Type::Record(Box::new(env.collections[key].1.clone()))))
                }
                // a step that hands back more than its output keeps that shape
                Type::Span(fields, _) => match env.steps.get(key) {
                    Some(step) if carries(step) => step.clone(),
                    _ => fields.get("output").cloned().unwrap_or(Type::Null),
                },
                Type::Optional(child) => match &**child {
                    Type::Span(fields, _) => join(
                        Type::Null,
                        match env.steps.get(key) {
                            Some(step) if carries(step) => step.clone(),
                            _ => fields.get("output").cloned().unwrap_or(Type::Null),
                        },
                    ),
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
            if !fields.is_empty() && matches!(kind, Type::Null) {
                silent.push(result);
            } else {
                results.push((result, field.clone()));
            }
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
        if results
            .iter()
            .map(|(name, _)| name)
            .chain(&silent)
            .any(|name| !present(&function.body, name))
        {
            return None;
        }
        // nothing reads the name such a step is called under, so the call is left a statement later
        code::rewrite(&mut function.body, &mut |value| {
            if matches!(value, Expr::Name(name) if silent.contains(name)) {
                *value = Expr::Null;
            }
        });
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
            gate: env.gates.get(&frame).cloned(),
        })
    }
    fn carries(kind: &Type) -> bool {
        match kind {
            Type::Product(step) => step.bundled(),
            Type::Optional(inner) => carries(inner),
            _ => false,
        }
    }
    // The conditions that must all hold for a scope to run, when nothing is left to chance. Each is
    // one name or comparison, so the same condition reads the same wherever it is written.
    fn conjuncts(gate: &Expr) -> Option<Vec<String>> {
        fn settled(value: &Expr) -> bool {
            match value {
                Expr::Name(_) | Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
                Expr::Index(target, index) => settled(target) && settled(index),
                Expr::Member(target, _) => settled(target),
                Expr::Binary { left, right, .. } => settled(left) && settled(right),
                Expr::Unary { value, .. } => settled(value),
                Expr::Call {
                    function,
                    args,
                    keywords,
                } => {
                    keywords.is_empty()
                        && matches!(&**function, Expr::Name(name) if args.is_empty() || name == "_boolean" || name == "_equal")
                        && args.iter().all(settled)
                }
                _ => false,
            }
        }
        fn split(value: &Expr, found: &mut Vec<String>) -> bool {
            match value {
                Expr::Bool(true) => true,
                Expr::Binary {
                    left,
                    op: BinOp::And,
                    right,
                } => split(left, found) && split(right, found),
                Expr::Call { function, args, .. }
                    if args.len() == 1 && matches!(&**function, Expr::Name(name) if name == "_boolean") =>
                {
                    split(&args[0], found)
                }
                // a value is the same whether it is read by name or taken where it is first needed
                Expr::Call { function, args, .. } if args.is_empty() && matches!(&**function, Expr::Name(_)) => {
                    split(function, found)
                }
                other => {
                    found.push(format!("{other:?}"));
                    settled(other)
                }
            }
        }
        let Expr::Call { function, args, .. } = gate else {
            return None;
        };
        let Some(Expr::Conditional { condition, yes, no }) = args.first() else {
            return None;
        };
        if !matches!(&**function, Expr::Name(name) if name == "_include")
            || !matches!(&**yes, Expr::Number(n) if n == "1")
            || !matches!(&**no, Expr::Number(n) if n == "0")
        {
            return None;
        }
        let mut found = Vec::new();
        split(condition, &mut found).then_some(found)
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
    // The statements that use a value on the paths where it is not null. A value that is
    // never null, or null down both sides of a choice, has no such form.
    fn where_present(value: &Expr, used: &impl Fn(Expr) -> Statement) -> Option<Vec<Statement>> {
        fn nullable(value: &Expr) -> bool {
            match value {
                Expr::Null => true,
                Expr::Conditional { yes, no, .. } => nullable(yes) || nullable(no),
                _ => false,
            }
        }
        fn paths(value: &Expr, used: &impl Fn(Expr) -> Statement) -> Option<Vec<Statement>> {
            Some(match value {
                Expr::Null => vec![],
                Expr::Conditional { condition, yes, no } => {
                    let then_body = paths(yes, used)?;
                    let else_body = paths(no, used)?;
                    match (then_body.is_empty(), else_body.is_empty()) {
                        (true, true) => return None,
                        (true, false) => vec![Statement::If {
                            condition: negated((**condition).clone()),
                            then_body: else_body,
                            else_body: vec![],
                        }],
                        _ => vec![Statement::If {
                            condition: (**condition).clone(),
                            then_body,
                            else_body,
                        }],
                    }
                }
                value => vec![used(value.clone())],
            })
        }
        if matches!(value, Expr::Null) {
            return Some(vec![]);
        }
        if !matches!(value, Expr::Conditional { .. }) || !nullable(value) {
            return None;
        }
        paths(value, used)
    }
    fn negated(value: Expr) -> Expr {
        match value {
            Expr::Unary { op: UnaryOp::Not, value } => *value,
            Expr::Binary {
                left,
                op: BinOp::Eq,
                right,
            } => left.binary(BinOp::Ne, *right),
            Expr::Binary {
                left,
                op: BinOp::Ne,
                right,
            } => left.binary(BinOp::Eq, *right),
            value => value.not(),
        }
    }
    // Move a cached initializer to its first guaranteed evaluation, without
    // crossing an earlier fallible operation or an unselected branch.
    fn materialize(value: &Expr, env: &mut Environment, output: &mut Vec<Statement>, lang: ScorerLang) {
        fn first(value: &Expr, env: &Environment, lang: ScorerLang) -> Result<Option<String>, ()> {
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
                if let Some(name) = first(child, env, lang)? {
                    return Ok(Some(name));
                }
            }
            if infallible(value, env, lang) { Ok(None) } else { Err(()) }
        }
        while let Ok(Some(name)) = first(value, env, lang) {
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
    fn constants(value: Expr, env: &Environment, lang: ScorerLang) -> Expr {
        match value {
            Expr::Call {
                ref function, ref args, ..
            } if matches!(&**function,Expr::Name(n) if n=="_lazy") => {
                if let Some(Expr::Lambda { value: inner, .. }) = args.first()
                    && infallible(inner, env, lang)
                {
                    return constants((**inner).clone(), env, lang);
                }
                value
            }
            Expr::Array(items) => Expr::Array(items.into_iter().map(|v| constants(v, env, lang)).collect()),
            Expr::Object(fields) => Expr::Object(fields.into_iter().map(|(k, v)| (k, constants(v, env, lang))).collect()),
            value => value,
        }
    }
    fn materialized(kind: &Type) -> bool {
        match kind {
            Type::Unknown | Type::Deferred(_) | Type::Span(..) | Type::Record(_) | Type::Product(_) => false,
            Type::List(kind) | Type::Rest(kind) => materialized(kind),
            Type::Optional(kind) => materialized(kind),
            Type::Array(items) => items.iter().all(materialized),
            Type::Object(fields) => fields.values().all(materialized),
            _ => true,
        }
    }
    // Whether a value is sure to be computed without raising, so that nothing is lost by taking it
    // before it is read. A value that can raise is never taken on a path that does not read it.
    fn infallible(value: &Expr, env: &Environment, lang: ScorerLang) -> bool {
        let sure = |value: &Expr| infallible(value, env, lang);
        let kind = |value: &Expr| expression(value.clone(), env, lang).1;
        match value {
            Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
            Expr::Name(name) => env.names.contains_key(name),
            Expr::Array(items) => items.iter().all(sure),
            Expr::Object(fields) => fields.iter().all(|(_, v)| sure(v)),
            Expr::Template(items) => items.iter().all(sure),
            // a list spreads into its items
            Expr::Spread(spread) => sure(spread) && matches!(kind(spread), Type::Array(_) | Type::List(_)),
            Expr::Conditional { condition, yes, no } => sure(condition) && sure(yes) && sure(no),
            Expr::Binary {
                left,
                op: BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or,
                right,
            } => sure(left) && sure(right),
            Expr::Unary { op: UnaryOp::Not, value } => sure(value),
            // a float literal is a constant in either language, though TypeScript marks its kind
            Expr::Call { .. } if constant(value) => true,
            // and so is text written around a draw
            Expr::Call { .. } | Expr::Binary { op: BinOp::Add, .. } if drawn_text(value) => true,
            // strings join, and whole numbers within bounds stay whole numbers
            Expr::Binary {
                left,
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Rem,
                right,
            } => {
                sure(left)
                    && sure(right)
                    && match kind(value) {
                        Type::String => matches!((kind(left), kind(right)), (Type::String, Type::String)),
                        found @ Type::Integer(..) => safe(&found),
                        _ => false,
                    }
            }
            // a key the object is known to have, and a place the list is known to reach
            Expr::Index(target, index) => {
                sure(target)
                    && match (kind(target), &**index) {
                        (Type::Object(fields), Expr::String(key)) => fields.contains_key(key),
                        (Type::Array(items), Expr::Number(at)) => {
                            at.parse::<usize>().is_ok_and(|at| placed(&items, at).is_some())
                        }
                        _ => false,
                    }
            }
            Expr::Member(target, key) if key == "length" => {
                sure(target) && matches!(kind(target), Type::Array(_) | Type::List(_) | Type::String)
            }
            Expr::Call {
                function,
                args,
                keywords,
            } if keywords.is_empty() && matches!(&**function, Expr::Name(_)) => {
                let Expr::Name(name) = &**function else { unreachable!() };
                let kinds: Vec<Type> = args.iter().map(&kind).collect();
                match (name.as_str(), args.as_slice()) {
                    // a draw held within written bounds is as sure as the draw, and so is any number
                    ("_clamp", [held, low, high]) => {
                        matches!((written(low), written(high)), (Some(low), Some(high)) if low <= high)
                            && (sampled(held) || sure(held) && span(&kinds[0]).is_some())
                    }
                    // what these write out is a string, a boolean or a number both languages hold
                    ("_text", [written]) => sure(written) && (matches!(kinds[0], Type::String | Type::Bool) || safe(&kinds[0])),
                    ("str" | "String" | "len", [written]) => sure(written),
                    ("_floor" | "_ceil" | "_round", [number]) => sure(number) && safe(&kinds[0]),
                    // arithmetic that is known to stay within bounds
                    ("_arithmetic", [_, left, right]) => sure(left) && sure(right) && span(&kind(value)).is_some(),
                    ("checked_number", [Expr::Binary { left, right, .. }]) => {
                        sure(left) && sure(right) && span(&kind(value)).is_some()
                    }
                    ("trace_of" | "traceOf", _) => true,
                    // a value still waiting to be read is as sure as what it computes
                    (name, []) if env.pending.contains_key(name) => sure(&env.pending[name]),
                    // a provider helper says which of its calls cannot raise
                    _ => match facts(env, name, args, &kinds) {
                        Some((helper, facts)) => {
                            let given = env.counts.called(name, args).map(|(_, given)| given).unwrap_or_default();
                            helper.sure.is_some_and(|sure| sure(&facts)) && given.iter().all(sure)
                        }
                        None => sampled(value),
                    },
                }
            }
            _ => sampled(value),
        }
    }
    // the number a literal writes, under TypeScript's mark for a float or not
    fn written(value: &Expr) -> Option<f64> {
        match value {
            Expr::Number(number) => number.parse().ok().filter(|number: &f64| number.is_finite()),
            Expr::Call { function, args, .. } if matches!(&**function, Expr::Name(name) if name == "_float") => {
                match args.as_slice() {
                    [value] => written(value),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    // text joined from written pieces and draws
    fn drawn_text(value: &Expr) -> bool {
        match value {
            Expr::String(_) => true,
            Expr::Call {
                function,
                args,
                keywords,
            } => {
                // a draw of text is text already
                keywords.is_empty()
                    && match &**function {
                        Expr::Name(name) if name == "_text" => matches!(args.as_slice(), [value] if sampled(value)),
                        Expr::Name(name) => ["_random_text", "_uuid"].contains(&name.as_str()) && sampled(value),
                        _ => false,
                    }
            }
            Expr::Binary {
                left,
                op: BinOp::Add,
                right,
            } => drawn_text(left) && drawn_text(right),
            _ => false,
        }
    }
    fn constant(value: &Expr) -> bool {
        match value {
            Expr::String(_) | Expr::Number(_) | Expr::Bool(_) | Expr::Null => true,
            Expr::Array(items) => items.iter().all(constant),
            Expr::Object(fields) => fields.iter().all(|(_, value)| constant(value)),
            Expr::Call { function, args, .. } => {
                matches!(&**function, Expr::Name(name) if name == "_float") && args.iter().all(constant)
            }
            _ => false,
        }
    }
    // A draw from constant parameters cannot fail, so nothing is lost by taking it where it is declared.
    fn sampled(value: &Expr) -> bool {
        let Expr::Call { function, args, .. } = value else {
            return false;
        };
        let Expr::Name(name) = &**function else { return false };
        // the random source is read by name, and through the context once the agent has one
        let random = |value: &Expr| {
            matches!(value, Expr::Name(name) if name == "random_source" || name == "randomSource")
                || matches!(value, Expr::Member(target, field) if field == "random" && matches!(&**target, Expr::Name(name) if name == "context"))
        };
        let options = |value: &Expr| {
            matches!(value, Expr::Array(items) if items.iter().all(|item| {
                matches!(item, Expr::Call { function, args, .. }
                    if matches!(&**function, Expr::Name(name) if name == "_lazy")
                        && matches!(args.as_slice(), [Expr::Lambda { parameters, value }] if parameters.is_empty() && constant(value)))
            }))
        };
        match (name.as_str(), args.as_slice()) {
            ("_include" | "_range" | "_uuid" | "_sample" | "_random_text" | "_pick", args) => {
                args.iter().all(|arg| constant(arg) || random(arg))
            }
            ("_weighted", [weights, choices, source]) => constant(weights) && options(choices) && random(source),
            ("_select", [pick, choices]) => sampled(pick) && options(choices),
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
                let t = (v.parse::<i128>().map(|v| Type::Integer(v, v)))
                    .or_else(|_| v.parse::<f64>().map(|v| Type::Real(v, v)))
                    .unwrap_or(Type::Number);
                (Expr::Number(v), t)
            }
            Expr::Read { dependency, value } => {
                if let Dependency::Child(_, child) = &dependency
                    && let Some((value, shape)) = env.collections.get(child)
                {
                    return (value.clone(), Type::List(Box::new(Type::Record(Box::new(shape.clone())))));
                }
                if let Dependency::Child(_, child) = &dependency
                    && let Some(record) = env.records.get(child)
                {
                    return (record.value.clone(), Type::Record(Box::new(record.shape.clone())));
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
                        && !carries(kind)
                        && let Expr::Lambda { parameters, value } = &selection[1]
                        && let Some(parameter) = parameters.first()
                        && matches!(unwrapped(value), Expr::Member(target, field) if field == "output" && matches!(&**target, Expr::Name(n) if n == &parameter.name))
                    {
                        if record.shape.silent(kind) {
                            return (Expr::Null, Type::Null);
                        }
                        return (record.value.clone().index(Expr::string(field)), kind.clone());
                    }
                }
                // a step that already ran is read through the values it produced
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
                    && !matches!(unwrapped(value), Expr::Member(..))
                {
                    let mut local = env.clone();
                    let mut known = false;
                    for ((frame, field), found) in &env.fields {
                        if frame == key {
                            local.fields.insert((parameter.name.clone(), field.clone()), found.clone());
                            known = true;
                        }
                    }
                    if let Some(found) = env.outputs.get(key) {
                        local.outputs.insert(parameter.name.clone(), found.clone());
                        known = true;
                    }
                    if known {
                        let (result, kind) = expression((**value).clone(), &local, lang);
                        let leaks = result.references().iter().any(|dependency| match dependency {
                            Dependency::Variable(name) | Dependency::Field(name, _) | Dependency::Child(name, _) => {
                                name == &parameter.name
                            }
                        });
                        if !leaks {
                            return (result, kind);
                        }
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
                            // every item of a resolved literal is evaluated, so none needs deferring
                            fn plain(value: Expr) -> Expr {
                                match value {
                                    Expr::Array(items) => Expr::Array(
                                        items
                                            .into_iter()
                                            .map(|item| match item {
                                                Expr::Call { function, mut args, .. }
                                                    if matches!(&*function, Expr::Name(name) if name == "_lazy")
                                                        && matches!(args.as_slice(), [Expr::Lambda { parameters, .. }] if parameters.is_empty()) =>
                                                {
                                                    match args.remove(0) {
                                                        Expr::Lambda { value, .. } => plain(*value),
                                                        other => other,
                                                    }
                                                }
                                                other => plain(other),
                                            })
                                            .collect(),
                                    ),
                                    Expr::Object(fields) => {
                                        Expr::Object(fields.into_iter().map(|(key, value)| (key, plain(value))).collect())
                                    }
                                    other => other,
                                }
                            }
                            let value = constants(plain(value.clone()), env, lang);
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
                        ("_num", [value], [kind @ (Type::Integer(..) | Type::Real(..) | Type::Number)]) => {
                            return (value.clone(), kind.clone());
                        }
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
                        // a list is already a list
                        ("_array", [value], [kind @ (Type::Array(_) | Type::List(_))]) if materialized(kind) => {
                            return (value.clone(), kind.clone());
                        }
                        // an item of a list written as one spread is an item of what was spread
                        ("_index", [Expr::Array(items), index], [Type::Array(_) | Type::List(_), _])
                            if matches!(items.as_slice(), [Expr::Spread(_)]) =>
                        {
                            let Some(Expr::Spread(spread)) = items.first() else {
                                unreachable!()
                            };
                            return expression(Expr::call("_index", vec![(**spread).clone(), index.clone()]), env, lang);
                        }
                        // a key of an object written out is what was written under it
                        ("_index", [Expr::Object(fields), Expr::String(key)], [Type::Object(kinds), _])
                            if fields.iter().filter(|(found, _)| found == key).count() == 1
                                && fields.iter().all(|(_, value)| steady(value))
                                && kinds.get(key).is_some_and(materialized) =>
                        {
                            let (_, value) = fields.iter().find(|(found, _)| found == key).unwrap();
                            return (value.clone(), kinds[key].clone());
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
                                && let Some(kind) = placed(items, i)
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
                        ("_len", [value], [Type::Array(items)])
                            if items.iter().all(materialized) && !items.iter().any(|item| matches!(item, Type::Rest(_))) =>
                        {
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
                        // typescript keeps float markers when arithmetic produces a whole number
                        ("_arithmetic", [Expr::String(op), left, right], [_, a, b])
                            if lang == ScorerLang::Python
                                && matches!(a, Type::Number | Type::Real(..) | Type::Integer(..))
                                && matches!(b, Type::Number | Type::Real(..) | Type::Integer(..))
                                && (matches!(a, Type::Number | Type::Real(..))
                                    || matches!(b, Type::Number | Type::Real(..))) =>
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
                            return (value, reach(op, a, b).unwrap_or(Type::Number));
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
                        ("_read", [value, Expr::String(key)], [Type::Record(branch), _]) => {
                            if let Some(kind) = branch.fields.get(key) {
                                return (value.clone().index(Expr::string(key)), kind.clone());
                            }
                        }
                        ("_len", [Expr::Map { iterable, .. }], _) => {
                            return expression(Expr::call("_len", vec![(**iterable).clone()]), env, lang);
                        }
                        ("_last", [value], [Type::List(kind)]) => {
                            let position = if lang == ScorerLang::Python {
                                Expr::number(-1)
                            } else {
                                value.clone().member("length").binary(BinOp::Sub, Expr::number(1))
                            };
                            return (value.clone().index(position), (**kind).clone());
                        }
                        ("_slice", [value, start, end], [kind @ Type::List(_), _, _])
                            if matches!(start, Expr::Null) && matches!(end, Expr::Null) =>
                        {
                            return (value.clone(), kind.clone());
                        }
                        ("_slice", _, [kind @ Type::List(_), ..]) => return (call, kind.clone()),
                        ("_child", [scope, Expr::String(key)], [Type::Record(branch), _]) => {
                            if let Some((field, kind)) = branch.children.get(key) {
                                return (
                                    if branch.silent(kind) {
                                        Expr::Null
                                    } else if branch.fields.is_empty() && branch.children.len() == 1 {
                                        scope.clone()
                                    } else {
                                        scope.clone().index(Expr::string(field))
                                    },
                                    // a nested scope keeps its own shape, a step is its result
                                    match kind {
                                        Type::Record(_) | Type::List(_) | Type::Product(_) => kind.clone(),
                                        // a step that ran whenever this is read is there to read
                                        Type::Optional(step)
                                            if carries(step)
                                                && matches!(scope, Expr::Name(name) if env.present.contains(name)) =>
                                        {
                                            (**step).clone()
                                        }
                                        _ if carries(kind) => kind.clone(),
                                        _ => Type::Product(Box::new(Step::plain(kind.clone()))),
                                    },
                                );
                            }
                        }
                        // a step that ran here and carries the steps inside it is read by its result
                        ("_child", [Expr::Name(_), Expr::String(key)], [Type::Span(..), _])
                            if env.steps.get(key).is_some_and(carries)
                                && env.names.contains_key(&label(env, key, "output", lang)) =>
                        {
                            return (Expr::Name(label(env, key, "output", lang)), env.steps[key].clone());
                        }
                        ("_child", [scope, Expr::String(key)], [Type::Product(step), _]) => {
                            if let Some((field, kind)) = step.children.get(key) {
                                return (scope.clone().index(Expr::string(field)), kind.clone());
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
                                Type::Optional(inner) if matches!(&**inner, Type::Span(..) | Type::Product(_)) => {
                                    ((**inner).clone(), true)
                                }
                                _ => (Type::Unknown, false),
                            };
                            if !matches!(span, Type::Unknown)
                                && let Some(parameter) = parameters.first()
                            {
                                let mut local = env.clone();
                                local.names.insert(parameter.name.clone(), span);
                                if matches!(scope, Expr::Name(name) if env.present.contains(name)) {
                                    local.present.insert(parameter.name.clone());
                                }
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
                    // a provider helper says what its own call returns
                    if let Some((helper, facts)) = facts(env, &name, &args, &kinds)
                        && let Some(returns) = helper.returns
                    {
                        let kind = learned(&returns(&facts));
                        if !matches!(kind, Type::Unknown) {
                            return (call, kind);
                        }
                    }
                    let kind = match name.as_str() {
                        "_text" | "_trim" | "_upper" | "_lower" | "_replace" | "_join" | "_render" | "_compose"
                        | "_random_text" => Type::String,
                        "_len" | "_count" | "_pick" => Type::Integer(0, 9_007_199_254_740_991),
                        "_integer" => Type::Integer(-9_007_199_254_740_991, 9_007_199_254_740_991),
                        "_value" => kinds.first().map(normalized).unwrap_or(Type::Value),
                        "_index" if kinds.first().is_some_and(materialized) => Type::Value,
                        "_at" => Type::Value,
                        "_duration" => Type::Value,
                        // arithmetic on bounded numbers stays within bounds
                        "_arithmetic" => match (args.first(), kinds.as_slice()) {
                            (Some(Expr::String(op)), [_, a, b]) => reach(op, a, b).unwrap_or(Type::Number),
                            _ => Type::Number,
                        },
                        "checked_number" => match args.as_slice() {
                            [Expr::Binary { left, op, right }] => {
                                let sign = match op {
                                    BinOp::Add => "+",
                                    BinOp::Sub => "-",
                                    BinOp::Mul => "*",
                                    _ => "",
                                };
                                let a = expression((**left).clone(), env, lang).1;
                                let b = expression((**right).clone(), env, lang).1;
                                reach(sign, &a, &b).unwrap_or(Type::Number)
                            }
                            _ => Type::Number,
                        },
                        "_floor" | "_ceil" | "_round" => match kinds.first() {
                            Some(kind) if safe(kind) => {
                                let (low, high) = span(kind).unwrap();
                                match name.as_str() {
                                    "_floor" => Type::Integer(low.floor() as i128, high.floor() as i128),
                                    "_ceil" => Type::Integer(low.ceil() as i128, high.ceil() as i128),
                                    _ => Type::Integer(low.floor() as i128, high.ceil() as i128),
                                }
                            }
                            _ => env.returns.get(&name).cloned().unwrap_or(Type::Unknown),
                        },
                        "_clamp" => match args.as_slice() {
                            [_, low, high] => match (written(low), written(high)) {
                                (Some(low), Some(high)) if low <= high => Type::Real(low, high),
                                _ => Type::Number,
                            },
                            _ => Type::Number,
                        },
                        "remainder" | "divide" | "_sample" | "_abs" | "_extreme" => Type::Number,
                        "_uuid" | "str" | "String" => Type::String,
                        "_float" => match args.as_slice() {
                            [value] => written(value).map(|value| Type::Real(value, value)).unwrap_or(Type::Number),
                            _ => Type::Number,
                        },
                        // which trace of a run the agent is
                        "trace_of" | "traceOf" => Type::Integer(0, SAFE),
                        "len" => Type::Integer(0, 9_007_199_254_740_991),
                        // these return what they were given, or resolve it first
                        "_slice" | "_array" if kinds.first().is_some_and(materialized) => Type::Value,
                        "_history" | "_emissions" => Type::Value,
                        // a fold resolves what it returns
                        "_fold" => Type::Value,
                        "_range" => match args.as_slice() {
                            [Expr::Number(low), Expr::Number(high), Expr::Number(whole), ..] if whole == "1" => {
                                match (low.parse::<i128>(), high.parse::<i128>()) {
                                    (Ok(low), Ok(high)) => Type::Integer(low, high),
                                    _ => Type::Number,
                                }
                            }
                            [low, high, ..] => match (written(low), written(high)) {
                                (Some(low), Some(high)) if low <= high => Type::Real(low, high),
                                _ => Type::Number,
                            },
                            _ => Type::Number,
                        },
                        // a choice is whichever option it takes
                        "_select" | "_weighted" => {
                            let choices = args.get(1);
                            let mut kinds = Vec::new();
                            if let Some(Expr::Array(items)) = choices {
                                for item in items {
                                    if let Expr::Call { args, .. } = item
                                        && let [Expr::Lambda { value, .. }] = args.as_slice()
                                    {
                                        kinds.push(expression((**value).clone(), env, lang).1);
                                    }
                                }
                            }
                            let all = kinds.len();
                            match kinds.into_iter().reduce(join) {
                                Some(kind) if all > 0 && materialized(&kind) => kind,
                                _ => Type::Unknown,
                            }
                        }
                        name if env.counts.counts.contains(name) => Type::Integer(0, 9_007_199_254_740_991),
                        name if env.counts.plain.contains(name) => Type::Value,
                        name if matches!(args.first(), Some(Expr::String(operation))
                            if env.counts.texts.iter().any(|(helper, known)| *helper == name && *known == operation.as_str())) =>
                        {
                            Type::String
                        }
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
                // a spread list of known length gives its items, one of any length stands for any number of them
                let mut kinds = Vec::new();
                for (value, kind) in &items {
                    match (value, kind) {
                        (Expr::Spread(_), Type::Array(spread)) => kinds.extend(spread.iter().cloned()),
                        (Expr::Spread(_), Type::List(item)) => kinds.push(Type::Rest(item.clone())),
                        (_, kind) => kinds.push(kind.clone()),
                    }
                }
                (Expr::Array(items.into_iter().map(|(v, _)| v).collect()), Type::Array(kinds))
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
                if op == BinOp::And && matches!(b, Type::Bool) && matches!(left, Expr::Bool(true)) {
                    return (right, Type::Bool);
                }
                if op == BinOp::Or && matches!(b, Type::Bool) && matches!(left, Expr::Bool(false)) {
                    return (right, Type::Bool);
                }
                if op == BinOp::Or && matches!(a, Type::Bool) && matches!(right, Expr::Bool(false)) {
                    return (left, Type::Bool);
                }
                if op == BinOp::And && matches!(a, Type::Bool) && matches!(right, Expr::Bool(true)) {
                    return (left, Type::Bool);
                }
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
                // a condition that is already decided leaves one side
                match condition {
                    Expr::Bool(true) => return expression(*yes, env, lang),
                    Expr::Bool(false) => return expression(*no, env, lang),
                    _ => {}
                }
                let (yes, a) = expression(*yes, env, lang);
                let (mut no, mut b) = expression(*no, env, lang);
                // a second test that is the first one turned around is already decided
                if let Expr::Conditional {
                    condition: second,
                    yes: taken,
                    ..
                } = &no
                    && steady(&condition)
                    && (format!("{:?}", negated(condition.clone())) == format!("{second:?}")
                        || format!("{condition:?}") == format!("{:?}", negated((**second).clone())))
                {
                    (no, b) = expression((**taken).clone(), env, lang);
                }
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
                if let Type::Product(step) = &t {
                    if !step.bundled() && key == "output" {
                        return (value, step.output.clone());
                    }
                    if step.bundled() {
                        return match step.fields.get(&key) {
                            Some(kind) => (value.index(Expr::string(export_name(&key))), kind.clone()),
                            None => (Expr::Null, Type::Null),
                        };
                    }
                }
                let t = match t {
                    Type::Object(fields) | Type::Span(fields, _) => fields.get(&key).cloned().unwrap_or(unknown),
                    // the length of a list or a string
                    _ if key == "length" => Type::Integer(0, 9_007_199_254_740_991),
                    _ => unknown,
                };
                (value.member(key), t)
            }
            Expr::Index(value, index) => {
                let (value, t) = expression(*value, env, lang);
                let (index, _) = expression(*index, env, lang);
                let t = match (&t, &index) {
                    (Type::Object(fields), Expr::String(key)) => fields.get(key).cloned().unwrap_or(unknown),
                    (Type::Array(items), Expr::Number(at)) => (at.parse::<usize>().ok().and_then(|at| placed(items, at)))
                        .cloned()
                        .unwrap_or(unknown),
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
            Expr::Spread(value) => {
                let (value, kind) = expression(*value, env, lang);
                // spreading a list gives its items, and spreading other plain data gives plain items
                let kind = match kind {
                    Type::Array(_) | Type::List(_) if materialized(&kind) => kind,
                    kind if materialized(&kind) => Type::Value,
                    _ => unknown,
                };
                (Expr::Spread(Box::new(value)), kind)
            }
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
    // the item a written list is sure to have at a place, which it is not past where any number of items begin
    fn placed(items: &[Type], at: usize) -> Option<&Type> {
        let sure = items
            .iter()
            .position(|item| matches!(item, Type::Rest(_)))
            .unwrap_or(items.len());
        items[..sure].get(at)
    }
    // one of the items a part of a written list stands for
    fn one(kind: Type) -> Type {
        match kind {
            Type::Rest(item) => *item,
            kind => kind,
        }
    }
    // the helper a call is to and what is known of each argument the function was called with
    fn facts<'a>(
        env: &'a Environment,
        name: &str,
        args: &[Expr],
        kinds: &[Type],
    ) -> Option<(&'a crate::mocking::Helper, Vec<crate::mocking::Known>)> {
        let (helper, given) = env.counts.called(name, args)?;
        let kinds: Vec<Type> = match (helper.operation, kinds.get(1)) {
            (Some(_), Some(Type::Array(kinds))) if kinds.len() == given.len() => kinds.clone(),
            (Some(_), _) => vec![Type::Unknown; given.len()],
            (None, _) => kinds.iter().take(given.len()).cloned().collect(),
        };
        Some((
            helper,
            given.iter().zip(&kinds).map(|(value, kind)| fact(value, kind)).collect(),
        ))
    }
    // whether a test gives the same answer however often it is made
    fn steady(value: &Expr) -> bool {
        let mut steady = true;
        code::rewrite_expr(&mut value.clone(), &mut |found| {
            if let Expr::Call { function, args, .. } = found {
                steady &= matches!(&**function, Expr::Name(name)
                    if args.is_empty() || ["_equal", "_boolean", "_num", "_index", "_text", "_len"].contains(&name.as_str()));
            }
            steady &= !matches!(found, Expr::New { .. } | Expr::Lambda { .. });
        });
        steady
    }
    // the bounds of arithmetic on two bounded numbers, when the result is sure to be a number both languages hold
    fn reach(op: &str, a: &Type, b: &Type) -> Option<Type> {
        if !safe(a) || !safe(b) {
            return None;
        }
        let ((a, b), (c, d)) = (span(a)?, span(b)?);
        let (low, high) = match op {
            "+" => (a + c, b + d),
            "-" => (a - d, b - c),
            "*" => {
                let ends = [a * c, a * d, b * c, b * d];
                (
                    ends.iter().copied().fold(f64::INFINITY, f64::min),
                    ends.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                )
            }
            "/" if c > 0.0 || d < 0.0 => {
                let ends = [a / c, a / d, b / c, b / d];
                (
                    ends.iter().copied().fold(f64::INFINITY, f64::min),
                    ends.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                )
            }
            _ => return None,
        };
        let kind = Type::Real(low, high);
        safe(&kind).then_some(kind)
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
                | (
                    Type::Integer(..) | Type::Real(..) | Type::Number,
                    Type::Integer(..) | Type::Real(..) | Type::Number
                )
                | (Type::Null, Type::Null)
        ) || matches!(a, Type::Null)
            && matches!(
                b,
                Type::Bool | Type::String | Type::Integer(..) | Type::Real(..) | Type::Number
            )
            || matches!(b, Type::Null)
                && matches!(
                    a,
                    Type::Bool | Type::String | Type::Integer(..) | Type::Real(..) | Type::Number
                )
    }
    fn normalized(t: &Type) -> Type {
        match t {
            Type::Unknown => Type::Value,
            Type::Optional(t) => Type::Optional(Box::new(normalized(t))),
            Type::Deferred(t) => normalized(t),
            Type::Rest(t) => Type::Rest(Box::new(normalized(t))),
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
            (Type::Number, Type::Number | Type::Real(..) | Type::Integer(..))
            | (Type::Real(..) | Type::Integer(..), Type::Number) => Type::Number,
            (a @ (Type::Real(..) | Type::Integer(..)), b @ Type::Real(..)) | (a @ Type::Real(..), b @ Type::Integer(..)) => {
                let ((a, b), (c, d)) = (span(&a).unwrap(), span(&b).unwrap());
                Type::Real(a.min(c), b.max(d))
            }
            (Type::Bool, Type::Bool) => Type::Bool,
            (Type::String, Type::String) => Type::String,
            (Type::Null, Type::Null) => Type::Null,
            (Type::Integer(a, b), Type::Integer(c, d)) => Type::Integer(a.min(c), b.max(d)),
            (Type::Object(a), Type::Object(b)) => Type::Object(
                a.into_iter()
                    .filter_map(|(k, v)| b.get(&k).map(|w| (k, join(v, w.clone()))))
                    .collect(),
            ),
            // lists written alike are alike item by item, and otherwise a list of what either holds
            (Type::Array(a), Type::Array(b))
                if a.len() == b.len() && !a.iter().chain(&b).any(|item| matches!(item, Type::Rest(_))) =>
            {
                Type::Array(a.into_iter().zip(b).map(|(a, b)| join(a, b)).collect())
            }
            (a @ (Type::Array(_) | Type::List(_)), b @ (Type::Array(_) | Type::List(_)))
                if materialized(&a) && materialized(&b) =>
            {
                let item = |list: Type| match list {
                    Type::Array(items) => items.into_iter().map(one).reduce(join),
                    Type::List(item) => Some(*item),
                    _ => None,
                };
                match (item(a), item(b)) {
                    (Some(a), Some(b)) => Type::List(Box::new(join(a, b))),
                    (Some(item), None) | (None, Some(item)) => Type::List(Box::new(item)),
                    (None, None) => Type::Array(vec![]),
                }
            }
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
            Func::ForkPick(branches) => Expr::call(
                "_fork_pick",
                vec![
                    Expr::Array(
                        branches
                            .iter()
                            .map(|branch| Ok(Expr::Array(branch.iter().map(expr).collect::<Result<_, String>>()?)))
                            .collect::<Result<_, String>>()?,
                    ),
                    random(),
                ],
            ),
            Func::Arrivals { ready } => call("_require_arrivals", &[ready])?,
            Func::Fold { items, initial, combine } => Expr::call(
                "_fold",
                vec![
                    expr(items)?,
                    expr(initial)?,
                    Expr::Lambda {
                        parameters: vec![code::Parameter::new("accumulator"), code::Parameter::new("arrival")],
                        value: Box::new(expr(combine)?),
                    },
                ],
            ),
            Func::FoldAccumulator => "accumulator".into(),
            Func::FoldArrival => "arrival".into(),
            Func::Emissions(join) => {
                let optional =
                    |value: &Option<Value>| Ok::<_, String>(value.as_ref().map(expr).transpose()?.unwrap_or(Expr::Null));
                let inputs = (join.inputs.iter())
                    .map(|input| {
                        Ok(Expr::Object(vec![
                            ("edge".into(), expr(&input.edge)?),
                            ("arrived".into(), optional(&input.arrived)?),
                            ("value".into(), optional(&input.value)?),
                            ("shared".into(), Expr::Bool(input.shared)),
                        ]))
                    })
                    .collect::<Result<_, String>>()?;
                Expr::call(
                    "_emissions",
                    vec![
                        Expr::string(format!("{:?}", join.kind).to_lowercase()),
                        Expr::Array(inputs),
                        Expr::Array(join.order.iter().map(Expr::number).collect()),
                        optional(&join.rounds)?,
                        optional(&join.keys)?,
                        optional(&join.count)?,
                    ],
                )
            }
            Func::Check { value, ty } => Expr::call(
                "_check_type",
                vec![expr(value)?, Expr::string(format!("{ty:?}").to_lowercase())],
            ),
            Func::Len { target } => call("_len", &[target])?,
            Func::Upper { text } => call("_upper", &[text])?,
            Func::Lower { text } => call("_lower", &[text])?,
            Func::Trim { text } => call("_trim", &[text])?,
            Func::Replace { text, from, to } => call("_replace", &[text, from, to])?,
            Func::Split { text, separator } => call("_split", &[text, separator])?,
            Func::Join { array, separator } => call("_join", &[array, separator])?,
            Func::Render { template, values } => call("_render", &[template, values])?,
            Func::Compose { array, separator } => call("_compose", &[array, separator])?,
            Func::Sample { array, count } => Expr::call("_sample_items", vec![expr(array)?, expr(count)?, random()]),
            Func::Contains { target, needle } => call("_contains", &[target, needle])?,
            Func::StartsWith { text, prefix } => call("_starts_with", &[text, prefix])?,
            Func::EndsWith { text, suffix } => call("_ends_with", &[text, suffix])?,
            Func::Format { pieces, args } => {
                // an empty piece adds nothing to the text
                let mut parts = vec![];
                for (i, piece) in pieces.iter().enumerate() {
                    if !piece.is_empty() {
                        parts.push(Expr::string(piece));
                    }
                    if let Some(value) = args.get(i) {
                        parts.push(Expr::call("_text", vec![expr(value)?]));
                    }
                }
                parts
                    .into_iter()
                    .reduce(|a, b| a.binary(BinOp::Add, b))
                    .unwrap_or_else(|| Expr::string(""))
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
            Func::Provider { function, args } => {
                let helper = function
                    .helper(lang)
                    .ok_or_else(|| format!("function `{}` is not supported by agent generation", function.name))?;
                let mut args = args.iter().map(expr).collect::<Result<Vec<_>, _>>()?;
                // a helper that serves several functions takes the operation and the arguments as one list
                if let Some(operation) = helper.operation {
                    args = vec![Expr::string(operation), Expr::Array(args)];
                }
                if helper.random {
                    args.push(random());
                }
                Expr::call(helper.name, args)
            }
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
        // calls to provider functions, none of which scorer code can run
        pub unsupported: bts_lang::Diags,
        loops: Vec<(String, String)>,
    }
    impl Compiler {
        pub fn new(lang: ScorerLang) -> Self {
            Self {
                lang,
                imports: BTreeSet::new(),
                helpers: BTreeSet::new(),
                next_loop: 0,
                unsupported: vec![],
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
                // the call is reported with its location and the rest of the scorer is still checked
                Value::Func {
                    func: Func::Provider { function, .. },
                    range,
                } if !function.runs(crate::mocking::Consumer::Scorer) => {
                    let diag = super::unsupported_call(function, crate::mocking::Consumer::Scorer, *range);
                    self.unsupported.push(diag);
                    Expr::Null
                }
                Value::Func { func, .. } => self.func(func)?,
                Value::Unary { op, operand, .. } => Expr::Unary {
                    op: *op,
                    value: Box::new(self.lower(operand)?),
                },
                // two integers divide to an integer, dropping the remainder toward zero
                Value::Binary {
                    op: BinOp::Div,
                    lhs,
                    rhs,
                    ..
                } if integer(lhs) && integer(rhs) => {
                    let quotient = self.lower(lhs)?.binary(BinOp::Div, self.lower(rhs)?);
                    if self.lang == ScorerLang::Python {
                        Expr::call("int", vec![quotient])
                    } else {
                        Expr::call(Expr::from("Math").member("trunc"), vec![quotient])
                    }
                }
                // a remainder takes the sign of what is divided, which python gives only when neither side is negative
                Value::Binary {
                    op: BinOp::Rem,
                    lhs,
                    rhs,
                    ..
                } if self.lang == ScorerLang::Python
                    && integer(lhs)
                    && integer(rhs)
                    && !(non_negative(lhs) && non_negative(rhs)) =>
                {
                    self.imports.insert("math");
                    let remainder = Expr::call(Expr::from("math").member("fmod"), vec![self.lower(lhs)?, self.lower(rhs)?]);
                    Expr::call("int", vec![remainder])
                }
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
        // keep authored numeric types until render or sample has checked them
        fn language_value(&mut self, value: &Value) -> Result<Expr, Error> {
            self.helpers.insert(Helper::Language);
            Ok(match value {
                Value::Num(Number::Float(value)) => Expr::call("_language_float", vec![Expr::Number(format!("{value:?}"))]),
                Value::Object(object) => Expr::Object(
                    object
                        .elem
                        .iter()
                        .map(|field| Ok((field.key.clone(), self.language_value(&field.value)?)))
                        .collect::<Result<_, Error>>()?,
                ),
                Value::Array(array) => Expr::Array(
                    array
                        .elem
                        .iter()
                        .map(|item| match item {
                            ArrayElem::Item(value) => self.language_value(value),
                            ArrayElem::Spread(value) => self.language_value(value).map(|value| Expr::Spread(Box::new(value))),
                        })
                        .collect::<Result<_, _>>()?,
                ),
                Value::Cond { cond, then, otherwise } => Expr::Conditional {
                    condition: Box::new(self.lower(cond)?),
                    yes: Box::new(self.language_value(then)?),
                    no: Box::new(self.language_value(otherwise)?),
                },
                Value::Index { target, index, .. } => self.language_value(target)?.index(self.lower(index)?),
                Value::Slice { target, start, end, .. } => Expr::Slice {
                    value: Box::new(self.language_value(target)?),
                    start: start.as_deref().map(|value| self.lower(value).map(Box::new)).transpose()?,
                    end: end.as_deref().map(|value| self.lower(value).map(Box::new)).transpose()?,
                },
                Value::Unary {
                    op: crate::dsl::UnaryOp::Neg,
                    operand,
                    ..
                } => Expr::call(
                    "_language_math",
                    vec![Expr::string("neg"), Expr::Array(vec![self.language_value(operand)?])],
                ),
                Value::Binary { op, lhs, rhs, .. }
                    if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem) =>
                {
                    let op = match op {
                        BinOp::Add => "+",
                        BinOp::Sub => "-",
                        BinOp::Mul => "*",
                        BinOp::Div => "/",
                        _ => "%",
                    };
                    Expr::call(
                        "_language_math",
                        vec![
                            Expr::string(op),
                            Expr::Array(vec![self.language_value(lhs)?, self.language_value(rhs)?]),
                        ],
                    )
                }
                Value::Func {
                    func: Func::Choice(options),
                    ..
                } => {
                    let values = Expr::Array(
                        options
                            .iter()
                            .map(|value| self.language_value(value))
                            .collect::<Result<_, _>>()?,
                    );
                    if self.lang == ScorerLang::Python {
                        self.imports.insert("random");
                        Expr::call(Expr::from("random").member("choice"), vec![values])
                    } else {
                        self.need(Helper::Choice);
                        Expr::call("choice", vec![values])
                    }
                }
                Value::Func {
                    func: Func::Weighted(options),
                    ..
                } => {
                    if self.lang == ScorerLang::Python {
                        self.imports.insert("random");
                        Expr::Call {
                            function: Box::new(Expr::from("random").member("choices")),
                            args: vec![Expr::Array(
                                options
                                    .iter()
                                    .map(|option| self.language_value(&option.value))
                                    .collect::<Result<_, _>>()?,
                            )],
                            keywords: vec![(
                                "weights".into(),
                                Expr::Array(
                                    options
                                        .iter()
                                        .map(|option| Expr::Number(format!("{:?}", option.weight)))
                                        .collect(),
                                ),
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
                                    .map(|option| {
                                        Ok(Expr::Array(vec![
                                            self.language_value(&option.value)?,
                                            Expr::Number(format!("{:?}", option.weight)),
                                        ]))
                                    })
                                    .collect::<Result<_, Error>>()?,
                            )],
                        )
                    }
                }
                Value::Func {
                    func: Func::Abs { value },
                    ..
                } => Expr::call(
                    "_language_math",
                    vec![Expr::string("abs"), Expr::Array(vec![self.language_value(value)?])],
                ),
                Value::Func {
                    func: Func::Min(values) | Func::Max(values),
                    ..
                } => {
                    let op = if matches!(value, Value::Func { func: Func::Min(_), .. }) {
                        "min"
                    } else {
                        "max"
                    };
                    Expr::call(
                        "_language_math",
                        vec![
                            Expr::string(op),
                            Expr::Array(
                                values
                                    .iter()
                                    .map(|value| self.language_value(value))
                                    .collect::<Result<_, _>>()?,
                            ),
                        ],
                    )
                }
                Value::Func {
                    func: Func::Clamp { value, min, max },
                    ..
                } => Expr::call(
                    "_language_math",
                    vec![
                        Expr::string("clamp"),
                        Expr::Array(vec![
                            self.language_value(value)?,
                            self.language_value(min)?,
                            self.language_value(max)?,
                        ]),
                    ],
                ),
                Value::Func {
                    func:
                        Func::Range(Range::Float { .. })
                        | Func::Normal { .. }
                        | Func::Lognormal { .. }
                        | Func::Exponential { .. }
                        | Func::Pareto { .. }
                        | Func::Beta { .. },
                    ..
                } => Expr::call("_language_float", vec![self.lower(value)?]),
                Value::Func {
                    func: Func::Sample { array, count },
                    ..
                } => {
                    let random = if self.lang == ScorerLang::Python {
                        self.imports.insert("random");
                        Expr::from("random").member("random")
                    } else {
                        Expr::from("Math").member("random")
                    };
                    Expr::call(
                        "_language_sample",
                        vec![self.language_value(array)?, self.language_value(count)?, random],
                    )
                }
                _ => self.lower(value)?,
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
                Func::Check { .. }
                | Func::Arrivals { .. }
                | Func::Fold { .. }
                | Func::FoldAccumulator
                | Func::FoldArrival
                | Func::Emissions(_)
                | Func::ForkPick(_) => {
                    return Err(Error::Unsupported {
                        what: "runtime schema checks",
                    });
                }
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
                Func::Render { template, values } => {
                    self.helpers.insert(Helper::Language);
                    Expr::call("_language_render", vec![self.lower(template)?, self.language_value(values)?])
                }
                Func::Compose { array, separator } => {
                    self.helpers.insert(Helper::Language);
                    Expr::call("_language_compose", vec![self.lower(array)?, self.lower(separator)?])
                }
                Func::Sample { array, count } => {
                    self.helpers.insert(Helper::Language);
                    let random = if python {
                        self.imports.insert("random");
                        Expr::from("random").member("random")
                    } else {
                        Expr::from("Math").member("random")
                    };
                    Expr::call(
                        "_language_sample",
                        vec![self.lower(array)?, self.language_value(count)?, random],
                    )
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
                // the capability check reports a call a scorer cannot make first, with its location
                Func::Provider { function, args } => {
                    let lang = if python { ScorerLang::Python } else { ScorerLang::Typescript };
                    let (Some(helper), Some(name)) = (function.helper(lang), function.scorer(lang)) else {
                        return Err(Error::Unsupported {
                            what: "a provider function",
                        });
                    };
                    // a scorer file has no runtime module, so the text that defines the call comes along
                    if let Some(runtime) = helper.runtime {
                        self.helpers.insert(Helper::Runtime(runtime));
                    }
                    let mut args = args.iter().map(|arg| self.lower(arg)).collect::<Result<Vec<_>, _>>()?;
                    if let Some(operation) = helper.operation {
                        args = vec![Expr::string(operation), Expr::Array(args)];
                    }
                    if helper.random {
                        args.push(if python {
                            self.imports.insert("random");
                            Expr::from("random").member("random")
                        } else {
                            Expr::from("Math").member("random")
                        });
                    }
                    Expr::call(name, args)
                }
            })
        }
    }

    // language-neutral runtime helpers a generated scorer may need; collected
    // during emission so only used snippets appear, in declaration order (normal
    // first, its dependents after)
    #[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
    pub(in crate::mocking::code) enum Helper {
        Language,
        // the text a provider's functions are defined in
        Runtime(crate::mocking::Runtime),
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

    // Every provider function call the scorers would emit, which scorer code cannot run.
    pub(crate) fn unsupported(name: &str, steps: &[ScorerStep], score: &Value) -> bts_lang::Diags {
        let mut compiler = Compiler::new(ScorerLang::Python);
        // a failure for another reason is reported when the module is emitted
        let _ = compiler.steps(steps, name);
        let _ = compiler.lower(score);
        compiler.unsupported
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
    // whether a value is an integer wherever it is computed, as far as the shape alone says
    fn integer(value: &Value) -> bool {
        match value {
            Value::Num(Number::Int(_)) => true,
            Value::CtxRef(CtxRef::RepeatIndex | CtxRef::RepeatCount) => true,
            Value::Unary {
                op: crate::dsl::UnaryOp::Neg,
                operand,
                ..
            } => integer(operand),
            Value::Binary { op, lhs, rhs, .. } => {
                matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem) && integer(lhs) && integer(rhs)
            }
            Value::Cond { then, otherwise, .. } => integer(then) && integer(otherwise),
            Value::Func { func, .. } => match func {
                Func::Len { .. }
                | Func::Poisson { .. }
                | Func::Round { .. }
                | Func::Floor { .. }
                | Func::Ceil { .. }
                | Func::Range(Range::Int { .. }) => true,
                Func::Abs { value } => integer(value),
                Func::Clamp { value, min, max } => integer(value) && integer(min) && integer(max),
                Func::Min(values) | Func::Max(values) | Func::Choice(values) => values.iter().all(integer),
                _ => false,
            },
            _ => false,
        }
    }
    // whether an integer is sure not to be negative
    fn non_negative(value: &Value) -> bool {
        match value {
            Value::Num(Number::Int(value)) => *value >= 0,
            Value::CtxRef(CtxRef::RepeatIndex | CtxRef::RepeatCount) => true,
            Value::Binary { op, lhs, rhs, .. } => match op {
                BinOp::Add | BinOp::Mul | BinOp::Div => non_negative(lhs) && non_negative(rhs),
                BinOp::Rem => non_negative(lhs),
                _ => false,
            },
            Value::Cond { then, otherwise, .. } => non_negative(then) && non_negative(otherwise),
            Value::Func { func, .. } => match func {
                Func::Len { .. } | Func::Poisson { .. } | Func::Abs { .. } => true,
                Func::Range(Range::Int { min, .. }) => *min >= 0,
                Func::Clamp { min, .. } => non_negative(min),
                Func::Min(values) | Func::Choice(values) => values.iter().all(non_negative),
                Func::Max(values) => values.iter().any(non_negative),
                _ => false,
            },
            _ => false,
        }
    }

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

pub(crate) fn cases(model: &Model, dataset: &crate::dsl::Dataset, seed: u64, lang: ScorerLang) -> Result<Vec<Json>, String> {
    fn safe_numbers(value: &Json) -> bool {
        match value {
            Json::Number(number) => {
                number
                    .as_i64()
                    .is_none_or(|n| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n))
                    && number.as_u64().is_none_or(|n| n <= 9_007_199_254_740_991)
            }
            Json::Array(values) => values.iter().all(safe_numbers),
            Json::Object(values) => values.values().all(safe_numbers),
            _ => true,
        }
    }

    let mut rows = Vec::new();
    for case in &dataset.cases {
        let context = |message: String| format!("generated eval dataset {:?}, case {:?}: {message}", dataset.name, case.name);
        let key = json!([dataset.name, case.name]).to_string();
        let hash = crate::mocking::data::stable_uuid(&key);
        let case_seed = seed ^ u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap());
        let row = crate::mocking::data::case_snapshot(model, case, case_seed)
            .map_err(|error| context(error.to_string()))?
            .project(&())
            .map_err(|_| context("local attachments cannot be embedded in generated eval cases".into()))?;
        if row.get("metadata").is_some_and(|metadata| !metadata.is_object()) {
            return Err(context("metadata must evaluate to an object".into()));
        }
        if row
            .get("tags")
            .is_some_and(|tags| !tags.as_array().is_some_and(|tags| tags.iter().all(Json::is_string)))
        {
            return Err(context("tags must evaluate to an array of strings".into()));
        }
        if lang == ScorerLang::Typescript && !safe_numbers(&row) {
            return Err(context("an integer exceeds TypeScript's safe integer range".into()));
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
