use crate::dsl::ast;
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::model::{
    Accessor, Array, ArrayElem, BinOp, Binding, Child, Choice, CtxRef, Field, Func, Maybe, Model, NodeId, Number,
    Object, ObjectField, Part, Range, RefId, Repeat, ResolvedRef, Selection, Span, SpanFields, SpanKind, Step,
    Template, Trace, UnaryOp, Value, WeightedOption,
};
use crate::dsl::spec;
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

enum FieldValue {
    Value(Value),
    Object(Object),
    Tags(Vec<Template>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExprType {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
    Func,
    // a residual expr whose concrete type is only known during generation
    Abstract,
}

impl ExprType {
    fn of(folded: &Folded) -> Self {
        match &folded.kind {
            FoldedKind::Array(_) => Self::Array,
            FoldedKind::Object(_) => Self::Object,
            FoldedKind::Value(value) => match value {
                Value::Str(_) | Value::Template(_) => Self::String,
                Value::Num(_) => Self::Number,
                Value::Bool(_) => Self::Boolean,
                Value::Null => Self::Null,
                Value::Array(_) => Self::Array,
                Value::Object(_) => Self::Object,
                Value::Func { .. } => Self::Func,
                // context indexes and counts are always integers
                Value::CtxRef(_) => Self::Number,
                // constant exprs fold to values, only dynamic ones get here
                Value::VarRef(_)
                | Value::BlockRef { .. }
                | Value::Unary { .. }
                | Value::Binary { .. }
                | Value::Cond { .. }
                | Value::Index { .. }
                | Value::Slice { .. } => Self::Abstract,
            },
        }
    }
}

impl fmt::Display for ExprType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Null => "null",
            Self::Array => "array",
            Self::Object => "object",
            Self::Func => "function",
            Self::Abstract => "expression",
        })
    }
}

// an expr evaluated in place: constant parts folded to model values, dynamic
// parts left residual; containers stay structured so field checks can point
// at per-element ranges
#[derive(Debug, Clone)]
struct Folded {
    kind: FoldedKind,
    range: SrcRange,
}

#[derive(Debug, Clone)]
enum FoldedKind {
    // scalars, templates, funcs, refs, and residual operator exprs
    Value(Value),
    Array(Vec<Folded>),
    Object(Vec<FoldedField>),
}

#[derive(Debug, Clone)]
struct FoldedField {
    key: String,
    value: Folded,
    range: SrcRange,
}

// an array entry mid-fold; spreads survive only when their shape is unknown
enum FoldedElem {
    Item(Folded),
    Spread(Folded),
}

impl Folded {
    fn new(kind: FoldedKind, range: SrcRange) -> Self {
        Self { kind, range }
    }

    fn value(value: Value, range: SrcRange) -> Self {
        Self::new(FoldedKind::Value(value), range)
    }

    // drops ranges once checks are done, the model keeps only failure sites
    fn into_value(self) -> Value {
        match self.kind {
            FoldedKind::Value(value) => value,
            FoldedKind::Array(values) => Value::Array(Array {
                elem: values
                    .into_iter()
                    .map(|value| ArrayElem::Item(value.into_value()))
                    .collect(),
            }),
            FoldedKind::Object(fields) => Value::Object(Object {
                elem: fields
                    .into_iter()
                    .map(|field| ObjectField {
                        key: field.key,
                        value: field.value.into_value(),
                    })
                    .collect(),
            }),
        }
    }
}

// an evaluated var definition; constant values substitute at each reference,
// dynamic ones become scope bindings referenced by name
#[derive(Debug, Clone)]
struct VarDef {
    value: Folded,
    constant: bool,
    // the block whose instantiation evaluates the binding, none = root scope
    owner: Option<NodeId>,
}

// the block kinds a reference can address
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum BlockKind {
    Trace,
    Task,
    Llm,
    Tool,
    Function,
    Repeat,
    Choice,
    Maybe,
}

impl BlockKind {
    fn keyword(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Task => "task",
            Self::Llm => "llm",
            Self::Tool => "tool",
            Self::Function => "function",
            Self::Repeat => "repeat",
            Self::Choice => "choice",
            Self::Maybe => "maybe",
        }
    }

    fn has_fields(self) -> bool {
        matches!(self, Self::Trace | Self::Task | Self::Llm | Self::Tool | Self::Function)
    }
}

// one node of the symbol tree, appended as the walk enters its block
#[derive(Debug)]
struct SymNode {
    kind: BlockKind,
    name: Option<String>,
    children: Vec<NodeId>,
    // the fields the block sets, filled once its body models
    fields: Vec<Field>,
}

// a reference path segment as folded; the split into block steps, accessor,
// and drill-in selections needs the full symbol tree, so it waits for fixup
#[derive(Debug, Clone)]
enum Segment {
    Name { value: String, range: SrcRange },
    Index { value: Value, range: SrcRange },
    Slice {
        start: Option<Value>,
        end: Option<Value>,
        range: SrcRange,
    },
}

// where a block reference starts resolving
#[derive(Debug, Clone)]
enum Head {
    // a kind keyword, resolved by walking up the enclosing scopes
    Kind(BlockKind),
    // the innermost enclosing span or trace, resolved at fold time
    SelfBlock(NodeId),
    // the enclosing trace's own fields
    Trace,
}

// a value's landing place, the nodes of the static dependency graph
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum SlotId {
    Field {
        node: NodeId,
        field: Field,
        // metadata/metrics attribute at key precision when the value is an
        // object literal, so sibling-key references stay acyclic
        key: Option<String>,
    },
    Binding {
        owner: Option<NodeId>,
        name: String,
    },
}

// a reference recorded mid-walk, resolved against the symbol tree at fixup
#[derive(Debug, Clone)]
struct PendingRef {
    head: Head,
    segments: Vec<Segment>,
    // the sym stack at fold time, innermost last
    origin: Vec<NodeId>,
    // the slot whose expression contains the reference, for cycle edges
    slot: Option<SlotId>,
    range: SrcRange,
}

// one frame of name bindings: a block's vars and, while a for body evaluates,
// its loop bindings; frames stack innermost-last
#[derive(Default)]
struct Scope {
    // var.<name> definitions; none = invalid and already diagnosed
    vars: HashMap<String, Option<VarDef>>,
    // bare loop bindings, a separate namespace from vars
    loops: HashMap<String, Folded>,
}

// the outcome of one comprehension iteration
enum Iteration {
    Skipped,
    Invalid,
    Value(Folded),
    Entry { key: String, range: SrcRange, value: Folded },
}

pub(super) struct Modeler {
    ast: ast::Ast,
    scopes: Vec<Scope>,
    // every var name ever declared, for duplicate and visibility diagnostics
    declared: HashSet<String>,
    errors: Errors,
    // how many repeat blocks enclose the decl being lowered, gates repeat.index
    repeat_depth: usize,
    // the symbol tree of blocks, indexed by NodeId in walk order
    syms: Vec<SymNode>,
    // the chain of blocks enclosing the decl being lowered, innermost last
    sym_stack: Vec<NodeId>,
    // block references recorded mid-walk, indexed by RefId, resolved at fixup
    pending: Vec<PendingRef>,
    // the slot whose expression is folding, for dependency edges
    current_slot: Option<SlotId>,
    // static dependency edges for cycle detection: (from, to, reference site)
    edges: Vec<(SlotId, SlotId, SrcRange)>,
}

impl Modeler {
    fn new(ast: ast::Ast) -> Self {
        Self {
            ast,
            scopes: Vec::new(),
            declared: HashSet::new(),
            errors: Vec::new(),
            repeat_depth: 0,
            syms: Vec::new(),
            sym_stack: Vec::new(),
            pending: Vec::new(),
            current_slot: None,
            edges: Vec::new(),
        }
    }

    // appends a symbol node and enters it; the caller leaves via leave_sym
    fn enter_sym(&mut self, kind: BlockKind, name: Option<String>) -> NodeId {
        let node = NodeId(self.syms.len() as u32);
        self.syms.push(SymNode {
            kind,
            name,
            children: Vec::new(),
            fields: Vec::new(),
        });
        if let Some(&parent) = self.sym_stack.last() {
            self.syms[parent.0 as usize].children.push(node);
        }
        self.sym_stack.push(node);
        node
    }

    fn leave_sym(&mut self) {
        self.sym_stack.pop();
    }

    fn model(mut self) -> Result<Model, Errors> {
        let mut traces = Vec::new();

        // collect vars first so refs work no matter the decl order
        let (vars_blocks, rest) = split_vars(std::mem::take(&mut self.ast.decls));
        let mut scope = Scope::default();
        let mut bindings = Vec::new();
        for block in vars_blocks {
            self.collect_vars(block, &mut scope, &mut bindings);
        }
        self.scopes.push(scope);

        for decl in rest {
            match decl {
                ast::Decl::Attr(attr) => {
                    self.errors
                        .push(Error::new(ErrorKind::RootAttribute { keyword: attr.key }, attr.range));
                }
                ast::Decl::Block(block) => {
                    let Some(desc) = spec::SPEC.block(&block.kind) else {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownBlock {
                                keyword: block.kind,
                                parent: spec::Place::Root,
                            },
                            block.range,
                        ));
                        continue;
                    };

                    if desc.id == spec::ids::TRACE && desc.allows(spec::Place::Root) {
                        if let Some(trace) = self.model_trace(block, desc) {
                            traces.push(trace);
                        }
                    } else {
                        self.errors.push(Error::new(
                            ErrorKind::BlockNotAllowed {
                                block: desc.id,
                                parent: spec::Place::Root,
                            },
                            block.range,
                        ));
                    }
                }
            }
        }

        // fixup: every block name exists now, so pending references resolve;
        // this iterates collected records only, never the ast; extension
        // leaves dead intermediate records behind, so only references the
        // model actually stores resolve
        let live = live_refs(&self.pending, &traces, &bindings);
        let refs = self.resolve_pending(&live);
        self.detect_cycles();

        // per-iteration evaluation can repeat an identical diagnostic, keep the first
        let mut seen: Vec<Error> = Vec::new();
        self.errors.retain(|error| {
            if seen.contains(error) {
                false
            } else {
                seen.push(error.clone());
                true
            }
        });

        if traces.is_empty() && self.errors.is_empty() {
            self.errors.push(Error::new(
                ErrorKind::EmptyShape {
                    rule: spec::ids::NONEMPTY_SHAPE,
                },
                SrcRange::new(0, 0),
            ));
        }

        if self.errors.is_empty() {
            Ok(Model { traces, bindings, refs })
        } else {
            Err(self.errors)
        }
    }

    fn model_trace(&mut self, block: ast::Block, desc: &spec::BlockDesc) -> Option<Trace> {
        let ast::Block { name, decls, range, .. } = block;
        let name = self.model_name(name, range, desc);
        let node = self.enter_sym(BlockKind::Trace, name.clone());
        let (decls, bindings) = self.enter_scope(decls);
        let (fields, blocks) = self.model_body(decls, desc, range);
        self.record_fields(node, &fields);
        let children = blocks
            .into_iter()
            .filter_map(|block| self.model_child(block, desc.id))
            .collect();
        self.scopes.pop();
        self.leave_sym();

        name.map(|name| Trace {
            node,
            name,
            fields,
            bindings,
            children,
        })
    }

    // notes which fields a block sets so absent-field references diagnose
    fn record_fields(&mut self, node: NodeId, fields: &SpanFields) {
        let sym = &mut self.syms[node.0 as usize];
        let set = [
            (Field::Input, fields.input.is_some()),
            (Field::Output, fields.output.is_some()),
            (Field::Expected, fields.expected.is_some()),
            (Field::Error, fields.error.is_some()),
            (Field::Metadata, fields.metadata.is_some()),
            (Field::Metrics, fields.metrics.is_some()),
            (Field::Tags, !fields.tags.is_empty()),
        ];
        sym.fields.extend(set.into_iter().filter_map(|(field, set)| set.then_some(field)));
    }

    // collects a block's vars and pushes their scope frame; the caller pops it
    fn enter_scope(&mut self, decls: Vec<ast::Decl>) -> (Vec<ast::Decl>, Vec<Binding>) {
        let (vars_blocks, rest) = split_vars(decls);
        let mut scope = Scope::default();
        let mut bindings = Vec::new();
        for block in vars_blocks {
            self.collect_vars(block, &mut scope, &mut bindings);
        }
        self.scopes.push(scope);
        (rest, bindings)
    }

    fn collect_vars(&mut self, block: ast::Block, scope: &mut Scope, bindings: &mut Vec<Binding>) {
        let desc = spec::SPEC
            .block_by_id(spec::ids::VARS)
            .expect("the spec describes the vars block");
        let ast::Block { name, decls, range, .. } = block;
        self.model_name(name, range, desc);

        for decl in decls {
            match decl {
                ast::Decl::Block(inner) => {
                    let parent = spec::Place::Block { id: spec::ids::VARS };
                    let error = match spec::SPEC.block(&inner.kind) {
                        Some(desc) => ErrorKind::BlockNotAllowed { block: desc.id, parent },
                        None => ErrorKind::UnknownBlock {
                            keyword: inner.kind,
                            parent,
                        },
                    };
                    self.errors.push(Error::new(error, inner.range));
                }
                ast::Decl::Attr(attr) => {
                    if self.declared.contains(&attr.key) {
                        self.errors.push(Error::new(
                            ErrorKind::DuplicateVar {
                                rule: spec::ids::UNIQUE_VARS,
                                name: attr.key,
                            },
                            attr.range,
                        ));
                        continue;
                    }

                    // the value folds against the enclosing scopes only: the
                    // frame for the block being entered is not pushed yet, so
                    // same-scope references diagnose as unknown; the sym stack
                    // already holds the block, so block refs anchor inside it
                    let owner = self.sym_stack.last().copied();
                    let slot = SlotId::Binding {
                        owner,
                        name: attr.key.clone(),
                    };
                    let outer_slot = self.current_slot.replace(slot);
                    let def = self.fold_expr(attr.value).map(|value| VarDef {
                        constant: is_constant(&value),
                        value,
                        owner,
                    });
                    self.current_slot = outer_slot;

                    // a root var evaluates before any trace exists, so block
                    // references inside one have nothing to anchor to
                    if owner.is_none()
                        && let Some(def) = &def
                        && self.folded_reaches_block_ref(&def.value)
                    {
                        self.errors.push(Error::new(
                            ErrorKind::RootVarBlockRef {
                                rule: spec::ids::STATIC_STRUCTURE,
                                name: attr.key.clone(),
                            },
                            attr.range,
                        ));
                        continue;
                    }

                    // dynamic defs lower once here; references share the binding
                    if let Some(def) = &def
                        && !def.constant
                    {
                        bindings.push(Binding {
                            name: attr.key.clone(),
                            value: def.value.clone().into_value(),
                        });
                    }

                    self.declared.insert(attr.key.clone());
                    scope.vars.insert(attr.key, def);
                }
            }
        }
    }

    // the one pass over an expr: folds constant parts to model values as it
    // walks, checks types in place, and leaves dynamic parts residual
    fn fold_expr(&mut self, expr: ast::Expr) -> Option<Folded> {
        let ast::Expr { kind, range } = expr;
        match kind {
            ast::ExprKind::Str(value) => Some(Folded::value(Value::Str(value), range)),
            ast::ExprKind::Template(parts) => self.fold_template(parts, range),
            ast::ExprKind::Num(raw) => self.model_number(raw, false, range).map(|number| Folded::value(Value::Num(number), range)),
            ast::ExprKind::Bool(value) => Some(Folded::value(Value::Bool(value), range)),
            ast::ExprKind::Null => Some(Folded::value(Value::Null, range)),
            ast::ExprKind::Array(values) => self.fold_array(values, range),
            ast::ExprKind::Object(items) => self.fold_object(items, range),
            ast::ExprKind::Ref { path } => self.fold_ref(path, range),
            ast::ExprKind::Func { name, args } => {
                let mut folded = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.fold_expr(arg) {
                        Some(arg) => folded.push(arg),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                let func = self.model_func(name, folded, range)?;
                Some(Folded::value(Value::Func { func, range }, range))
            }
            ast::ExprKind::Unary { op, operand } => self.fold_unary(op, *operand, range),
            ast::ExprKind::Binary { op, lhs, rhs } => self.fold_binary(op, *lhs, *rhs, range),
            ast::ExprKind::Cond { cond, then, otherwise } => self.fold_cond(*cond, *then, *otherwise, range),
            ast::ExprKind::Index { target, index } => {
                // fold both sides first so each reports its own diagnostics
                let target = self.fold_expr(*target);
                let index = self.fold_expr(*index);
                let (target, index) = (target?, index?);
                self.fold_index(target, index, range)
            }
            ast::ExprKind::Slice { target, start, end } => self.fold_slice(*target, start, end, range),
            ast::ExprKind::For {
                bindings,
                collection,
                key,
                body,
                cond,
            } => self.fold_for(bindings, *collection, key, *body, cond, range),
            ast::ExprKind::Spread(_) => unreachable!("spreads only parse inside arrays and objects"),
        }
    }

    // resolves the ref namespaces: var.<name>, loop bindings, context refs,
    // and block references; loop bindings shadow every other head
    fn fold_ref(&mut self, path: Vec<String>, range: SrcRange) -> Option<Folded> {
        // use-site range wins so diags point at the ref, not the definition;
        // constant defs substitute, dynamic ones stay a reference to the binding
        if path.len() >= 2 && path[0] == "var" {
            let mut segments = path.into_iter().skip(1);
            let name = segments.next().expect("path has at least two segments");
            let def = self.lookup_var(name.clone(), range)?;
            let head = if def.constant {
                Folded::new(def.value.kind, range)
            } else {
                // a dynamic binding is a dataflow edge from the slot folding now
                if let Some(from) = self.current_slot.clone() {
                    let to = SlotId::Binding {
                        owner: def.owner,
                        name: name.clone(),
                    };
                    self.edges.push((from, to, range));
                }
                Folded::value(Value::VarRef(name), range)
            };
            return self.select_segments(head, segments, range);
        }

        // loop bindings shadow context namespaces, innermost frame wins
        if let Some(binding) = self.lookup_loop(&path[0]) {
            let mut segments = path.into_iter();
            segments.next();
            let head = Folded::new(binding.kind, range);
            return self.select_segments(head, segments, range);
        }

        // exact context paths resolve before block heads so trace.index stays
        // the generation counter while trace.output reads the enclosing trace
        if let Some(ctx_ref) = model_ctx_ref(&path, self.repeat_depth) {
            return Some(Folded::value(Value::CtxRef(ctx_ref), range));
        }

        // a repeat context path outside any repeat keeps its own diagnostic
        // instead of resolving as a block named `index` or `count`
        if let [first, second] = &path[..]
            && first == "repeat"
            && (second == "index" || second == "count")
        {
            self.errors.push(Error::new(
                ErrorKind::RepeatRefOutsideRepeat {
                    rule: spec::ids::REPEAT_REFS,
                    path: path.join("."),
                },
                range,
            ));
            return None;
        }

        if let Some(head) = self.block_head(&path[0], range) {
            let head = head?;
            let segments = path
                .into_iter()
                .skip(1)
                .map(|value| Segment::Name { value, range })
                .collect();
            return Some(self.begin_block_ref(head, segments, range));
        }

        self.require_ctx_ref(&path, range)
            .map(|ctx_ref| Folded::value(Value::CtxRef(ctx_ref), range))
    }

    // classifies a reference head: none = not a block head, some(none) = a
    // block head that failed to resolve and already diagnosed
    fn block_head(&mut self, head: &str, range: SrcRange) -> Option<Option<Head>> {
        let kind = match head {
            "task" => Head::Kind(BlockKind::Task),
            "llm" => Head::Kind(BlockKind::Llm),
            "tool" => Head::Kind(BlockKind::Tool),
            "function" => Head::Kind(BlockKind::Function),
            "repeat" => Head::Kind(BlockKind::Repeat),
            "choice" => Head::Kind(BlockKind::Choice),
            "maybe" => Head::Kind(BlockKind::Maybe),
            "trace" => Head::Trace,
            "self" => {
                // the innermost enclosing span or trace resolves right here
                let anchor = self
                    .sym_stack
                    .iter()
                    .rev()
                    .copied()
                    .find(|node| self.syms[node.0 as usize].kind.has_fields());
                return match anchor {
                    Some(node) => Some(Some(Head::SelfBlock(node))),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::SelfOutsideBlock {
                                rule: spec::ids::SELF_REFS,
                            },
                            range,
                        ));
                        Some(None)
                    }
                };
            }
            _ => return None,
        };
        Some(Some(kind))
    }

    // records a pending reference and hands back its opaque value
    fn begin_block_ref(&mut self, head: Head, segments: Vec<Segment>, range: SrcRange) -> Folded {
        let ref_id = RefId(self.pending.len() as u32);
        self.pending.push(PendingRef {
            head,
            segments,
            origin: self.sym_stack.clone(),
            slot: self.current_slot.clone(),
            range,
        });
        Folded::value(Value::BlockRef { ref_id, range }, range)
    }

    // extension never mutates the shared record: loop bindings clone their
    // folded values per use site, so two uses of one binding must not append
    // to the same pending reference
    fn extend_block_ref(&mut self, ref_id: RefId, segment: Segment, range: SrcRange) -> Folded {
        let mut pending = self.pending[ref_id.0 as usize].clone();
        pending.segments.push(segment);
        pending.range = range;
        let extended = RefId(self.pending.len() as u32);
        self.pending.push(pending);
        Folded::value(
            Value::BlockRef {
                ref_id: extended,
                range,
            },
            range,
        )
    }

    // selects the trailing segments of a reference path as string indexes
    fn select_segments(
        &mut self,
        head: Folded,
        segments: impl Iterator<Item = String>,
        range: SrcRange,
    ) -> Option<Folded> {
        let mut selected = head;
        for segment in segments {
            let index = Folded::value(Value::Str(segment), range);
            selected = self.fold_index(selected, index, range)?;
        }
        Some(selected)
    }

    // interpolation holes fold like any expression: constants splice as
    // literals, templates splice their parts, dynamic values stay parts when
    // their type is a known scalar, and reference-fed values settle
    // scalar-ness at generation
    fn fold_template(&mut self, parts: Vec<ast::TemplatePart>, range: SrcRange) -> Option<Folded> {
        let mut modeled: Vec<Part> = Vec::with_capacity(parts.len());
        let mut valid = true;

        for part in parts {
            match part {
                ast::TemplatePart::Lit(value) => modeled.push(Part::Lit(value)),
                ast::TemplatePart::Ref { expr, range } => {
                    let Some(folded) = self.fold_expr(expr) else {
                        valid = false;
                        continue;
                    };
                    match folded.kind {
                        FoldedKind::Value(Value::Str(text)) => modeled.push(Part::Lit(text)),
                        FoldedKind::Value(Value::Num(number)) => modeled.push(Part::Lit(num_text(number))),
                        FoldedKind::Value(Value::Bool(value)) => {
                            modeled.push(Part::Lit(if value { "true" } else { "false" }.to_owned()));
                        }
                        // a template value splices its parts inline
                        FoldedKind::Value(Value::Template(template)) => modeled.extend(template.parts),
                        FoldedKind::Value(Value::CtxRef(ctx_ref)) => modeled.push(Part::Ref(ctx_ref)),
                        // dynamic defs resolve per scope instantiation, scalar by type
                        FoldedKind::Value(Value::VarRef(name)) => {
                            let var_type = self.value_type(&Value::VarRef(name.clone()));
                            match var_type {
                                Some(StaticType::String | StaticType::Number | StaticType::Boolean) => {
                                    modeled.push(Part::VarRef(name));
                                }
                                _ => {
                                    self.errors.push(Error::new(
                                        ErrorKind::NonScalarInterpolation {
                                            rule: spec::ids::SCALAR_INTERPOLATION,
                                            found: self.found_type(&Folded::value(Value::VarRef(name), range)),
                                        },
                                        range,
                                    ));
                                    valid = false;
                                }
                            }
                        }
                        FoldedKind::Value(value) => {
                            let folded = Folded::value(value, range);
                            let scalar = matches!(
                                self.static_type(&folded),
                                Some(StaticType::String | StaticType::Number | StaticType::Boolean)
                            );
                            if scalar || self.defers_to_generation(&folded) {
                                modeled.push(Part::Dynamic(folded.into_value()));
                            } else {
                                self.errors.push(Error::new(
                                    ErrorKind::NonScalarInterpolation {
                                        rule: spec::ids::SCALAR_INTERPOLATION,
                                        found: self.found_type(&folded),
                                    },
                                    range,
                                ));
                                valid = false;
                            }
                        }
                        kind @ (FoldedKind::Array(_) | FoldedKind::Object(_)) => {
                            self.errors.push(Error::new(
                                ErrorKind::NonScalarInterpolation {
                                    rule: spec::ids::SCALAR_INTERPOLATION,
                                    found: self.found_type(&Folded::new(kind, range)),
                                },
                                range,
                            ));
                            valid = false;
                        }
                    }
                }
            }
        }

        if !valid {
            return None;
        }

        // an all-literal template folds to a plain string so downstream only
        // sees templates that actually resolve
        if modeled.iter().all(|part| matches!(part, Part::Lit(_))) {
            let joined = modeled
                .into_iter()
                .map(|part| match part {
                    Part::Lit(value) => value,
                    Part::Ref(_) | Part::VarRef(_) | Part::Dynamic(_) => unreachable!("all parts are literal"),
                })
                .collect();
            return Some(Folded::value(Value::Str(joined), range));
        }

        Some(Folded::value(Value::Template(Template { parts: modeled }), range))
    }

    fn fold_array(&mut self, values: Vec<ast::Expr>, range: SrcRange) -> Option<Folded> {
        let mut folded = Vec::with_capacity(values.len());
        let mut valid = true;
        for value in values {
            match value.kind {
                // constant-shape spreads splice in place, reference-fed ones
                // stay residual and splice at generation
                ast::ExprKind::Spread(operand) => match self.fold_array_spread(*operand) {
                    Some(elems) => folded.extend(elems),
                    None => valid = false,
                },
                _ => match self.fold_expr(value) {
                    Some(value) => folded.push(FoldedElem::Item(value)),
                    None => valid = false,
                },
            }
        }
        if !valid {
            return None;
        }

        // an array with a residual spread has no static positions, so it
        // lowers to a value instead of a folded container
        if folded.iter().any(|elem| matches!(elem, FoldedElem::Spread(_))) {
            let elem = folded
                .into_iter()
                .map(|elem| match elem {
                    FoldedElem::Item(value) => ArrayElem::Item(value.into_value()),
                    FoldedElem::Spread(value) => ArrayElem::Spread(value.into_value()),
                })
                .collect();
            return Some(Folded::value(Value::Array(Array { elem }), range));
        }

        let items = folded
            .into_iter()
            .map(|elem| match elem {
                FoldedElem::Item(value) => value,
                FoldedElem::Spread(_) => unreachable!("residual spreads returned above"),
            })
            .collect();
        Some(Folded::new(FoldedKind::Array(items), range))
    }

    fn fold_unary(&mut self, op: ast::UnaryOp, operand: ast::Expr, range: SrcRange) -> Option<Folded> {
        // a minus folds into an adjacent number literal so i64::MIN parses
        if op == ast::UnaryOp::Neg
            && let ast::ExprKind::Num(raw) = operand.kind
        {
            let number = self.model_number(raw, true, range)?;
            return Some(Folded::value(Value::Num(number), range));
        }

        let operand = self.fold_expr(operand)?;

        let required = match op {
            ast::UnaryOp::Neg => StaticType::Number,
            ast::UnaryOp::Not => StaticType::Boolean,
        };
        if !self.check_operand(&operand, required, op.to_string()) {
            return None;
        }

        match operand.kind {
            FoldedKind::Value(Value::Bool(value)) if op == ast::UnaryOp::Not => {
                Some(Folded::value(Value::Bool(!value), range))
            }
            // negating a constant computes now, overflow can only hit i64::MIN
            FoldedKind::Value(Value::Num(number)) => {
                let negated = match number {
                    Number::Int(value) => value.checked_neg().map(Number::Int),
                    Number::Float(value) => Some(Number::Float(-value)),
                };
                match negated {
                    Some(number) => Some(Folded::value(Value::Num(number), range)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonFiniteResult {
                                rule: spec::ids::FINITE_NUMBERS,
                            },
                            range,
                        ));
                        None
                    }
                }
            }
            kind => Some(Folded::value(
                Value::Unary {
                    op: model_unary_op(op),
                    operand: Box::new(Folded::new(kind, operand.range).into_value()),
                    range,
                },
                range,
            )),
        }
    }

    fn fold_binary(&mut self, op: ast::BinOp, lhs: ast::Expr, rhs: ast::Expr, range: SrcRange) -> Option<Folded> {
        // fold both sides first so each reports its own diagnostics
        let lhs = self.fold_expr(lhs);
        let rhs = self.fold_expr(rhs);
        let (lhs, rhs) = (lhs?, rhs?);

        let valid = match op_class(op) {
            OpClass::Arith | OpClass::Cmp => {
                let left = self.check_operand(&lhs, StaticType::Number, op.to_string());
                self.check_operand(&rhs, StaticType::Number, op.to_string()) && left
            }
            OpClass::Logic => {
                let left = self.check_operand(&lhs, StaticType::Boolean, op.to_string());
                self.check_operand(&rhs, StaticType::Boolean, op.to_string()) && left
            }
            OpClass::Eq => self.check_equality_operands(&lhs, &rhs, op),
        };
        if !valid {
            return None;
        }

        // a constant zero divisor is always an error, even with a dynamic lhs
        if matches!(op, ast::BinOp::Div | ast::BinOp::Rem)
            && let FoldedKind::Value(Value::Num(number)) = &rhs.kind
            && float_bound(number.clone()) == 0.0
        {
            self.errors.push(Error::new(
                ErrorKind::DivisionByZero {
                    rule: spec::ids::NONZERO_DIVISORS,
                    op: op.to_string(),
                },
                rhs.range,
            ));
            return None;
        }

        if is_constant(&lhs) && is_constant(&rhs) {
            return self.eval_binary(op, const_scalar(&lhs), const_scalar(&rhs), range);
        }

        Some(Folded::value(
            Value::Binary {
                op: model_bin_op(op),
                lhs: Box::new(lhs.into_value()),
                rhs: Box::new(rhs.into_value()),
                range,
            },
            range,
        ))
    }

    fn fold_cond(&mut self, cond: ast::Expr, then: ast::Expr, otherwise: ast::Expr, range: SrcRange) -> Option<Folded> {
        let cond = self.fold_expr(cond);
        let then = self.fold_expr(then);
        let otherwise = self.fold_expr(otherwise);
        let (cond, then, otherwise) = (cond?, then?, otherwise?);

        if self.static_type(&cond) != Some(StaticType::Boolean) && !self.defers_to_generation(&cond) {
            self.errors.push(Error::new(
                ErrorKind::NonBooleanCondition {
                    rule: spec::ids::BOOLEAN_CONDITIONS,
                    found: self.found_type(&cond),
                },
                cond.range,
            ));
            return None;
        }

        // a constant condition picks its branch during validation
        if let FoldedKind::Value(Value::Bool(value)) = cond.kind {
            let taken = if value { then } else { otherwise };
            return Some(Folded::new(taken.kind, range));
        }

        Some(Folded::value(
            Value::Cond {
                cond: Box::new(cond.into_value()),
                then: Box::new(then.into_value()),
                otherwise: Box::new(otherwise.into_value()),
            },
            range,
        ))
    }

    fn fold_index(&mut self, target: Folded, index: Folded, range: SrcRange) -> Option<Folded> {
        // an index on a block reference extends its path; where the block path
        // ends and json drill-in begins is decided at fixup, so no checks here
        if let FoldedKind::Value(Value::BlockRef { ref_id, .. }) = target.kind {
            let segment = match index.kind {
                FoldedKind::Value(Value::Str(value)) => Segment::Name {
                    value,
                    range: index.range,
                },
                kind => Segment::Index {
                    value: Folded::new(kind, index.range).into_value(),
                    range: index.range,
                },
            };
            return Some(self.extend_block_ref(ref_id, segment, range));
        }

        let expected = match self.static_type(&target) {
            Some(StaticType::Array) => StaticType::Number,
            Some(StaticType::Object) => StaticType::String,
            // a target fed by a block reference is only shaped at generation
            None if self.folded_reaches_block_ref(&target) => {
                return Some(Folded::value(
                    Value::Index {
                        target: Box::new(target.into_value()),
                        index: Box::new(index.into_value()),
                        range,
                    },
                    range,
                ));
            }
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonIndexableTarget {
                        rule: spec::ids::INDEXABLE_TARGETS,
                        found: self.found_type(&target),
                    },
                    target.range,
                ));
                return None;
            }
        };

        if self.static_type(&index) != Some(expected) && !self.defers_to_generation(&index) {
            self.errors.push(Error::new(
                ErrorKind::IndexTypeMismatch {
                    rule: spec::ids::INDEXABLE_TARGETS,
                    expected: type_name(expected),
                    found: self.found_type(&index),
                },
                index.range,
            ));
            return None;
        }

        // a constant index into a container literal selects its element during
        // validation, the element itself may still be dynamic
        match target.kind {
            FoldedKind::Array(values) if is_constant(&index) => {
                let Const::Num(number) = const_scalar(&index) else {
                    unreachable!("index was type checked as a number");
                };
                let Number::Int(position) = number else {
                    self.errors.push(Error::new(
                        ErrorKind::NonIntegerIndex {
                            rule: spec::ids::INDEXABLE_TARGETS,
                        },
                        index.range,
                    ));
                    return None;
                };

                match usize::try_from(position).ok().filter(|&position| position < values.len()) {
                    Some(position) => {
                        let selected = values.into_iter().nth(position).expect("position is in bounds");
                        Some(Folded::new(selected.kind, range))
                    }
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::IndexOutOfBounds {
                                rule: spec::ids::INDEX_BOUNDS,
                                index: position,
                                len: values.len(),
                            },
                            index.range,
                        ));
                        None
                    }
                }
            }
            FoldedKind::Object(fields) if is_constant(&index) => {
                let Const::Str(key) = const_scalar(&index) else {
                    unreachable!("index was type checked as a string");
                };

                let selected = fields.into_iter().find_map(|field| (field.key == key).then_some(field.value));
                match selected {
                    Some(value) => Some(Folded::new(value.kind, range)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownObjectKey {
                                rule: spec::ids::INDEX_BOUNDS,
                                key,
                            },
                            index.range,
                        ));
                        None
                    }
                }
            }
            kind => Some(Folded::value(
                Value::Index {
                    target: Box::new(Folded::new(kind, target.range).into_value()),
                    index: Box::new(index.into_value()),
                    range,
                },
                range,
            )),
        }
    }

    fn fold_array_spread(&mut self, operand: ast::Expr) -> Option<Vec<FoldedElem>> {
        let operand = self.fold_expr(operand)?;

        // a dynamic var splices as accessors into the one evaluated binding
        if let FoldedKind::Value(Value::VarRef(name)) = &operand.kind {
            let ranges = match self.var_value(name).map(|value| &value.kind) {
                Some(FoldedKind::Array(values)) => Some(values.iter().map(|value| value.range).collect::<Vec<_>>()),
                _ => None,
            };
            if let Some(ranges) = ranges {
                return Some(
                    ranges
                        .into_iter()
                        .enumerate()
                        .map(|(index, range)| {
                            FoldedElem::Item(Folded::value(
                                Value::Index {
                                    target: Box::new(Value::VarRef(name.clone())),
                                    index: Box::new(Value::Num(Number::Int(index as i64))),
                                    range,
                                },
                                range,
                            ))
                        })
                        .collect(),
                );
            }
        }

        // arrays whose shape only exists at generation splice there
        let dynamic = self.static_type(&operand) == Some(StaticType::Array) && !matches!(operand.kind, FoldedKind::Array(_));
        if dynamic || self.defers_to_generation(&operand) {
            return Some(vec![FoldedElem::Spread(operand)]);
        }

        match operand.kind {
            FoldedKind::Array(values) => Some(values.into_iter().map(FoldedElem::Item).collect()),
            kind => {
                let operand = Folded::new(kind, operand.range);
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "array",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                None
            }
        }
    }

    // later entries win over keys a spread introduced, two explicit keys collide
    fn fold_object(&mut self, items: Vec<ast::ObjectItem>, range: SrcRange) -> Option<Folded> {
        let mut merged: Vec<(FoldedField, bool)> = Vec::new();
        let mut valid = true;

        for item in items {
            match item {
                ast::ObjectItem::Attr(attr) => match self.fold_expr(attr.value) {
                    Some(value) => {
                        let field = FoldedField {
                            key: attr.key,
                            value,
                            range: attr.range,
                        };
                        match merged.iter_mut().find(|(existing, _)| existing.key == field.key) {
                            Some((_, true)) => {
                                self.errors.push(Error::new(
                                    ErrorKind::DuplicateObjectKey {
                                        rule: spec::ids::UNIQUE_OBJECT_KEYS,
                                        key: field.key,
                                    },
                                    field.range,
                                ));
                                valid = false;
                            }
                            Some(slot) => *slot = (field, true),
                            None => merged.push((field, true)),
                        }
                    }
                    None => valid = false,
                },
                ast::ObjectItem::Spread(operand) => match self.fold_object_spread(operand) {
                    Some(fields) => {
                        for field in fields {
                            match merged.iter_mut().find(|(existing, _)| existing.key == field.key) {
                                Some(slot) => *slot = (field, false),
                                None => merged.push((field, false)),
                            }
                        }
                    }
                    None => valid = false,
                },
            }
        }

        if !valid {
            return None;
        }

        let fields = merged.into_iter().map(|(field, _)| field).collect();
        Some(Folded::new(FoldedKind::Object(fields), range))
    }

    fn fold_object_spread(&mut self, operand: ast::Expr) -> Option<Vec<FoldedField>> {
        let operand = self.fold_expr(operand)?;

        // a dynamic var splices as accessors into the one evaluated binding
        if let FoldedKind::Value(Value::VarRef(name)) = &operand.kind {
            let keys = match self.var_value(name).map(|value| &value.kind) {
                Some(FoldedKind::Object(fields)) => Some(
                    fields
                        .iter()
                        .map(|field| (field.key.clone(), field.range))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            };
            let Some(keys) = keys else {
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "object",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                return None;
            };
            return Some(
                keys.into_iter()
                    .map(|(key, range)| FoldedField {
                        value: Folded::value(
                            Value::Index {
                                target: Box::new(Value::VarRef(name.clone())),
                                index: Box::new(Value::Str(key.clone())),
                                range,
                            },
                            range,
                        ),
                        key,
                        range,
                    })
                    .collect(),
            );
        }

        match operand.kind {
            FoldedKind::Object(fields) => Some(fields),
            kind => {
                let operand = Folded::new(kind, operand.range);
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "object",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                None
            }
        }
    }

    fn fold_slice(
        &mut self,
        target: ast::Expr,
        start: Option<Box<ast::Expr>>,
        end: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<Folded> {
        // fold every part first so each reports its own diagnostics
        let target = self.fold_expr(target);
        let start = start.map(|bound| self.fold_expr(*bound));
        let end = end.map(|bound| self.fold_expr(*bound));
        let target = target?;
        let start = match start {
            Some(bound) => Some(bound?),
            None => None,
        };
        let end = match end {
            Some(bound) => Some(bound?),
            None => None,
        };

        // a slice on a block reference extends its path like an index does
        if let FoldedKind::Value(Value::BlockRef { ref_id, .. }) = target.kind {
            let mut valid = true;
            for bound in [&start, &end].into_iter().flatten() {
                if self.static_type(bound) != Some(StaticType::Number) && !self.defers_to_generation(bound) {
                    self.errors.push(Error::new(
                        ErrorKind::SliceTypeMismatch {
                            rule: spec::ids::SLICE_BOUNDS,
                            found: self.found_type(bound),
                        },
                        bound.range,
                    ));
                    valid = false;
                }
            }
            if !valid {
                return None;
            }
            let segment = Segment::Slice {
                start: start.map(Folded::into_value),
                end: end.map(Folded::into_value),
                range,
            };
            return Some(self.extend_block_ref(ref_id, segment, range));
        }

        let sliceable = self.static_type(&target) == Some(StaticType::Array)
            // a target fed by a block reference is only shaped at generation
            || (self.static_type(&target).is_none() && self.folded_reaches_block_ref(&target));
        if !sliceable {
            self.errors.push(Error::new(
                ErrorKind::NonSliceableTarget {
                    rule: spec::ids::SLICEABLE_TARGETS,
                    found: self.found_type(&target),
                },
                target.range,
            ));
            return None;
        }

        let mut valid = true;
        for bound in [&start, &end].into_iter().flatten() {
            if self.static_type(bound) != Some(StaticType::Number) && !self.defers_to_generation(bound) {
                self.errors.push(Error::new(
                    ErrorKind::SliceTypeMismatch {
                        rule: spec::ids::SLICE_BOUNDS,
                        found: self.found_type(bound),
                    },
                    bound.range,
                ));
                valid = false;
            }
        }
        if !valid {
            return None;
        }

        // constant bounds must check out even when the target is dynamic
        let start_const = match &start {
            Some(bound) => self.check_slice_bound(bound),
            None => Some(Some(0)),
        };
        let end_const = match &end {
            Some(bound) => self.check_slice_bound(bound),
            None => Some(None),
        };
        let (start_const, end_const) = match (start_const, end_const) {
            (Some(start), Some(end)) => (start, end),
            _ => return None,
        };

        // a constant slice of a literal selects its elements during validation,
        // the elements themselves may still be dynamic
        let known = start_const.is_some() && (end_const.is_some() || end.is_none());
        if known && matches!(target.kind, FoldedKind::Array(_)) {
            let FoldedKind::Array(values) = target.kind else {
                unreachable!("the target was just matched as an array literal");
            };
            let len = values.len();
            let start = start_const.expect("a known slice has a constant start").min(len);
            let end = end_const.unwrap_or(len).min(len);
            let selected = if start >= end {
                Vec::new()
            } else {
                values.into_iter().skip(start).take(end - start).collect()
            };
            return Some(Folded::new(FoldedKind::Array(selected), range));
        }

        Some(Folded::value(
            Value::Slice {
                target: Box::new(target.into_value()),
                start: start.map(|bound| Box::new(bound.into_value())),
                end: end.map(|bound| Box::new(bound.into_value())),
                range,
            },
            range,
        ))
    }

    // some(none) = dynamic, checked during generation, none = diagnostic pushed
    fn check_slice_bound(&mut self, bound: &Folded) -> Option<Option<usize>> {
        if !is_constant(bound) {
            return Some(None);
        }

        let Const::Num(number) = const_scalar(bound) else {
            unreachable!("bound was type checked as a number");
        };
        let Number::Int(value) = number else {
            self.errors.push(Error::new(
                ErrorKind::NonIntegerSliceBound {
                    rule: spec::ids::SLICE_BOUNDS,
                },
                bound.range,
            ));
            return None;
        };

        match usize::try_from(value) {
            Ok(value) => Some(Some(value)),
            Err(_) => {
                self.errors.push(Error::new(
                    ErrorKind::NegativeSliceBound {
                        rule: spec::ids::SLICE_BOUNDS,
                    },
                    bound.range,
                ));
                None
            }
        }
    }

    // unrolls comprehensions during validation, bindings live in a loop scope frame
    fn fold_for(
        &mut self,
        bindings: Vec<String>,
        collection: ast::Expr,
        key: Option<Box<ast::Expr>>,
        body: ast::Expr,
        cond: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<Folded> {
        let collection = self.fold_expr(collection)?;
        let iterations = self.for_iterations(&bindings, collection)?;

        let object = key.is_some();
        let mut values = Vec::new();
        let mut fields: Vec<FoldedField> = Vec::new();
        let mut valid = true;

        for loops in iterations {
            self.scopes.push(Scope {
                loops,
                ..Scope::default()
            });
            let outcome = self.fold_iteration(&key, &body, &cond);
            self.scopes.pop();

            match outcome {
                Iteration::Skipped => {}
                Iteration::Invalid => valid = false,
                Iteration::Value(value) => values.push(value),
                Iteration::Entry { key, range, value } => {
                    // duplicate keys diagnose like literal object entries
                    if fields.iter().any(|field| field.key == key) {
                        self.errors.push(Error::new(
                            ErrorKind::DuplicateObjectKey {
                                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                                key,
                            },
                            range,
                        ));
                        valid = false;
                    } else {
                        fields.push(FoldedField { key, value, range });
                    }
                }
            }
        }

        if !valid {
            return None;
        }

        let kind = if object {
            FoldedKind::Object(fields)
        } else {
            FoldedKind::Array(values)
        };
        Some(Folded::new(kind, range))
    }

    // per-iteration loop bindings; a dynamic var iterates by its definition's
    // shape, bindings access the one evaluated value by position
    fn for_iterations(&mut self, bindings: &[String], collection: Folded) -> Option<Vec<HashMap<String, Folded>>> {
        match collection.kind {
            FoldedKind::Array(values) => Some(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let mut map = HashMap::new();
                        if bindings.len() == 2 {
                            let index = Folded::value(Value::Num(Number::Int(index as i64)), value.range);
                            map.insert(bindings[0].clone(), index);
                            map.insert(bindings[1].clone(), value);
                        } else {
                            map.insert(bindings[0].clone(), value);
                        }
                        map
                    })
                    .collect(),
            ),
            FoldedKind::Object(fields) => Some(
                fields
                    .into_iter()
                    .map(|field| {
                        let mut map = HashMap::new();
                        let key = Folded::value(Value::Str(field.key), field.range);
                        map.insert(bindings[0].clone(), key);
                        if bindings.len() == 2 {
                            map.insert(bindings[1].clone(), field.value);
                        }
                        map
                    })
                    .collect(),
            ),
            FoldedKind::Value(Value::VarRef(name)) => {
                enum VarShape {
                    Array(Vec<SrcRange>),
                    Object(Vec<(String, SrcRange)>),
                }

                let shape = match self.var_value(&name).map(|value| &value.kind) {
                    Some(FoldedKind::Array(values)) => {
                        Some(VarShape::Array(values.iter().map(|value| value.range).collect()))
                    }
                    Some(FoldedKind::Object(fields)) => Some(VarShape::Object(
                        fields.iter().map(|field| (field.key.clone(), field.range)).collect(),
                    )),
                    _ => None,
                };
                let Some(shape) = shape else {
                    let collection = Folded::value(Value::VarRef(name), collection.range);
                    self.errors.push(Error::new(
                        ErrorKind::ForCollectionMismatch {
                            rule: spec::ids::FOR_COLLECTIONS,
                            found: ExprType::of(&collection),
                        },
                        collection.range,
                    ));
                    return None;
                };

                let accessor = |index: Value, range| {
                    Folded::value(
                        Value::Index {
                            target: Box::new(Value::VarRef(name.clone())),
                            index: Box::new(index),
                            range,
                        },
                        range,
                    )
                };
                match shape {
                    VarShape::Array(ranges) => Some(
                        ranges
                            .into_iter()
                            .enumerate()
                            .map(|(index, range)| {
                                let mut map = HashMap::new();
                                let element = accessor(Value::Num(Number::Int(index as i64)), range);
                                if bindings.len() == 2 {
                                    let index = Folded::value(Value::Num(Number::Int(index as i64)), range);
                                    map.insert(bindings[0].clone(), index);
                                    map.insert(bindings[1].clone(), element);
                                } else {
                                    map.insert(bindings[0].clone(), element);
                                }
                                map
                            })
                            .collect(),
                    ),
                    VarShape::Object(keys) => Some(
                        keys.into_iter()
                            .map(|(key, range)| {
                                let mut map = HashMap::new();
                                map.insert(bindings[0].clone(), Folded::value(Value::Str(key.clone()), range));
                                if bindings.len() == 2 {
                                    map.insert(bindings[1].clone(), accessor(Value::Str(key), range));
                                }
                                map
                            })
                            .collect(),
                    ),
                }
            }
            // a comprehension unrolls during validation, so its shape can
            // never come from a generated field
            kind @ FoldedKind::Value(Value::BlockRef { .. }) => {
                let collection = Folded::new(kind, collection.range);
                self.errors.push(Error::new(
                    ErrorKind::ForBlockRefCollection {
                        rule: spec::ids::STATIC_STRUCTURE,
                    },
                    collection.range,
                ));
                None
            }
            kind => {
                let collection = Folded::new(kind, collection.range);
                self.errors.push(Error::new(
                    ErrorKind::ForCollectionMismatch {
                        rule: spec::ids::FOR_COLLECTIONS,
                        found: ExprType::of(&collection),
                    },
                    collection.range,
                ));
                None
            }
        }
    }

    // filter first so skipped elements never fold their bodies
    fn fold_iteration(
        &mut self,
        key: &Option<Box<ast::Expr>>,
        body: &ast::Expr,
        cond: &Option<Box<ast::Expr>>,
    ) -> Iteration {
        if let Some(cond) = cond {
            let Some(cond) = self.fold_expr((**cond).clone()) else {
                return Iteration::Invalid;
            };
            match cond.kind {
                FoldedKind::Value(Value::Bool(true)) => {}
                FoldedKind::Value(Value::Bool(false)) => return Iteration::Skipped,
                _ => {
                    self.errors.push(Error::new(
                        ErrorKind::NonConstantForFilter {
                            rule: spec::ids::STATIC_FOR,
                            found: ExprType::of(&cond),
                        },
                        cond.range,
                    ));
                    return Iteration::Invalid;
                }
            }
        }

        let Some(value) = self.fold_expr(body.clone()) else {
            return Iteration::Invalid;
        };

        match key {
            Some(key) => {
                let Some(key) = self.fold_expr((**key).clone()) else {
                    return Iteration::Invalid;
                };
                match const_string(&key) {
                    Some(text) => Iteration::Entry {
                        key: text,
                        range: key.range,
                        value,
                    },
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonConstantForKey {
                                rule: spec::ids::STATIC_FOR,
                                found: ExprType::of(&key),
                            },
                            key.range,
                        ));
                        Iteration::Invalid
                    }
                }
            }
            None => Iteration::Value(value),
        }
    }

    fn check_operand(&mut self, operand: &Folded, required: StaticType, op: String) -> bool {
        if self.static_type(operand) == Some(required) || self.defers_to_generation(operand) {
            return true;
        }

        self.errors.push(Error::new(
            ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op,
                expected: type_name(required),
                found: self.found_type(operand),
            },
            operand.range,
        ));
        false
    }

    // a value fed by a block reference has no static type by design; its type
    // checks move to generation instead of failing validation
    fn defers_to_generation(&self, folded: &Folded) -> bool {
        self.static_type(folded).is_none() && self.folded_reaches_block_ref(folded)
    }

    fn folded_reaches_block_ref(&self, folded: &Folded) -> bool {
        match &folded.kind {
            FoldedKind::Value(value) => self.value_reaches_block_ref(value),
            FoldedKind::Array(values) => values.iter().any(|value| self.folded_reaches_block_ref(value)),
            FoldedKind::Object(fields) => fields.iter().any(|field| self.folded_reaches_block_ref(&field.value)),
        }
    }

    fn value_reaches_block_ref(&self, value: &Value) -> bool {
        let any = |values: &[Value]| values.iter().any(|value| self.value_reaches_block_ref(value));
        match value {
            Value::BlockRef { .. } => true,
            Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null | Value::CtxRef(_) => false,
            Value::Template(template) => template.parts.iter().any(|part| match part {
                Part::Dynamic(value) => self.value_reaches_block_ref(value),
                Part::Lit(_) | Part::Ref(_) | Part::VarRef(_) => false,
            }),
            Value::Array(array) => array.elem.iter().any(|elem| match elem {
                ArrayElem::Item(value) | ArrayElem::Spread(value) => self.value_reaches_block_ref(value),
            }),
            Value::Object(object) => object.elem.iter().any(|field| self.value_reaches_block_ref(&field.value)),
            // a binding reaches through its definition
            Value::VarRef(name) => self.var_value(name).is_some_and(|def| self.folded_reaches_block_ref(def)),
            Value::Unary { operand, .. } => self.value_reaches_block_ref(operand),
            Value::Binary { lhs, rhs, .. } => self.value_reaches_block_ref(lhs) || self.value_reaches_block_ref(rhs),
            Value::Cond { cond, then, otherwise } => {
                self.value_reaches_block_ref(cond)
                    || self.value_reaches_block_ref(then)
                    || self.value_reaches_block_ref(otherwise)
            }
            Value::Index { target, index, .. } => {
                self.value_reaches_block_ref(target) || self.value_reaches_block_ref(index)
            }
            Value::Slice { target, start, end, .. } => {
                self.value_reaches_block_ref(target)
                    || start.as_deref().is_some_and(|bound| self.value_reaches_block_ref(bound))
                    || end.as_deref().is_some_and(|bound| self.value_reaches_block_ref(bound))
            }
            Value::Func { func, .. } => match func {
                Func::Choice(options) | Func::Min(options) | Func::Max(options) => any(options),
                Func::Weighted(options) => options.iter().any(|option| self.value_reaches_block_ref(&option.value)),
                Func::Range(_)
                | Func::Normal { .. }
                | Func::Lognormal { .. }
                | Func::Exponential { .. }
                | Func::Pareto { .. }
                | Func::Beta { .. }
                | Func::Poisson { .. }
                | Func::Chance { .. }
                | Func::Uuid
                | Func::Hex { .. }
                | Func::Alphanum { .. } => false,
                Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => {
                    self.value_reaches_block_ref(text)
                }
                Func::Replace { text, from, to } => {
                    self.value_reaches_block_ref(text)
                        || self.value_reaches_block_ref(from)
                        || self.value_reaches_block_ref(to)
                }
                Func::Split { text, separator } => {
                    self.value_reaches_block_ref(text) || self.value_reaches_block_ref(separator)
                }
                Func::Join { array, separator } => {
                    self.value_reaches_block_ref(array) || self.value_reaches_block_ref(separator)
                }
                Func::Contains { target, needle } => {
                    self.value_reaches_block_ref(target) || self.value_reaches_block_ref(needle)
                }
                Func::StartsWith { text, prefix } => {
                    self.value_reaches_block_ref(text) || self.value_reaches_block_ref(prefix)
                }
                Func::EndsWith { text, suffix } => {
                    self.value_reaches_block_ref(text) || self.value_reaches_block_ref(suffix)
                }
                Func::Len { target } => self.value_reaches_block_ref(target),
                Func::Tokens { value } => self.value_reaches_block_ref(value),
                Func::Format { args, .. } => any(args),
                Func::Clamp { value, min, max } => {
                    self.value_reaches_block_ref(value)
                        || self.value_reaches_block_ref(min)
                        || self.value_reaches_block_ref(max)
                }
                Func::Round { value } | Func::Floor { value } | Func::Ceil { value } | Func::Abs { value } => {
                    self.value_reaches_block_ref(value)
                }
            },
        }
    }

    fn check_equality_operands(&mut self, lhs: &Folded, rhs: &Folded, op: ast::BinOp) -> bool {
        const SCALARS: &str = "string, number, or boolean";
        fn scalar(found: Option<StaticType>) -> bool {
            matches!(found, Some(StaticType::String | StaticType::Number | StaticType::Boolean))
        }

        let lhs_type = self.static_type(lhs);
        let rhs_type = self.static_type(rhs);
        let mut valid = true;
        // reference-fed sides settle their types at generation
        let deferred = self.defers_to_generation(lhs) || self.defers_to_generation(rhs);

        for (side, side_type) in [(lhs, lhs_type), (rhs, rhs_type)] {
            // a null literal compares against a possibly-absent reference
            let null_probe = side_type == Some(StaticType::Null) && deferred;
            if !scalar(side_type) && !null_probe && !self.defers_to_generation(side) {
                self.errors.push(Error::new(
                    ErrorKind::OperandTypeMismatch {
                        rule: spec::ids::OPERAND_TYPES,
                        op: op.to_string(),
                        expected: SCALARS,
                        found: self.found_type(side),
                    },
                    side.range,
                ));
                valid = false;
            }
        }

        // the left operand fixes the comparison type
        if valid && !deferred && lhs_type != rhs_type {
            self.errors.push(Error::new(
                ErrorKind::OperandTypeMismatch {
                    rule: spec::ids::OPERAND_TYPES,
                    op: op.to_string(),
                    expected: type_name(lhs_type.expect("scalar operands have a static type")),
                    found: self.found_type(rhs),
                },
                rhs.range,
            ));
            valid = false;
        }

        valid
    }

    fn eval_binary(&mut self, op: ast::BinOp, lhs: Const, rhs: Const, range: SrcRange) -> Option<Folded> {
        let value = match op_class(op) {
            OpClass::Arith => {
                let (Const::Num(lhs), Const::Num(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as numbers");
                };
                match eval_arith(op, lhs, rhs) {
                    Some(number) => Value::Num(number),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonFiniteResult {
                                rule: spec::ids::FINITE_NUMBERS,
                            },
                            range,
                        ));
                        return None;
                    }
                }
            }
            OpClass::Cmp => {
                let (Const::Num(lhs), Const::Num(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as numbers");
                };
                Value::Bool(eval_cmp(op, lhs, rhs))
            }
            OpClass::Eq => {
                let equal = match (lhs, rhs) {
                    (Const::Str(lhs), Const::Str(rhs)) => lhs == rhs,
                    (Const::Bool(lhs), Const::Bool(rhs)) => lhs == rhs,
                    (Const::Num(Number::Int(lhs)), Const::Num(Number::Int(rhs))) => lhs == rhs,
                    (Const::Num(lhs), Const::Num(rhs)) => float_bound(lhs) == float_bound(rhs),
                    _ => unreachable!("operands were type checked as matching scalars"),
                };
                Value::Bool(if op == ast::BinOp::Eq { equal } else { !equal })
            }
            OpClass::Logic => {
                let (Const::Bool(lhs), Const::Bool(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as booleans");
                };
                Value::Bool(match op {
                    ast::BinOp::And => lhs && rhs,
                    ast::BinOp::Or => lhs || rhs,
                    _ => unreachable!("operator is logical"),
                })
            }
        };

        Some(Folded::value(value, range))
    }

    // none = unknown, out of scope, or an invalid definition; all diagnosed
    fn lookup_var(&mut self, name: String, range: SrcRange) -> Option<VarDef> {
        match self.scopes.iter().rev().find_map(|scope| scope.vars.get(&name)) {
            Some(def) => def.clone(),
            None if self.declared.contains(&name) => {
                self.errors.push(Error::new(
                    ErrorKind::VarNotInScope {
                        rule: spec::ids::VISIBLE_VARS,
                        name,
                    },
                    range,
                ));
                None
            }
            None => {
                self.errors.push(Error::new(
                    ErrorKind::UnknownVariable {
                        rule: spec::ids::DEFINED_VARS,
                        name,
                    },
                    range,
                ));
                None
            }
        }
    }

    // a loop binding visible from the innermost frame outward
    fn lookup_loop(&self, name: &str) -> Option<Folded> {
        self.scopes.iter().rev().find_map(|scope| scope.loops.get(name)).cloned()
    }

    // the definition value behind a var name, chasing var-to-var definitions
    fn var_value(&self, name: &str) -> Option<&Folded> {
        let def = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.vars.get(name))?
            .as_ref()?;
        match &def.value.kind {
            FoldedKind::Value(Value::VarRef(inner)) => self.var_value(inner),
            _ => Some(&def.value),
        }
    }

    fn model_child(&mut self, block: ast::Block, parent: spec::Id) -> Option<Child> {
        let ast::Block {
            kind,
            name,
            decls,
            range,
            ..
        } = block;
        let Some(desc) = spec::SPEC.block(&kind) else {
            self.errors.push(Error::new(
                ErrorKind::UnknownBlock {
                    keyword: kind,
                    parent: spec::Place::Block { id: parent },
                },
                range,
            ));
            return None;
        };

        if !desc.allows(spec::Place::Block { id: parent }) {
            self.errors.push(Error::new(
                ErrorKind::BlockNotAllowed {
                    block: desc.id,
                    parent: spec::Place::Block { id: parent },
                },
                range,
            ));
            return None;
        }

        let name = self.model_name(name, range, desc);

        // the sym node opens before vars collect so block refs in var values
        // anchor inside the declaring block; each model_* leaves it
        let child = if desc.id == spec::ids::REPEAT {
            // index and count address the current iteration under repeat.<name>
            if let Some(name) = &name
                && (name == "index" || name == "count")
            {
                self.errors.push(Error::new(
                    ErrorKind::ReservedRepeatName {
                        rule: spec::ids::RESERVED_REPEAT_NAMES,
                        name: name.clone(),
                    },
                    range,
                ));
            }
            let node = self.enter_sym(BlockKind::Repeat, name.clone());
            self.model_repeat(node, name, decls, desc, range).map(Child::Repeat)
        } else if desc.id == spec::ids::CHOICE {
            let node = self.enter_sym(BlockKind::Choice, name.clone());
            self.model_choice(node, name, decls, desc, range).map(Child::Choice)
        } else if desc.id == spec::ids::MAYBE {
            let node = self.enter_sym(BlockKind::Maybe, name.clone());
            self.model_maybe(node, name, decls, desc, range).map(Child::Maybe)
        } else {
            let kind = if desc.id == spec::ids::TASK {
                BlockKind::Task
            } else if desc.id == spec::ids::LLM {
                BlockKind::Llm
            } else if desc.id == spec::ids::TOOL {
                BlockKind::Tool
            } else {
                BlockKind::Function
            };
            let node = self.enter_sym(kind, name.clone());
            self.model_span(node, name, decls, desc, range).map(Child::Span)
        };
        self.leave_sym();
        child
    }

    fn model_span(
        &mut self,
        node: NodeId,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Span> {
        let span_kind = if desc.id == spec::ids::TASK {
            SpanKind::Task
        } else if desc.id == spec::ids::LLM {
            SpanKind::Llm
        } else if desc.id == spec::ids::TOOL {
            SpanKind::Tool
        } else if desc.id == spec::ids::FUNCTION {
            SpanKind::Function
        } else {
            unreachable!("block {} does not have a model lowering", desc.id.as_str());
        };

        let (decls, bindings) = self.enter_scope(decls);
        let (fields, blocks) = self.model_body(decls, desc, range);
        self.record_fields(node, &fields);
        let children = blocks
            .into_iter()
            .filter_map(|block| self.model_child(block, desc.id))
            .collect();
        self.scopes.pop();

        name.map(|name| Span {
            node,
            name,
            kind: span_kind,
            fields,
            bindings,
            children,
        })
    }

    fn model_repeat(
        &mut self,
        node: NodeId,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Repeat> {
        // the block's vars collect first but their frame opens after count
        // models: count is drawn once in the parent scope, while the vars
        // re-evaluate per iteration, so count referencing them is out of scope
        let (vars_blocks, decls) = split_vars(decls);
        let mut scope = Scope::default();
        let mut bindings = Vec::new();
        self.repeat_depth += 1;
        for block in vars_blocks {
            self.collect_vars(block, &mut scope, &mut bindings);
        }
        self.repeat_depth -= 1;

        let (mut fields, blocks) = self.model_dynamic_body(decls, desc, range);

        self.repeat_depth += 1;
        self.scopes.push(scope);
        let children = self.model_dynamic_children(blocks, desc, range);
        self.scopes.pop();
        self.repeat_depth -= 1;

        let (count, count_range) = fields.remove(&spec::ids::COUNT)?;

        // constant counts validate now, dynamic ones fail the run during generation
        if let Value::Num(number) = &count {
            let valid = match number {
                Number::Int(value) => {
                    if *value < 0 {
                        self.errors.push(Error::new(
                            ErrorKind::NegativeRepeatCount {
                                rule: spec::ids::REPEAT_COUNT,
                            },
                            count_range,
                        ));
                    }
                    *value >= 0
                }
                Number::Float(_) => {
                    self.errors.push(Error::new(
                        ErrorKind::NonIntegerRepeatCount {
                            rule: spec::ids::REPEAT_COUNT,
                        },
                        count_range,
                    ));
                    false
                }
            };
            if !valid {
                return None;
            }
        }

        Some(Repeat {
            node,
            name,
            count,
            count_range,
            bindings,
            children,
        })
    }

    fn model_choice(
        &mut self,
        node: NodeId,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Choice> {
        let (decls, bindings) = self.enter_scope(decls);
        let (_, blocks) = self.model_dynamic_body(decls, desc, range);
        let children = self.model_dynamic_children(blocks, desc, range);
        self.scopes.pop();

        Some(Choice {
            node,
            name,
            bindings,
            children,
        })
    }

    fn model_maybe(
        &mut self,
        node: NodeId,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Maybe> {
        // chance models outside the block's own scope, like a repeat count
        let (vars_blocks, decls) = split_vars(decls);
        let mut scope = Scope::default();
        let mut bindings = Vec::new();
        for block in vars_blocks {
            self.collect_vars(block, &mut scope, &mut bindings);
        }

        let (mut fields, blocks) = self.model_dynamic_body(decls, desc, range);

        self.scopes.push(scope);
        let children = self.model_dynamic_children(blocks, desc, range);
        self.scopes.pop();

        let (chance, chance_range) = fields
            .remove(&spec::ids::CHANCE)
            .unwrap_or((Value::Num(Number::Float(0.5)), range));

        // constant chances validate now, dynamic ones fail the run during generation
        if let Value::Num(number) = &chance {
            let value = match number {
                Number::Int(value) => *value as f64,
                Number::Float(value) => *value,
            };
            if !(0.0..=1.0).contains(&value) {
                self.errors.push(Error::new(
                    ErrorKind::ChanceOutOfRange {
                        rule: spec::ids::MAYBE_CHANCE,
                    },
                    chance_range,
                ));
                return None;
            }
        }

        Some(Maybe {
            node,
            name,
            chance,
            chance_range,
            bindings,
            children,
        })
    }

    // mirrors model_body for dynamic blocks: config attrs must be numbers but may
    // stay dynamic, child blocks come back for the caller to recurse
    fn model_dynamic_body(
        &mut self,
        decls: Vec<ast::Decl>,
        block: &spec::BlockDesc,
        range: SrcRange,
    ) -> (HashMap<spec::Id, (Value, SrcRange)>, Vec<ast::Block>) {
        let mut fields = HashMap::new();
        let mut seen = HashSet::new();
        let mut blocks = Vec::new();

        for decl in decls {
            match decl {
                ast::Decl::Block(inner) => blocks.push(inner),
                ast::Decl::Attr(attr) => {
                    let attr_range = attr.range;
                    let Some(field) = block.field(&attr.key) else {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownField {
                                block: block.id,
                                keyword: attr.key,
                            },
                            attr_range,
                        ));
                        continue;
                    };
                    let field_id = field.id;

                    if !seen.insert(field_id) {
                        self.errors.push(Error::new(
                            ErrorKind::DuplicateField {
                                block: block.id,
                                field: field_id,
                            },
                            attr_range,
                        ));
                        continue;
                    }

                    let Some(value) = self.fold_expr(attr.value) else {
                        continue;
                    };

                    // structure must be decidable before any span exists, so a
                    // config value reaching a block reference gets its own
                    // diagnostic ahead of the type check
                    if self.folded_reaches_block_ref(&value) {
                        self.errors.push(Error::new(
                            ErrorKind::StructuralBlockRef {
                                rule: spec::ids::STATIC_STRUCTURE,
                                field: field.keyword,
                            },
                            value.range,
                        ));
                        continue;
                    }

                    if self.static_type(&value) != Some(StaticType::Number) {
                        self.errors.push(Error::new(
                            ErrorKind::TypeMismatch {
                                block: block.id,
                                field: field_id,
                                expected: &spec::ExprType::Number,
                                found: self.found_type(&value),
                            },
                            value.range,
                        ));
                        continue;
                    }

                    // diagnostics for bad values point at the value, not the key
                    let value_range = value.range;
                    fields.insert(field_id, (value.into_value(), value_range));
                }
            }
        }

        for field in block.body.fields {
            if field.cardinality == spec::Cardinality::Required && !seen.contains(&field.id) {
                self.errors.push(Error::new(
                    ErrorKind::MissingField {
                        block: block.id,
                        field: field.id,
                    },
                    range,
                ));
            }
        }

        (fields, blocks)
    }

    fn model_dynamic_children(&mut self, blocks: Vec<ast::Block>, block: &spec::BlockDesc, range: SrcRange) -> Vec<Child> {
        if blocks.is_empty() {
            self.errors.push(Error::new(
                ErrorKind::EmptyDynamicBlock {
                    rule: spec::ids::DYNAMIC_CHILDREN,
                    block: block.id,
                },
                range,
            ));
        }

        blocks
            .into_iter()
            .filter_map(|inner| self.model_child(inner, block.id))
            .collect()
    }

    fn model_body(&mut self, decls: Vec<ast::Decl>, block: &spec::BlockDesc, range: SrcRange) -> (SpanFields, Vec<ast::Block>) {
        let mut fields = FieldsBuilder::default();
        let mut blocks = Vec::new();

        for decl in decls {
            match decl {
                ast::Decl::Block(block) => blocks.push(block),
                ast::Decl::Attr(attr) => self.model_field(&mut fields, block, attr),
            }
        }

        for field in block.body.fields {
            if field.cardinality == spec::Cardinality::Required && !fields.seen.contains(&field.id) {
                self.errors.push(Error::new(
                    ErrorKind::MissingField {
                        block: block.id,
                        field: field.id,
                    },
                    range,
                ));
            }
        }

        (fields.finish(), blocks)
    }

    fn model_field(&mut self, fields: &mut FieldsBuilder, block: &spec::BlockDesc, attr: ast::Attr) {
        let range = attr.range;
        let Some(field) = block.field(&attr.key) else {
            if !block.body.open {
                self.errors.push(Error::new(
                    ErrorKind::UnknownField {
                        block: block.id,
                        keyword: attr.key,
                    },
                    range,
                ));
            }
            return;
        };

        if field.cardinality != spec::Cardinality::Repeated && !fields.seen.insert(field.id) {
            self.errors.push(Error::new(
                ErrorKind::DuplicateField {
                    block: block.id,
                    field: field.id,
                },
                range,
            ));
            return;
        }

        // the folding field is the slot references inside it flow from
        let node = self.sym_stack.last().copied().expect("fields model inside a block");
        let model_field = model_field_kind(field.id);
        let slot = model_field.map(|field| SlotId::Field { node, field, key: None });
        let outer_slot = std::mem::replace(&mut self.current_slot, slot);
        let pending_start = self.pending.len();
        let edges_start = self.edges.len();
        let value = self.fold_expr(attr.value);
        self.current_slot = outer_slot;
        let Some(value) = value else {
            return;
        };

        // metadata and metrics evaluate per key, so references recorded inside
        // an object literal re-attribute to the key that contains them
        if matches!(model_field, Some(Field::Metadata | Field::Metrics))
            && let FoldedKind::Object(items) = &value.kind
        {
            // the item range is the key token, references live in the value
            let key_of = |at: SrcRange| {
                items
                    .iter()
                    .find(|item| item.value.range.start <= at.start && at.end <= item.value.range.end)
                    .map(|item| item.key.clone())
            };
            for pending in &mut self.pending[pending_start..] {
                if let Some(SlotId::Field { key: key @ None, .. }) = &mut pending.slot
                    && let Some(found) = key_of(pending.range)
                {
                    *key = Some(found);
                }
            }
            for (from, _, at) in &mut self.edges[edges_start..] {
                if let SlotId::Field { key: key @ None, .. } = from
                    && let Some(found) = key_of(*at)
                {
                    *key = Some(found);
                }
            }
        }

        if !self.validate_value(&value, block.id, field.id, field.value) {
            return;
        }

        if field.id == spec::ids::METRICS && !self.validate_metric_keys(&value) {
            return;
        }

        let value = self.model_field_value(value, field.value);

        match (field.id, value) {
            (spec::ids::INPUT, FieldValue::Value(value)) => fields.input = Some(value),
            (spec::ids::OUTPUT, FieldValue::Value(value)) => fields.output = Some(value),
            (spec::ids::EXPECTED, FieldValue::Value(value)) => fields.expected = Some(value),
            (spec::ids::ERROR, FieldValue::Value(value)) => fields.error = Some(value),
            (spec::ids::METADATA, FieldValue::Object(value)) => fields.metadata = Some(value),
            (spec::ids::METRICS, FieldValue::Object(value)) => fields.metrics = Some(value),
            (spec::ids::TAGS, FieldValue::Tags(value)) => fields.tags = Some(value),
            _ => unreachable!("field {} does not match its model lowering", field.id.as_str()),
        }
    }

    fn validate_metric_keys(&mut self, folded: &Folded) -> bool {
        let FoldedKind::Object(fields) = &folded.kind else {
            unreachable!("expression was validated as an object");
        };

        let mut valid = true;

        for field in fields {
            if spec::RESERVED_METRIC_KEYS.contains(&field.key.as_str()) {
                self.errors.push(Error::new(
                    ErrorKind::ReservedMetricKey {
                        rule: spec::ids::RESERVED_METRICS,
                        key: field.key.clone(),
                    },
                    field.range,
                ));
                valid = false;
            }
        }

        valid
    }

    // fold, not all(): validate every element so each invalid item gets its own diagnostic
    #[allow(clippy::unnecessary_fold)]
    fn validate_value(&mut self, folded: &Folded, block: spec::Id, field: spec::Id, expected: &'static spec::ExprType) -> bool {
        let valid = match expected {
            spec::ExprType::Any => true,
            spec::ExprType::String => matches!(folded.kind, FoldedKind::Value(Value::Str(_) | Value::Template(_))),
            spec::ExprType::Number => matches!(folded.kind, FoldedKind::Value(Value::Num(_))),
            spec::ExprType::Boolean => matches!(folded.kind, FoldedKind::Value(Value::Bool(_))),
            spec::ExprType::Array { items } => {
                let FoldedKind::Array(values) = &folded.kind else {
                    self.push_type_mismatch(folded, block, field, expected);
                    return false;
                };

                return values
                    .iter()
                    .fold(true, |valid, value| self.validate_value(value, block, field, items) && valid);
            }
            spec::ExprType::Object { values } => {
                let FoldedKind::Object(fields) = &folded.kind else {
                    self.push_type_mismatch(folded, block, field, expected);
                    return false;
                };

                return fields.iter().fold(true, |valid, item| {
                    self.validate_value(&item.value, block, field, values) && valid
                });
            }
        };

        if !valid {
            self.push_type_mismatch(folded, block, field, expected);
        }

        valid
    }

    fn push_type_mismatch(&mut self, folded: &Folded, block: spec::Id, field: spec::Id, expected: &'static spec::ExprType) {
        self.errors.push(Error::new(
            ErrorKind::TypeMismatch {
                block,
                field,
                expected,
                found: ExprType::of(folded),
            },
            folded.range,
        ));
    }

    fn model_field_value(&mut self, folded: Folded, expected: &spec::ExprType) -> FieldValue {
        match expected {
            spec::ExprType::Any => FieldValue::Value(folded.into_value()),
            spec::ExprType::Object { values } if matches!(**values, spec::ExprType::Any) => {
                FieldValue::Object(require_object(folded))
            }
            spec::ExprType::Array { items } if matches!(**items, spec::ExprType::String) => {
                FieldValue::Tags(require_tags(folded))
            }
            _ => unreachable!("expression constraint does not have a model lowering"),
        }
    }

    fn model_func(&mut self, name: String, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        // the descriptor's name doubles as the &'static str for diagnostics
        let Some(func) = spec::SPEC.function(&name).map(|desc| desc.name) else {
            self.errors.push(Error::new(
                ErrorKind::UnknownFunction {
                    rule: spec::ids::KNOWN_FUNCTIONS,
                    name,
                },
                range,
            ));
            return None;
        };

        match func {
            "choice" => {
                if args.is_empty() {
                    self.errors.push(Error::new(
                        ErrorKind::EmptyChoice {
                            rule: spec::ids::CHOICE_ALTERNATIVES,
                        },
                        range,
                    ));
                    return None;
                }

                Some(Func::Choice(args.into_iter().map(Folded::into_value).collect()))
            }
            "range" => {
                let Ok([min, max]) = <[Folded; 2]>::try_from(args) else {
                    self.errors.push(Error::new(
                        ErrorKind::InvalidRangeArgs {
                            rule: spec::ids::RANGE_BOUNDS,
                        },
                        range,
                    ));
                    return None;
                };
                let min = self.model_range_bound(min);
                let max = self.model_range_bound(max)?;
                let min = min?;

                let bounds = match (min, max) {
                    (Number::Int(min), Number::Int(max)) => Range::Int { min, max },
                    (min, max) => Range::Float {
                        min: float_bound(min),
                        max: float_bound(max),
                    },
                };
                let ordered = match bounds {
                    Range::Int { min, max } => min <= max,
                    Range::Float { min, max } => min <= max,
                };

                if !ordered {
                    self.errors.push(Error::new(
                        ErrorKind::InvalidRangeBounds {
                            rule: spec::ids::RANGE_BOUNDS,
                        },
                        range,
                    ));
                    return None;
                }

                Some(Func::Range(bounds))
            }

            "weighted" => self.model_weighted(args, range),

            "normal" => {
                let [mean, stddev] = self.func_args(func, args, "exactly two arguments (mean, stddev)", range)?;
                let mean = self.model_dist_param(mean, func, "mean", "a finite number", |_| true);
                let stddev = self.model_dist_param(stddev, func, "stddev", "non-negative", |value| value >= 0.0);
                Some(Func::Normal {
                    mean: mean?,
                    stddev: stddev?,
                })
            }
            "lognormal" => {
                let [median, sigma] = self.func_args(func, args, "exactly two arguments (median, sigma)", range)?;
                let median = self.model_dist_param(median, func, "median", "positive", |value| value > 0.0);
                let sigma = self.model_dist_param(sigma, func, "sigma", "non-negative", |value| value >= 0.0);
                Some(Func::Lognormal {
                    median: median?,
                    sigma: sigma?,
                })
            }
            "exponential" => {
                let [mean] = self.func_args(func, args, "exactly one argument (mean)", range)?;
                let mean = self.model_dist_param(mean, func, "mean", "positive", |value| value > 0.0)?;
                Some(Func::Exponential { mean })
            }
            "pareto" => {
                let [min, shape] = self.func_args(func, args, "exactly two arguments (min, shape)", range)?;
                let min = self.model_dist_param(min, func, "min", "positive", |value| value > 0.0);
                let shape = self.model_dist_param(shape, func, "shape", "positive", |value| value > 0.0);
                Some(Func::Pareto {
                    min: min?,
                    shape: shape?,
                })
            }
            "beta" => {
                let [alpha, beta] = self.func_args(func, args, "exactly two arguments (alpha, beta)", range)?;
                let alpha = self.model_dist_param(alpha, func, "alpha", "positive", |value| value > 0.0);
                let beta = self.model_dist_param(beta, func, "beta", "positive", |value| value > 0.0);
                Some(Func::Beta {
                    alpha: alpha?,
                    beta: beta?,
                })
            }
            "poisson" => {
                let [mean] = self.func_args(func, args, "exactly one argument (mean)", range)?;
                // the sampler rejects astronomically large means, surface that here
                let mean = self.model_dist_param(mean, func, "mean", "positive and below 1e15", |value| {
                    value > 0.0 && value < 1.0e15
                })?;
                Some(Func::Poisson { mean })
            }
            "chance" => {
                let [probability] = self.func_args(func, args, "exactly one argument (probability)", range)?;
                let probability = self.model_dist_param(probability, func, "probability", "between 0 and 1", |value| {
                    (0.0..=1.0).contains(&value)
                })?;
                Some(Func::Chance { probability })
            }

            "upper" | "lower" | "trim" => {
                let [text] = self.func_args(func, args, "exactly one argument", range)?;
                let text = Box::new(self.model_typed_arg(text, func, StaticType::String)?);
                Some(match func {
                    "upper" => Func::Upper { text },
                    "lower" => Func::Lower { text },
                    _ => Func::Trim { text },
                })
            }
            "replace" => {
                let [text, from, to] = self.func_args(func, args, "exactly three arguments (text, from, to)", range)?;
                let text = self.model_typed_arg(text, func, StaticType::String);
                let from = self.model_typed_arg(from, func, StaticType::String);
                let to = self.model_typed_arg(to, func, StaticType::String);
                Some(Func::Replace {
                    text: Box::new(text?),
                    from: Box::new(from?),
                    to: Box::new(to?),
                })
            }
            "split" => {
                let [text, separator] = self.func_args(func, args, "exactly two arguments (text, separator)", range)?;
                if const_string(&separator).is_some_and(|separator: String| separator.is_empty()) {
                    self.errors.push(Error::new(
                        ErrorKind::EmptySplitSeparator {
                            rule: spec::ids::SPLIT_SEPARATOR,
                        },
                        separator.range,
                    ));
                    return None;
                }
                let text = self.model_typed_arg(text, func, StaticType::String);
                let separator = self.model_typed_arg(separator, func, StaticType::String);
                Some(Func::Split {
                    text: Box::new(text?),
                    separator: Box::new(separator?),
                })
            }
            "join" => {
                let [array, separator] = self.func_args(func, args, "exactly two arguments (array, separator)", range)?;
                let array = self.model_typed_arg(array, func, StaticType::Array);
                let separator = self.model_typed_arg(separator, func, StaticType::String);
                Some(Func::Join {
                    array: Box::new(array?),
                    separator: Box::new(separator?),
                })
            }
            "contains" => {
                let [target, needle] = self.func_args(func, args, "exactly two arguments (target, needle)", range)?;
                match self.static_type(&target) {
                    Some(StaticType::String) => {
                        let needle = self.model_typed_arg(needle, func, StaticType::String);
                        Some(Func::Contains {
                            target: Box::new(target.into_value()),
                            needle: Box::new(needle?),
                        })
                    }
                    Some(StaticType::Array) => {
                        let needle = self.model_scalar_arg(needle, func);
                        Some(Func::Contains {
                            target: Box::new(target.into_value()),
                            needle: Box::new(needle?),
                        })
                    }
                    _ => {
                        self.push_func_arg_type(&target, func, "string or array");
                        None
                    }
                }
            }
            "starts_with" | "ends_with" => {
                let [text, affix] = self.func_args(func, args, "exactly two arguments", range)?;
                let text = self.model_typed_arg(text, func, StaticType::String);
                let affix = self.model_typed_arg(affix, func, StaticType::String);
                let (text, affix) = (Box::new(text?), Box::new(affix?));
                Some(if func == "starts_with" {
                    Func::StartsWith { text, prefix: affix }
                } else {
                    Func::EndsWith { text, suffix: affix }
                })
            }
            "len" => {
                let [target] = self.func_args(func, args, "exactly one argument", range)?;
                let known = matches!(self.static_type(&target), Some(StaticType::String | StaticType::Array));
                if !known && !self.defers_to_generation(&target) {
                    self.push_func_arg_type(&target, func, "string or array");
                    return None;
                }
                Some(Func::Len {
                    target: Box::new(target.into_value()),
                })
            }
            "tokens" => {
                let [value] = self.func_args(func, args, "exactly one argument", range)?;
                let known = matches!(
                    self.static_type(&value),
                    Some(StaticType::String | StaticType::Array | StaticType::Object)
                );
                if !known && !self.defers_to_generation(&value) {
                    self.push_func_arg_type(&value, func, "string, array, or object");
                    return None;
                }
                Some(Func::Tokens {
                    value: Box::new(value.into_value()),
                })
            }
            "format" => self.model_format(args, range),

            "clamp" => {
                let [value, min, max] = self.func_args(func, args, "exactly three arguments (value, min, max)", range)?;
                // constant bounds must already be ordered
                if let (FoldedKind::Value(Value::Num(low)), FoldedKind::Value(Value::Num(high))) = (&min.kind, &max.kind)
                    && float_bound(low.clone()) > float_bound(high.clone())
                {
                    self.errors.push(Error::new(
                        ErrorKind::ClampBoundsOutOfOrder {
                            rule: spec::ids::CLAMP_BOUNDS,
                        },
                        range,
                    ));
                    return None;
                }
                let value = self.model_typed_arg(value, func, StaticType::Number);
                let min = self.model_typed_arg(min, func, StaticType::Number);
                let max = self.model_typed_arg(max, func, StaticType::Number);
                Some(Func::Clamp {
                    value: Box::new(value?),
                    min: Box::new(min?),
                    max: Box::new(max?),
                })
            }
            "round" | "floor" | "ceil" | "abs" => {
                let [value] = self.func_args(func, args, "exactly one argument", range)?;
                let value = Box::new(self.model_typed_arg(value, func, StaticType::Number)?);
                Some(match func {
                    "round" => Func::Round { value },
                    "floor" => Func::Floor { value },
                    "ceil" => Func::Ceil { value },
                    _ => Func::Abs { value },
                })
            }
            "min" | "max" => {
                if args.len() < 2 {
                    self.errors.push(Error::new(
                        ErrorKind::FuncArity {
                            rule: spec::ids::FUNC_ARITY,
                            func,
                            expected: "at least two arguments",
                        },
                        range,
                    ));
                    return None;
                }
                let mut values = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.model_typed_arg(arg, func, StaticType::Number) {
                        Some(value) => values.push(value),
                        None => valid = false,
                    }
                }
                valid.then(|| if func == "min" { Func::Min(values) } else { Func::Max(values) })
            }

            "uuid" => {
                if !args.is_empty() {
                    self.errors.push(Error::new(
                        ErrorKind::FuncArity {
                            rule: spec::ids::FUNC_ARITY,
                            func,
                            expected: "no arguments",
                        },
                        range,
                    ));
                    return None;
                }
                Some(Func::Uuid)
            }
            "hex" | "alphanum" => {
                let [length] = self.func_args(func, args, "exactly one argument (length)", range)?;
                let length = self.model_random_length(length, func)?;
                Some(if func == "hex" {
                    Func::Hex { length }
                } else {
                    Func::Alphanum { length }
                })
            }

            _ => unreachable!("function {name} does not have a model lowering"),
        }
    }

    fn model_weighted(&mut self, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        if args.is_empty() {
            self.errors.push(Error::new(
                ErrorKind::FuncArity {
                    rule: spec::ids::FUNC_ARITY,
                    func: "weighted",
                    expected: "at least one `[value, weight]` pair",
                },
                range,
            ));
            return None;
        }

        let mut options = Vec::with_capacity(args.len());
        let mut valid = true;
        for arg in args {
            let arg_range = arg.range;
            let FoldedKind::Array(mut elems) = arg.kind else {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            };
            if elems.len() != 2 {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            }
            let weight_value = elems.pop().expect("pair has two elements");
            let value = elems.pop().expect("pair has two elements");

            let weight_range = weight_value.range;
            let weight = match weight_value.kind {
                FoldedKind::Value(Value::Num(number)) => Some(float_bound(number)),
                _ => None,
            };
            let weight = match weight {
                Some(weight) if weight >= 0.0 => Some(weight),
                _ => {
                    self.push_weighted_option(weight_range);
                    None
                }
            };

            match weight {
                Some(weight) => options.push(WeightedOption {
                    value: value.into_value(),
                    weight,
                }),
                None => valid = false,
            }
        }

        if !valid {
            return None;
        }
        if options.iter().map(|option| option.weight).sum::<f64>() <= 0.0 {
            self.errors.push(Error::new(
                ErrorKind::WeightedTotal {
                    rule: spec::ids::WEIGHTED_OPTIONS,
                },
                range,
            ));
            return None;
        }

        Some(Func::Weighted(options))
    }

    fn model_format(&mut self, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        let mut args = args.into_iter();
        let Some(template_value) = args.next() else {
            self.errors.push(Error::new(
                ErrorKind::FuncArity {
                    rule: spec::ids::FUNC_ARITY,
                    func: "format",
                    expected: "a constant string template followed by one value per `{}` placeholder",
                },
                range,
            ));
            return None;
        };

        let Some(template) = const_string(&template_value) else {
            self.push_func_arg_type(&template_value, "format", "constant string");
            return None;
        };

        let rest: Vec<_> = args.collect();
        let pieces: Vec<String> = template.split("{}").map(str::to_owned).collect();
        let placeholders = pieces.len() - 1;
        if placeholders != rest.len() {
            self.errors.push(Error::new(
                ErrorKind::FormatPlaceholders {
                    rule: spec::ids::FORMAT_TEMPLATE,
                    placeholders,
                    args: rest.len(),
                },
                range,
            ));
            return None;
        }

        let mut values = Vec::with_capacity(rest.len());
        let mut valid = true;
        for arg in rest {
            match self.model_scalar_arg(arg, "format") {
                Some(value) => values.push(value),
                None => valid = false,
            }
        }

        valid.then_some(Func::Format { pieces, args: values })
    }

    fn func_args<const N: usize>(
        &mut self,
        func: &'static str,
        args: Vec<Folded>,
        expected: &'static str,
        range: SrcRange,
    ) -> Option<[Folded; N]> {
        match <[Folded; N]>::try_from(args) {
            Ok(args) => Some(args),
            Err(_) => {
                self.errors.push(Error::new(
                    ErrorKind::FuncArity {
                        rule: spec::ids::FUNC_ARITY,
                        func,
                        expected,
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_typed_arg(&mut self, arg: Folded, func: &'static str, required: StaticType) -> Option<Value> {
        if self.static_type(&arg) != Some(required) && !self.defers_to_generation(&arg) {
            self.push_func_arg_type(&arg, func, type_name(required));
            return None;
        }
        Some(arg.into_value())
    }

    fn model_scalar_arg(&mut self, arg: Folded, func: &'static str) -> Option<Value> {
        let scalar = matches!(
            self.static_type(&arg),
            Some(StaticType::String | StaticType::Number | StaticType::Boolean)
        );
        if !scalar && !self.defers_to_generation(&arg) {
            self.push_func_arg_type(&arg, func, "string, number, or boolean");
            return None;
        }
        Some(arg.into_value())
    }

    fn push_func_arg_type(&mut self, arg: &Folded, func: &'static str, expected: &'static str) {
        self.errors.push(Error::new(
            ErrorKind::FuncArgType {
                rule: spec::ids::FUNC_ARG_TYPES,
                func,
                expected,
                found: self.found_type(arg),
            },
            arg.range,
        ));
    }

    fn push_weighted_option(&mut self, range: SrcRange) {
        self.errors.push(Error::new(
            ErrorKind::WeightedOptionShape {
                rule: spec::ids::WEIGHTED_OPTIONS,
            },
            range,
        ));
    }

    // constant distribution parameter, already folded to a value when constant
    fn model_dist_param(
        &mut self,
        arg: Folded,
        func: &'static str,
        param: &'static str,
        expected: &'static str,
        valid: impl Fn(f64) -> bool,
    ) -> Option<f64> {
        let range = arg.range;
        let number = match arg.kind {
            FoldedKind::Value(Value::Num(number)) => number,
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonConstantParam {
                        rule: spec::ids::DIST_PARAMS,
                        func,
                        param,
                    },
                    range,
                ));
                return None;
            }
        };

        let value = float_bound(number);
        if !valid(value) {
            self.errors.push(Error::new(
                ErrorKind::ParamOutOfRange {
                    rule: spec::ids::DIST_PARAMS,
                    func,
                    param,
                    expected,
                },
                range,
            ));
            return None;
        }

        Some(value)
    }

    fn model_random_length(&mut self, arg: Folded, func: &'static str) -> Option<usize> {
        let range = arg.range;
        let number = match arg.kind {
            FoldedKind::Value(Value::Num(number)) => number,
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonConstantParam {
                        rule: spec::ids::RANDOM_LENGTH,
                        func,
                        param: "length",
                    },
                    range,
                ));
                return None;
            }
        };

        match number {
            Number::Int(value) if value >= 0 => Some(value as usize),
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::ParamOutOfRange {
                        rule: spec::ids::RANDOM_LENGTH,
                        func,
                        param: "length",
                        expected: "a non-negative integer",
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_range_bound(&mut self, bound: Folded) -> Option<Number> {
        match bound.kind {
            FoldedKind::Value(Value::Num(number)) => Some(number),
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::InvalidRangeArgs {
                        rule: spec::ids::RANGE_BOUNDS,
                    },
                    bound.range,
                ));
                None
            }
        }
    }

    // resolves a context path or emits the diagnostic shared by both syntaxes
    fn require_ctx_ref(&mut self, path: &[String], range: SrcRange) -> Option<CtxRef> {
        if let Some(ctx_ref) = model_ctx_ref(path, self.repeat_depth) {
            return Some(ctx_ref);
        }

        // a known reference in the wrong place gets its own diagnostic
        let kind = match path {
            [first, second] if first == "repeat" && (second == "index" || second == "count") => {
                ErrorKind::RepeatRefOutsideRepeat {
                    rule: spec::ids::REPEAT_REFS,
                    path: path.join("."),
                }
            }
            _ => ErrorKind::UnknownReference {
                rule: spec::ids::KNOWN_REFERENCES,
                path: path.join("."),
            },
        };
        self.errors.push(Error::new(kind, range));
        None
    }

    // the one place a number literal parses; negated folds a leading minus
    // into the literal so i64::MIN stays representable
    fn model_number(&mut self, raw: String, negated: bool, range: SrcRange) -> Option<Number> {
        let raw = if negated { format!("-{raw}") } else { raw };
        let number = if raw.contains(['.', 'e', 'E']) {
            raw.parse::<f64>().ok().filter(|number| number.is_finite()).map(Number::Float)
        } else {
            raw.parse::<i64>().ok().map(Number::Int)
        };

        if number.is_none() {
            self.errors.push(Error::new(
                ErrorKind::InvalidNumber {
                    rule: spec::ids::FINITE_NUMBERS,
                    raw,
                },
                range,
            ));
        }

        number
    }

    // statically known result type, none = unknown (eg heterogeneous choice)
    fn static_type(&self, folded: &Folded) -> Option<StaticType> {
        match &folded.kind {
            FoldedKind::Array(_) => Some(StaticType::Array),
            FoldedKind::Object(_) => Some(StaticType::Object),
            FoldedKind::Value(value) => self.value_type(value),
        }
    }

    fn value_type(&self, value: &Value) -> Option<StaticType> {
        match value {
            Value::Str(_) | Value::Template(_) => Some(StaticType::String),
            Value::Num(_) => Some(StaticType::Number),
            Value::Bool(_) => Some(StaticType::Boolean),
            Value::Null => Some(StaticType::Null),
            Value::Array(_) => Some(StaticType::Array),
            Value::Object(_) => Some(StaticType::Object),
            // a binding is typed by its definition
            Value::VarRef(name) => {
                let value = self.var_value(name)?;
                self.static_type(value)
            }
            // context indexes and counts are always integers
            Value::CtxRef(_) => Some(StaticType::Number),
            // a referenced field's shape is only known at generation
            Value::BlockRef { .. } => None,
            Value::Func { func, .. } => self.func_type(func),
            Value::Unary { op: UnaryOp::Neg, .. } => Some(StaticType::Number),
            Value::Unary { op: UnaryOp::Not, .. } => Some(StaticType::Boolean),
            Value::Binary { op, .. } => Some(match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => StaticType::Number,
                _ => StaticType::Boolean,
            }),
            Value::Cond { then, otherwise, .. } => {
                let then = self.value_type(then)?;
                let otherwise = self.value_type(otherwise)?;
                (then == otherwise).then_some(then)
            }
            // a dynamic index is typed when the target's elements agree
            Value::Index { target, .. } => match &**target {
                Value::Array(array) => unify_types(array.elem.iter().map(|elem| match elem {
                    ArrayElem::Item(value) => self.value_type(value),
                    // a spread's elements are unknown until generation
                    ArrayElem::Spread(_) => None,
                })),
                Value::Object(object) => unify_types(object.elem.iter().map(|field| self.value_type(&field.value))),
                Value::VarRef(name) => match self.var_value(name).map(|value| &value.kind) {
                    Some(FoldedKind::Array(values)) => unify_types(values.iter().map(|value| self.static_type(value))),
                    Some(FoldedKind::Object(fields)) => {
                        unify_types(fields.iter().map(|field| self.static_type(&field.value)))
                    }
                    _ => None,
                },
                _ => None,
            },
            // a residual slice always selects from an array
            Value::Slice { .. } => Some(StaticType::Array),
        }
    }

    // result type by function, a pick is typed when its alternatives agree
    fn func_type(&self, func: &Func) -> Option<StaticType> {
        match func {
            Func::Choice(options) => unify_types(options.iter().map(|option| self.value_type(option))),
            Func::Weighted(options) => unify_types(options.iter().map(|option| self.value_type(&option.value))),
            Func::Range(_)
            | Func::Normal { .. }
            | Func::Lognormal { .. }
            | Func::Exponential { .. }
            | Func::Pareto { .. }
            | Func::Beta { .. }
            | Func::Poisson { .. }
            | Func::Len { .. }
            | Func::Tokens { .. }
            | Func::Clamp { .. }
            | Func::Round { .. }
            | Func::Floor { .. }
            | Func::Ceil { .. }
            | Func::Abs { .. }
            | Func::Min(_)
            | Func::Max(_) => Some(StaticType::Number),
            Func::Chance { .. } | Func::Contains { .. } | Func::StartsWith { .. } | Func::EndsWith { .. } => {
                Some(StaticType::Boolean)
            }
            Func::Upper { .. }
            | Func::Lower { .. }
            | Func::Trim { .. }
            | Func::Replace { .. }
            | Func::Join { .. }
            | Func::Format { .. }
            | Func::Uuid
            | Func::Hex { .. }
            | Func::Alphanum { .. } => Some(StaticType::String),
            Func::Split { .. } => Some(StaticType::Array),
        }
    }

    // the found side of diagnostics, prefers the static type over the value shape
    fn found_type(&self, folded: &Folded) -> ExprType {
        match self.static_type(folded) {
            Some(StaticType::String) => ExprType::String,
            Some(StaticType::Number) => ExprType::Number,
            Some(StaticType::Boolean) => ExprType::Boolean,
            Some(StaticType::Null) => ExprType::Null,
            Some(StaticType::Array) => ExprType::Array,
            Some(StaticType::Object) => ExprType::Object,
            None => ExprType::of(folded),
        }
    }

    fn model_name(&mut self, name: Option<String>, range: SrcRange, block: &spec::BlockDesc) -> Option<String> {
        match block.name {
            spec::NamePolicy::Required => {
                if name.is_none() {
                    self.errors
                        .push(Error::new(ErrorKind::MissingName { block: block.id }, range));
                }
                name
            }
            spec::NamePolicy::Forbidden if name.is_some() => {
                self.errors
                    .push(Error::new(ErrorKind::UnexpectedName { block: block.id }, range));
                None
            }
            spec::NamePolicy::Optional => name,
            spec::NamePolicy::Forbidden => None,
        }
    }

    // resolves every live pending reference against the completed symbol
    // tree; dead intermediates and failures leave placeholders, which never
    // escape because the model is only returned when no error was recorded
    fn resolve_pending(&mut self, live: &[bool]) -> Vec<ResolvedRef> {
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .enumerate()
            .map(|(index, pending)| {
                if live[index] {
                    self.resolve_ref(pending)
                } else {
                    ResolvedRef {
                        up: 0,
                        steps: Vec::new(),
                        accessor: Accessor::Field(Field::Input),
                        path: Vec::new(),
                        range: pending.range,
                    }
                }
            })
            .collect()
    }

    fn resolve_ref(&mut self, pending: PendingRef) -> ResolvedRef {
        let PendingRef {
            head,
            segments,
            origin,
            slot,
            range,
        } = pending;
        let placeholder = || ResolvedRef {
            up: 0,
            steps: Vec::new(),
            accessor: Accessor::Field(Field::Input),
            path: Vec::new(),
            range,
        };
        let mut segments = segments.into_iter().peekable();
        let mut steps: Vec<Step> = Vec::new();

        // the anchor the up-walk lands on; kind heads anchor at the scope
        // whose children matched and descend from there
        let (up, mut target) = match head {
            Head::SelfBlock(node) => {
                let position = origin
                    .iter()
                    .rposition(|&at| at == node)
                    .expect("self anchors on its own origin chain");
                (origin.len() - 1 - position, Some(node))
            }
            Head::Trace => {
                let Some(&trace) = origin.first() else {
                    // a root var holding this ref was already rejected
                    return placeholder();
                };
                (origin.len().saturating_sub(1), Some(trace))
            }
            Head::Kind(kind) => {
                let name = match segments.next() {
                    Some(Segment::Name { value, .. }) => value,
                    _ => {
                        self.errors.push(Error::new(
                            ErrorKind::IncompleteBlockRef {
                                rule: spec::ids::BLOCK_REFS,
                                keyword: kind.keyword(),
                            },
                            range,
                        ));
                        return placeholder();
                    }
                };

                // nearest enclosing scope with any match wins
                let found = origin.iter().enumerate().rev().find_map(|(position, &scope)| {
                    let candidates = self.matching_children(scope, kind, &name);
                    (!candidates.is_empty()).then_some((position, candidates))
                });
                let Some((position, candidates)) = found else {
                    self.errors.push(Error::new(
                        ErrorKind::UnknownBlockRef {
                            rule: spec::ids::BLOCK_REFS,
                            keyword: kind.keyword(),
                            name,
                        },
                        range,
                    ));
                    return placeholder();
                };

                let up = origin.len() - 1 - position;
                let target = if kind.has_fields() {
                    match self.consume_position(&mut segments, &mut steps, candidates, &name, range) {
                        Some(target) => target,
                        None => return placeholder(),
                    }
                } else {
                    // dynamic blocks have no positional escape, their names
                    // must be unique among matching siblings
                    match self.lone_candidate(&mut steps, candidates, &name, range) {
                        Some(target) => Some(target),
                        None => return placeholder(),
                    }
                };
                (up, target)
            }
        };

        let mut kind = match head {
            Head::Kind(kind) => kind,
            Head::SelfBlock(node) => self.syms[node.0 as usize].kind,
            Head::Trace => BlockKind::Trace,
        };
        // a repeat cursor must select an iteration before descending
        let mut selected = false;
        // references through iterations stay off the static cycle graph:
        // cross-iteration field pairs are legitimately mutual
        let mut has_iteration = false;

        // descend until a field or accessor closes the block path
        let accessor = loop {
            match segments.next() {
                // an index or slice on a repeat selects among its iterations
                Some(Segment::Index { value, range: at }) => {
                    if kind != BlockKind::Repeat || selected {
                        self.errors.push(Error::new(
                            ErrorKind::InvalidRefSegment {
                                rule: spec::ids::REF_FIELDS,
                                segment: "[...]".to_owned(),
                            },
                            at,
                        ));
                        return placeholder();
                    }
                    steps.push(Step::Iteration(value));
                    selected = true;
                    has_iteration = true;
                }
                Some(Segment::Slice { start, end, range: at }) => {
                    if kind != BlockKind::Repeat || selected {
                        self.errors.push(Error::new(
                            ErrorKind::InvalidRefSegment {
                                rule: spec::ids::REF_FIELDS,
                                segment: "[...]".to_owned(),
                            },
                            at,
                        ));
                        return placeholder();
                    }
                    // the rest of the reference projects over each iteration
                    steps.push(Step::Iterations { start, end });
                    selected = true;
                    has_iteration = true;
                }
                Some(Segment::Name { value, range: at }) => {
                    // accessors close dynamic-block paths
                    match (kind, selected, value.as_str()) {
                        (BlockKind::Repeat, false, "index" | "count") => {
                            // the current iteration only exists inside the repeat
                            let node = target.expect("repeats resolve without positions");
                            if !origin.contains(&node) {
                                self.errors.push(Error::new(
                                    ErrorKind::RepeatRefOutsideRepeat {
                                        rule: spec::ids::REPEAT_REFS,
                                        path: format!("repeat.{}.{}", self.syms[node.0 as usize].name.as_deref().unwrap_or("?"), value),
                                    },
                                    at,
                                ));
                                return placeholder();
                            }
                            break if value == "index" { Accessor::Index } else { Accessor::Count };
                        }
                        (BlockKind::Choice, _, "chosen") => break Accessor::Chosen,
                        (BlockKind::Maybe, _, "included") => break Accessor::Included,
                        _ => {}
                    }

                    if kind.has_fields()
                        && let Some(field) = field_by_name(&value)
                    {
                        // a statically known target must actually set the field
                        if let Some(node) = target
                            && !self.syms[node.0 as usize].fields.contains(&field)
                        {
                            self.errors.push(Error::new(
                                ErrorKind::AbsentFieldRef {
                                    rule: spec::ids::REF_FIELDS,
                                    field: value,
                                    label: self.sym_label(node),
                                },
                                at,
                            ));
                            return placeholder();
                        }
                        break Accessor::Field(field);
                    }

                    if let Some(child) = child_kind_by_name(&value) {
                        if kind == BlockKind::Repeat && !selected {
                            self.errors.push(Error::new(
                                ErrorKind::RepeatIterationRequired {
                                    rule: spec::ids::REF_COLLECTIONS,
                                },
                                at,
                            ));
                            return placeholder();
                        }
                        let Some(node) = target else {
                            self.errors.push(Error::new(
                                ErrorKind::DynamicPositionDescent {
                                    rule: spec::ids::REF_COLLECTIONS,
                                },
                                at,
                            ));
                            return placeholder();
                        };
                        let name = match segments.next() {
                            Some(Segment::Name { value, .. }) => value,
                            _ => {
                                self.errors.push(Error::new(
                                    ErrorKind::IncompleteBlockRef {
                                        rule: spec::ids::BLOCK_REFS,
                                        keyword: child.keyword(),
                                    },
                                    range,
                                ));
                                return placeholder();
                            }
                        };
                        let candidates = self.matching_children(node, child, &name);
                        if candidates.is_empty() {
                            self.errors.push(Error::new(
                                ErrorKind::UnknownBlockRef {
                                    rule: spec::ids::BLOCK_REFS,
                                    keyword: child.keyword(),
                                    name,
                                },
                                at,
                            ));
                            return placeholder();
                        }
                        target = if child.has_fields() {
                            match self.consume_position(&mut segments, &mut steps, candidates, &name, range) {
                                Some(next) => next,
                                None => return placeholder(),
                            }
                        } else {
                            match self.lone_candidate(&mut steps, candidates, &name, range) {
                                Some(next) => Some(next),
                                None => return placeholder(),
                            }
                        };
                        kind = child;
                        selected = false;
                        continue;
                    }

                    self.errors.push(Error::new(
                        ErrorKind::InvalidRefSegment {
                            rule: spec::ids::REF_FIELDS,
                            segment: value,
                        },
                        at,
                    ));
                    return placeholder();
                }
                None => {
                    self.errors.push(Error::new(
                        ErrorKind::IncompleteBlockRef {
                            rule: spec::ids::BLOCK_REFS,
                            keyword: match head {
                                Head::Kind(kind) => kind.keyword(),
                                Head::SelfBlock(_) => "self",
                                Head::Trace => "trace",
                            },
                        },
                        range,
                    ));
                    return placeholder();
                }
            }
        };

        // everything after the field drills into its json value at generation
        let path: Vec<Selection> = segments
            .map(|segment| match segment {
                Segment::Name { value, .. } => Selection::Index(Value::Str(value)),
                Segment::Index { value, .. } => Selection::Index(value),
                Segment::Slice { start, end, .. } => Selection::Slice { start, end },
            })
            .collect();

        // a statically known target joins the dependency graph; dynamic
        // positions and iteration steps fall to generation-time in-progress
        // detection instead
        if !has_iteration
            && let (Some(from), Some(node), Accessor::Field(field)) = (slot, target, &accessor)
        {
            let key = match (field, path.first()) {
                (Field::Metadata | Field::Metrics, Some(Selection::Index(Value::Str(key)))) => Some(key.clone()),
                _ => None,
            };
            self.edges.push((
                from,
                SlotId::Field {
                    node,
                    field: *field,
                    key,
                },
                range,
            ));
        }

        ResolvedRef {
            up,
            steps,
            accessor,
            path,
            range,
        }
    }

    // dynamic blocks have no positional collections: to be addressed, a
    // repeat, choice, or maybe must be uniquely named among its siblings
    fn lone_candidate(&mut self, steps: &mut Vec<Step>, candidates: Vec<NodeId>, name: &str, range: SrcRange) -> Option<NodeId> {
        if candidates.len() > 1 {
            self.errors.push(Error::new(
                ErrorKind::AmbiguousBlockRef {
                    rule: spec::ids::REF_COLLECTIONS,
                    name: name.to_owned(),
                    count: candidates.len(),
                },
                range,
            ));
            return None;
        }
        let node = candidates[0];
        steps.push(Step::Child {
            candidates,
            position: None,
        });
        Some(node)
    }

    // the same-kind children matching a name, in sibling order
    fn matching_children(&self, scope: NodeId, kind: BlockKind, name: &str) -> Vec<NodeId> {
        self.syms[scope.0 as usize]
            .children
            .iter()
            .copied()
            .filter(|&child| {
                let sym = &self.syms[child.0 as usize];
                sym.kind == kind && sym.name.as_deref() == Some(name)
            })
            .collect()
    }

    // narrows a candidate list by an optional position segment and records the
    // step; none = diagnosed, some(none) = target only known at generation
    fn consume_position(
        &mut self,
        segments: &mut std::iter::Peekable<std::vec::IntoIter<Segment>>,
        steps: &mut Vec<Step>,
        candidates: Vec<NodeId>,
        name: &str,
        range: SrcRange,
    ) -> Option<Option<NodeId>> {
        let position = match segments.peek() {
            Some(Segment::Index { .. }) => {
                let Some(Segment::Index { value, range: at }) = segments.next() else {
                    unreachable!("the peeked segment is an index");
                };
                Some((value, at))
            }
            _ => None,
        };

        match position {
            None if candidates.len() == 1 => {
                let node = candidates[0];
                steps.push(Step::Child {
                    candidates,
                    position: None,
                });
                Some(Some(node))
            }
            None => {
                self.errors.push(Error::new(
                    ErrorKind::AmbiguousBlockRef {
                        rule: spec::ids::REF_COLLECTIONS,
                        name: name.to_owned(),
                        count: candidates.len(),
                    },
                    range,
                ));
                None
            }
            Some((Value::Num(Number::Int(index)), at)) => {
                // a constant position picks its candidate during validation
                let position = usize::try_from(index).ok().filter(|&position| position < candidates.len());
                let Some(position) = position else {
                    self.errors.push(Error::new(
                        ErrorKind::IndexOutOfBounds {
                            rule: spec::ids::REF_COLLECTIONS,
                            index,
                            len: candidates.len(),
                        },
                        at,
                    ));
                    return None;
                };
                let node = candidates[position];
                steps.push(Step::Child {
                    candidates,
                    position: Some(Value::Num(Number::Int(index))),
                });
                Some(Some(node))
            }
            Some((value, _)) => {
                // a dynamic position resolves at generation
                steps.push(Step::Child {
                    candidates,
                    position: Some(value),
                });
                Some(None)
            }
        }
    }

    // reports every reference cycle the static graph shows; dynamic positions
    // stay off this graph and fail generation instead
    fn detect_cycles(&mut self) {
        let mut adjacency: HashMap<&SlotId, Vec<(&SlotId, SrcRange)>> = HashMap::new();
        for (from, to, at) in &self.edges {
            adjacency.entry(from).or_default().push((to, *at));
        }

        // 0 = unvisited, 1 = on the stack, 2 = done
        let mut state: HashMap<&SlotId, u8> = HashMap::new();
        let mut errors = Vec::new();

        for start in adjacency.keys() {
            if state.get(start).copied().unwrap_or(0) != 0 {
                continue;
            }
            // iterative dfs: (node, next edge index) with an explicit path
            let mut stack: Vec<(&SlotId, usize)> = vec![(start, 0)];
            let mut path: Vec<&SlotId> = vec![start];
            state.insert(start, 1);

            while let Some((node, edge)) = stack.last_mut() {
                let next = adjacency.get(*node).and_then(|edges| edges.get(*edge).copied());
                *edge += 1;
                match next {
                    Some((to, at)) => match state.get(to).copied().unwrap_or(0) {
                        0 => {
                            state.insert(to, 1);
                            stack.push((to, 0));
                            path.push(to);
                        }
                        1 => {
                            // a back edge closes a cycle through the path suffix
                            let from = path.iter().position(|&slot| slot == to).unwrap_or(0);
                            let mut chain: Vec<String> = path[from..].iter().map(|slot| self.slot_label(slot)).collect();
                            chain.push(self.slot_label(to));
                            errors.push(Error::new(
                                ErrorKind::CircularReference {
                                    rule: spec::ids::ACYCLIC_REFS,
                                    chain: chain.join(" -> "),
                                },
                                at,
                            ));
                        }
                        _ => {}
                    },
                    None => {
                        state.insert(node, 2);
                        stack.pop();
                        path.pop();
                    }
                }
            }
        }

        self.errors.extend(errors);
    }

    fn sym_label(&self, node: NodeId) -> String {
        let sym = &self.syms[node.0 as usize];
        match &sym.name {
            Some(name) => format!("{} \"{}\"", sym.kind.keyword(), name),
            None => sym.kind.keyword().to_owned(),
        }
    }

    fn slot_label(&self, slot: &SlotId) -> String {
        match slot {
            SlotId::Field { node, field, key } => {
                let mut label = format!("{} {}", self.sym_label(*node), field_keyword(*field));
                if let Some(key) = key {
                    label.push('.');
                    label.push_str(key);
                }
                label
            }
            SlotId::Binding { name, .. } => format!("var.{name}"),
        }
    }
}

// marks the pending references the model actually stores: extension leaves
// dead intermediates behind (llm.chat inside llm.chat[0].output), and only a
// stored reference must resolve
fn live_refs(pending: &[PendingRef], traces: &[Trace], bindings: &[Binding]) -> Vec<bool> {
    let mut live = vec![false; pending.len()];
    let mut found = Vec::new();

    for binding in bindings {
        collect_refs(&binding.value, &mut found);
    }
    for trace in traces {
        collect_trace_refs(trace, &mut found);
    }

    // segment expressions can hold nested references of their own
    while let Some(RefId(id)) = found.pop() {
        let id = id as usize;
        if std::mem::replace(&mut live[id], true) {
            continue;
        }
        for segment in &pending[id].segments {
            match segment {
                Segment::Name { .. } => {}
                Segment::Index { value, .. } => collect_refs(value, &mut found),
                Segment::Slice { start, end, .. } => {
                    if let Some(start) = start {
                        collect_refs(start, &mut found);
                    }
                    if let Some(end) = end {
                        collect_refs(end, &mut found);
                    }
                }
            }
        }
    }

    live
}

fn collect_trace_refs(trace: &Trace, found: &mut Vec<RefId>) {
    for binding in &trace.bindings {
        collect_refs(&binding.value, found);
    }
    collect_field_refs(&trace.fields, found);
    for child in &trace.children {
        collect_child_refs(child, found);
    }
}

fn collect_child_refs(child: &Child, found: &mut Vec<RefId>) {
    let (bindings, children) = match child {
        Child::Span(span) => {
            collect_field_refs(&span.fields, found);
            (&span.bindings, &span.children)
        }
        Child::Repeat(repeat) => {
            collect_refs(&repeat.count, found);
            (&repeat.bindings, &repeat.children)
        }
        Child::Choice(choice) => (&choice.bindings, &choice.children),
        Child::Maybe(maybe) => {
            collect_refs(&maybe.chance, found);
            (&maybe.bindings, &maybe.children)
        }
    };
    for binding in bindings {
        collect_refs(&binding.value, found);
    }
    for child in children {
        collect_child_refs(child, found);
    }
}

fn collect_field_refs(fields: &SpanFields, found: &mut Vec<RefId>) {
    for value in [&fields.input, &fields.output, &fields.expected, &fields.error].into_iter().flatten() {
        collect_refs(value, found);
    }
    for object in [&fields.metadata, &fields.metrics].into_iter().flatten() {
        for field in &object.elem {
            collect_refs(&field.value, found);
        }
    }
    for tag in &fields.tags {
        collect_template_refs(tag, found);
    }
}

fn collect_template_refs(template: &Template, found: &mut Vec<RefId>) {
    for part in &template.parts {
        if let Part::Dynamic(value) = part {
            collect_refs(value, found);
        }
    }
}

fn collect_refs(value: &Value, found: &mut Vec<RefId>) {
    match value {
        Value::BlockRef { ref_id, .. } => found.push(*ref_id),
        Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null | Value::VarRef(_) | Value::CtxRef(_) => {}
        Value::Template(template) => collect_template_refs(template, found),
        Value::Array(array) => {
            for elem in &array.elem {
                match elem {
                    ArrayElem::Item(value) | ArrayElem::Spread(value) => collect_refs(value, found),
                }
            }
        }
        Value::Object(object) => {
            for field in &object.elem {
                collect_refs(&field.value, found);
            }
        }
        Value::Unary { operand, .. } => collect_refs(operand, found),
        Value::Binary { lhs, rhs, .. } => {
            collect_refs(lhs, found);
            collect_refs(rhs, found);
        }
        Value::Cond { cond, then, otherwise } => {
            collect_refs(cond, found);
            collect_refs(then, found);
            collect_refs(otherwise, found);
        }
        Value::Index { target, index, .. } => {
            collect_refs(target, found);
            collect_refs(index, found);
        }
        Value::Slice { target, start, end, .. } => {
            collect_refs(target, found);
            if let Some(start) = start {
                collect_refs(start, found);
            }
            if let Some(end) = end {
                collect_refs(end, found);
            }
        }
        Value::Func { func, .. } => collect_func_refs(func, found),
    }
}

fn collect_func_refs(func: &Func, found: &mut Vec<RefId>) {
    let mut all = |values: &[Value]| {
        for value in values {
            collect_refs(value, found);
        }
    };
    match func {
        Func::Choice(options) | Func::Min(options) | Func::Max(options) => all(options),
        Func::Weighted(options) => {
            for option in options {
                collect_refs(&option.value, found);
            }
        }
        Func::Range(_)
        | Func::Normal { .. }
        | Func::Lognormal { .. }
        | Func::Exponential { .. }
        | Func::Pareto { .. }
        | Func::Beta { .. }
        | Func::Poisson { .. }
        | Func::Chance { .. }
        | Func::Uuid
        | Func::Hex { .. }
        | Func::Alphanum { .. } => {}
        Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => collect_refs(text, found),
        Func::Replace { text, from, to } => {
            collect_refs(text, found);
            collect_refs(from, found);
            collect_refs(to, found);
        }
        Func::Split { text, separator } => {
            collect_refs(text, found);
            collect_refs(separator, found);
        }
        Func::Join { array, separator } => {
            collect_refs(array, found);
            collect_refs(separator, found);
        }
        Func::Contains { target, needle } => {
            collect_refs(target, found);
            collect_refs(needle, found);
        }
        Func::StartsWith { text, prefix } => {
            collect_refs(text, found);
            collect_refs(prefix, found);
        }
        Func::EndsWith { text, suffix } => {
            collect_refs(text, found);
            collect_refs(suffix, found);
        }
        Func::Len { target } => collect_refs(target, found),
        Func::Tokens { value } => collect_refs(value, found),
        Func::Format { args, .. } => all(args),
        Func::Clamp { value, min, max } => {
            collect_refs(value, found);
            collect_refs(min, found);
            collect_refs(max, found);
        }
        Func::Round { value } | Func::Floor { value } | Func::Ceil { value } | Func::Abs { value } => {
            collect_refs(value, found)
        }
    }
}

// the referenceable field behind a span field id
fn model_field_kind(field: spec::Id) -> Option<Field> {
    if field == spec::ids::INPUT {
        Some(Field::Input)
    } else if field == spec::ids::OUTPUT {
        Some(Field::Output)
    } else if field == spec::ids::EXPECTED {
        Some(Field::Expected)
    } else if field == spec::ids::ERROR {
        Some(Field::Error)
    } else if field == spec::ids::METADATA {
        Some(Field::Metadata)
    } else if field == spec::ids::METRICS {
        Some(Field::Metrics)
    } else if field == spec::ids::TAGS {
        Some(Field::Tags)
    } else {
        None
    }
}

// the field a reference segment names, shared with fixup resolution
fn field_by_name(name: &str) -> Option<Field> {
    match name {
        "input" => Some(Field::Input),
        "output" => Some(Field::Output),
        "expected" => Some(Field::Expected),
        "error" => Some(Field::Error),
        "metadata" => Some(Field::Metadata),
        "metrics" => Some(Field::Metrics),
        "tags" => Some(Field::Tags),
        _ => None,
    }
}

fn field_keyword(field: Field) -> &'static str {
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

// the kind keyword a reference segment names, for descending into children
fn child_kind_by_name(name: &str) -> Option<BlockKind> {
    match name {
        "task" => Some(BlockKind::Task),
        "llm" => Some(BlockKind::Llm),
        "tool" => Some(BlockKind::Tool),
        "function" => Some(BlockKind::Function),
        "repeat" => Some(BlockKind::Repeat),
        "choice" => Some(BlockKind::Choice),
        "maybe" => Some(BlockKind::Maybe),
        _ => None,
    }
}

// vars blocks bind to their enclosing block, everything else models in place
fn split_vars(decls: Vec<ast::Decl>) -> (Vec<ast::Block>, Vec<ast::Decl>) {
    let mut vars = Vec::new();
    let mut rest = Vec::with_capacity(decls.len());
    for decl in decls {
        match decl {
            ast::Decl::Block(block) if spec::SPEC.block(&block.kind).is_some_and(|desc| desc.id == spec::ids::VARS) => {
                vars.push(block);
            }
            decl => rest.push(decl),
        }
    }
    (vars, rest)
}

fn float_bound(number: Number) -> f64 {
    match number {
        Number::Int(value) => value as f64,
        Number::Float(value) => value,
    }
}

// statically known result type of an expr, none = unknown (eg heterogeneous choice)
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum StaticType {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
}

// constant scalar operand pulled out of a folded literal
enum Const {
    Str(String),
    Num(Number),
    Bool(bool),
}

enum OpClass {
    Arith,
    Cmp,
    Eq,
    Logic,
}

fn op_class(op: ast::BinOp) -> OpClass {
    match op {
        ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::Mul | ast::BinOp::Div | ast::BinOp::Rem => OpClass::Arith,
        ast::BinOp::Lt | ast::BinOp::Le | ast::BinOp::Gt | ast::BinOp::Ge => OpClass::Cmp,
        ast::BinOp::Eq | ast::BinOp::Ne => OpClass::Eq,
        ast::BinOp::And | ast::BinOp::Or => OpClass::Logic,
    }
}

// residual dynamic exprs cross into the model here, off the ast op enums
fn model_unary_op(op: ast::UnaryOp) -> UnaryOp {
    match op {
        ast::UnaryOp::Neg => UnaryOp::Neg,
        ast::UnaryOp::Not => UnaryOp::Not,
    }
}

fn model_bin_op(op: ast::BinOp) -> BinOp {
    match op {
        ast::BinOp::Add => BinOp::Add,
        ast::BinOp::Sub => BinOp::Sub,
        ast::BinOp::Mul => BinOp::Mul,
        ast::BinOp::Div => BinOp::Div,
        ast::BinOp::Rem => BinOp::Rem,
        ast::BinOp::Eq => BinOp::Eq,
        ast::BinOp::Ne => BinOp::Ne,
        ast::BinOp::Lt => BinOp::Lt,
        ast::BinOp::Le => BinOp::Le,
        ast::BinOp::Gt => BinOp::Gt,
        ast::BinOp::Ge => BinOp::Ge,
        ast::BinOp::And => BinOp::And,
        ast::BinOp::Or => BinOp::Or,
    }
}

// none when empty or the types disagree
fn unify_types(types: impl Iterator<Item = Option<StaticType>>) -> Option<StaticType> {
    let mut unified = None;
    for found in types {
        let found = found?;
        match unified {
            None => unified = Some(found),
            Some(previous) if previous == found => {}
            Some(_) => return None,
        }
    }
    unified
}

fn type_name(required: StaticType) -> &'static str {
    match required {
        StaticType::String => "string",
        StaticType::Number => "number",
        StaticType::Boolean => "boolean",
        StaticType::Null => "null",
        StaticType::Array => "array",
        StaticType::Object => "object",
    }
}

// no funcs or per-instantiation refs anywhere beneath; all-literal templates
// already folded to strings, so a surviving template is dynamic
fn is_constant(folded: &Folded) -> bool {
    match &folded.kind {
        FoldedKind::Value(value) => value_is_constant(value),
        FoldedKind::Array(values) => values.iter().all(is_constant),
        FoldedKind::Object(fields) => fields.iter().all(|field| is_constant(&field.value)),
    }
}

fn value_is_constant(value: &Value) -> bool {
    match value {
        Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null => true,
        Value::Array(array) => array.elem.iter().all(|elem| match elem {
            ArrayElem::Item(value) => value_is_constant(value),
            // a surviving spread only exists because its shape is dynamic
            ArrayElem::Spread(_) => false,
        }),
        Value::Object(object) => object.elem.iter().all(|field| value_is_constant(&field.value)),
        Value::Template(_) | Value::Func { .. } | Value::VarRef(_) | Value::CtxRef(_) | Value::BlockRef { .. } => false,
        Value::Unary { .. } | Value::Binary { .. } | Value::Cond { .. } | Value::Index { .. } | Value::Slice { .. } => false,
    }
}

// constant scalar operand pulled out of a folded value
fn const_scalar(folded: &Folded) -> Const {
    match &folded.kind {
        FoldedKind::Value(Value::Str(value)) => Const::Str(value.clone()),
        FoldedKind::Value(Value::Num(number)) => Const::Num(number.clone()),
        FoldedKind::Value(Value::Bool(value)) => Const::Bool(*value),
        _ => unreachable!("constant operands are scalar literals"),
    }
}

// constant string value of a folded expr, none when dynamic or another type
fn const_string(folded: &Folded) -> Option<String> {
    match &folded.kind {
        FoldedKind::Value(Value::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

// interpolated numbers render like generation-time templates, {:?} keeps a .0
fn num_text(number: Number) -> String {
    match number {
        Number::Int(value) => value.to_string(),
        Number::Float(value) => format!("{value:?}"),
    }
}

// metadata and metrics were validated as objects, duplicate keys already merged
fn require_object(folded: Folded) -> Object {
    match folded.into_value() {
        Value::Object(object) => object,
        _ => unreachable!("expression was validated as an object"),
    }
}

// tag elements were validated as strings, templates already modeled
fn require_tags(folded: Folded) -> Vec<Template> {
    let FoldedKind::Array(values) = folded.kind else {
        unreachable!("expression was validated as an array of strings");
    };

    values
        .into_iter()
        .map(|value| match value.into_value() {
            Value::Str(value) => Template {
                parts: vec![Part::Lit(value)],
            },
            Value::Template(template) => template,
            _ => unreachable!("array item was validated as a string"),
        })
        .collect()
}

// none = overflow or a non-finite float result, zero divisors are caught before eval
fn eval_arith(op: ast::BinOp, lhs: Number, rhs: Number) -> Option<Number> {
    match (lhs, rhs) {
        (Number::Int(lhs), Number::Int(rhs)) => {
            let result = match op {
                ast::BinOp::Add => lhs.checked_add(rhs),
                ast::BinOp::Sub => lhs.checked_sub(rhs),
                ast::BinOp::Mul => lhs.checked_mul(rhs),
                // checked catches i64::MIN / -1
                ast::BinOp::Div => lhs.checked_div(rhs),
                ast::BinOp::Rem => lhs.checked_rem(rhs),
                _ => unreachable!("operator is arithmetic"),
            };
            result.map(Number::Int)
        }
        (lhs, rhs) => {
            let (lhs, rhs) = (float_bound(lhs), float_bound(rhs));
            let result = match op {
                ast::BinOp::Add => lhs + rhs,
                ast::BinOp::Sub => lhs - rhs,
                ast::BinOp::Mul => lhs * rhs,
                ast::BinOp::Div => lhs / rhs,
                ast::BinOp::Rem => lhs % rhs,
                _ => unreachable!("operator is arithmetic"),
            };
            result.is_finite().then_some(Number::Float(result))
        }
    }
}

fn eval_cmp(op: ast::BinOp, lhs: Number, rhs: Number) -> bool {
    fn compare<T: PartialOrd>(op: ast::BinOp, lhs: T, rhs: T) -> bool {
        match op {
            ast::BinOp::Lt => lhs < rhs,
            ast::BinOp::Le => lhs <= rhs,
            ast::BinOp::Gt => lhs > rhs,
            ast::BinOp::Ge => lhs >= rhs,
            _ => unreachable!("operator is a comparison"),
        }
    }

    match (lhs, rhs) {
        (Number::Int(lhs), Number::Int(rhs)) => compare(op, lhs, rhs),
        (lhs, rhs) => compare(op, float_bound(lhs), float_bound(rhs)),
    }
}

fn model_ctx_ref(path: &[String], repeat_depth: usize) -> Option<CtxRef> {
    match path {
        [first, second] if first == "trace" && second == "index" => Some(CtxRef::TraceIndex),
        [first, second] if first == "repeat" && second == "index" && repeat_depth > 0 => Some(CtxRef::RepeatIndex),
        [first, second] if first == "repeat" && second == "count" && repeat_depth > 0 => Some(CtxRef::RepeatCount),
        _ => None,
    }
}

pub(super) fn model(ast: ast::Ast) -> Result<Model, Diags> {
    Modeler::new(ast)
        .model()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Default)]
struct FieldsBuilder {
    input: Option<Value>,
    output: Option<Value>,
    expected: Option<Value>,
    error: Option<Value>,
    metadata: Option<Object>,
    metrics: Option<Object>,
    tags: Option<Vec<Template>>,
    seen: HashSet<spec::Id>,
}

impl FieldsBuilder {
    fn finish(self) -> SpanFields {
        SpanFields {
            input: self.input,
            output: self.output,
            expected: self.expected,
            error: self.error,
            metadata: self.metadata,
            metrics: self.metrics,
            tags: self.tags.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum ErrorKind {
    RootAttribute {
        keyword: String,
    },
    UnknownBlock {
        keyword: String,
        parent: spec::Place,
    },
    BlockNotAllowed {
        block: spec::Id,
        parent: spec::Place,
    },
    MissingName {
        block: spec::Id,
    },
    UnexpectedName {
        block: spec::Id,
    },
    UnknownField {
        block: spec::Id,
        keyword: String,
    },
    MissingField {
        block: spec::Id,
        field: spec::Id,
    },
    DuplicateField {
        block: spec::Id,
        field: spec::Id,
    },
    TypeMismatch {
        block: spec::Id,
        field: spec::Id,
        expected: &'static spec::ExprType,
        found: ExprType,
    },
    DuplicateObjectKey {
        rule: spec::Id,
        key: String,
    },
    InvalidNumber {
        rule: spec::Id,
        raw: String,
    },
    ReservedMetricKey {
        rule: spec::Id,
        key: String,
    },
    UnknownReference {
        rule: spec::Id,
        path: String,
    },
    DuplicateVar {
        rule: spec::Id,
        name: String,
    },
    VarNotInScope {
        rule: spec::Id,
        name: String,
    },
    UnknownVariable {
        rule: spec::Id,
        name: String,
    },
    NonScalarInterpolation {
        rule: spec::Id,
        found: ExprType,
    },
    UnknownBlockRef {
        rule: spec::Id,
        keyword: &'static str,
        name: String,
    },
    AmbiguousBlockRef {
        rule: spec::Id,
        name: String,
        count: usize,
    },
    IncompleteBlockRef {
        rule: spec::Id,
        keyword: &'static str,
    },
    AbsentFieldRef {
        rule: spec::Id,
        field: String,
        label: String,
    },
    InvalidRefSegment {
        rule: spec::Id,
        segment: String,
    },
    DynamicPositionDescent {
        rule: spec::Id,
    },
    RepeatIterationRequired {
        rule: spec::Id,
    },
    ReservedRepeatName {
        rule: spec::Id,
        name: String,
    },
    CircularReference {
        rule: spec::Id,
        chain: String,
    },
    SelfOutsideBlock {
        rule: spec::Id,
    },
    RootVarBlockRef {
        rule: spec::Id,
        name: String,
    },
    StructuralBlockRef {
        rule: spec::Id,
        field: &'static str,
    },
    ForBlockRefCollection {
        rule: spec::Id,
    },
    EmptyShape {
        rule: spec::Id,
    },
    UnknownFunction {
        rule: spec::Id,
        name: String,
    },
    EmptyChoice {
        rule: spec::Id,
    },
    InvalidRangeArgs {
        rule: spec::Id,
    },
    InvalidRangeBounds {
        rule: spec::Id,
    },
    FuncArity {
        rule: spec::Id,
        func: &'static str,
        expected: &'static str,
    },
    FuncArgType {
        rule: spec::Id,
        func: &'static str,
        expected: &'static str,
        found: ExprType,
    },
    NonConstantParam {
        rule: spec::Id,
        func: &'static str,
        param: &'static str,
    },
    ParamOutOfRange {
        rule: spec::Id,
        func: &'static str,
        param: &'static str,
        expected: &'static str,
    },
    WeightedOptionShape {
        rule: spec::Id,
    },
    WeightedTotal {
        rule: spec::Id,
    },
    FormatPlaceholders {
        rule: spec::Id,
        placeholders: usize,
        args: usize,
    },
    EmptySplitSeparator {
        rule: spec::Id,
    },
    ClampBoundsOutOfOrder {
        rule: spec::Id,
    },
    OperandTypeMismatch {
        rule: spec::Id,
        op: String,
        expected: &'static str,
        found: ExprType,
    },
    NonBooleanCondition {
        rule: spec::Id,
        found: ExprType,
    },
    NonIndexableTarget {
        rule: spec::Id,
        found: ExprType,
    },
    IndexTypeMismatch {
        rule: spec::Id,
        expected: &'static str,
        found: ExprType,
    },
    NonIntegerIndex {
        rule: spec::Id,
    },
    IndexOutOfBounds {
        rule: spec::Id,
        index: i64,
        len: usize,
    },
    UnknownObjectKey {
        rule: spec::Id,
        key: String,
    },
    DivisionByZero {
        rule: spec::Id,
        op: String,
    },
    NonFiniteResult {
        rule: spec::Id,
    },
    NonSliceableTarget {
        rule: spec::Id,
        found: ExprType,
    },
    SliceTypeMismatch {
        rule: spec::Id,
        found: ExprType,
    },
    NonIntegerSliceBound {
        rule: spec::Id,
    },
    NegativeSliceBound {
        rule: spec::Id,
    },
    SpreadTypeMismatch {
        rule: spec::Id,
        expected: &'static str,
        found: ExprType,
    },
    ForCollectionMismatch {
        rule: spec::Id,
        found: ExprType,
    },
    NonConstantForFilter {
        rule: spec::Id,
        found: ExprType,
    },
    NonConstantForKey {
        rule: spec::Id,
        found: ExprType,
    },
    EmptyDynamicBlock {
        rule: spec::Id,
        block: spec::Id,
    },
    NegativeRepeatCount {
        rule: spec::Id,
    },
    NonIntegerRepeatCount {
        rule: spec::Id,
    },
    ChanceOutOfRange {
        rule: spec::Id,
    },
    RepeatRefOutsideRepeat {
        rule: spec::Id,
        path: String,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootAttribute { keyword } => {
                write!(
                    formatter,
                    "attribute `{keyword}` is not allowed at the root; allowed blocks: "
                )?;
                fmt_allowed_blocks(formatter, spec::Place::Root)
            }
            Self::UnknownBlock { keyword, parent } => {
                write!(formatter, "unknown block `{keyword}` ")?;
                fmt_place(formatter, *parent)?;
                formatter.write_str("; allowed blocks here: ")?;
                fmt_allowed_blocks(formatter, *parent)
            }
            Self::BlockNotAllowed { block, parent } => {
                let block = block_desc(*block);
                write!(formatter, "block `{}` is not allowed ", block.keyword)?;
                fmt_place(formatter, *parent)?;
                formatter.write_str("; allowed placements: ")?;
                fmt_places(formatter, block.allowed_in)
            }
            Self::MissingName { block } => {
                let block = block_desc(*block);
                write!(
                    formatter,
                    "block `{}` requires a name; expected `{}`",
                    block.keyword, block.syntax
                )
            }
            Self::UnexpectedName { block } => {
                let block = block_desc(*block);
                write!(formatter, "block `{}` does not accept a name", block.keyword)
            }
            Self::UnknownField { block, keyword } => {
                let block = block_desc(*block);
                write!(formatter, "unknown field `{keyword}` in block `{}`", block.keyword)?;
                if block.body.fields.is_empty() {
                    formatter.write_str("; this block does not accept fields")
                } else {
                    formatter.write_str("; expected one of: ")?;
                    for (index, field) in block.body.fields.iter().enumerate() {
                        if index > 0 {
                            formatter.write_str(", ")?;
                        }
                        write!(formatter, "`{}`", field.keyword)?;
                    }
                    Ok(())
                }
            }
            Self::MissingField { block, field } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(formatter, "block `{}` requires field `{}`", block.keyword, field.keyword)
            }
            Self::DuplicateField { block, field } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(
                    formatter,
                    "field `{}` may appear only once in block `{}`",
                    field.keyword, block.keyword
                )
            }
            Self::TypeMismatch {
                block,
                field,
                expected,
                found,
            } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(
                    formatter,
                    "field `{}` in block `{}` expects {expected}, but found {found}",
                    field.keyword, block.keyword
                )
            }
            Self::DuplicateObjectKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "duplicate object key `{key}`; {}", rule.summary)
            }
            Self::InvalidNumber { rule, raw } => {
                let rule = rule_desc(*rule);
                write!(formatter, "invalid number `{raw}`; {}", rule.summary)
            }
            Self::ReservedMetricKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "metric key `{key}` is reserved; {}", rule.summary)
            }
            Self::UnknownReference { rule, path } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown reference `{path}`; {}", rule.summary)
            }
            Self::DuplicateVar { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "variable `{name}` is defined more than once; {}", rule.summary)
            }
            Self::VarNotInScope { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "variable `{name}` is not in scope here; {}", rule.summary)
            }
            Self::UnknownVariable { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown variable `{name}`; {}", rule.summary)
            }
            Self::NonScalarInterpolation { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot interpolate {found} here; {}", rule.summary)
            }
            Self::UnknownBlockRef { rule, keyword, name } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "no {keyword} block named \"{name}\" is in scope; {}",
                    rule.summary
                )
            }
            Self::AmbiguousBlockRef { rule, name, count } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "{count} sibling blocks are named \"{name}\", index the one you mean; {}",
                    rule.summary
                )
            }
            Self::IncompleteBlockRef { rule, keyword } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "`{keyword}` reference does not reach a field; {}",
                    rule.summary
                )
            }
            Self::AbsentFieldRef { rule, field, label } => {
                let rule = rule_desc(*rule);
                write!(formatter, "{label} never sets `{field}`; {}", rule.summary)
            }
            Self::InvalidRefSegment { rule, segment } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "`{segment}` is neither a field nor a child block kind; {}",
                    rule.summary
                )
            }
            Self::DynamicPositionDescent { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "cannot descend past a dynamic position, only the final block may be picked at generation; {}",
                    rule.summary
                )
            }
            Self::RepeatIterationRequired { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "select an iteration with `[...]` before descending into a repeat; {}",
                    rule.summary
                )
            }
            Self::ReservedRepeatName { rule, name } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "`{name}` cannot name a repeat block; {}",
                    rule.summary
                )
            }
            Self::CircularReference { rule, chain } => {
                let rule = rule_desc(*rule);
                write!(formatter, "reference cycle: {chain}; {}", rule.summary)
            }
            Self::SelfOutsideBlock { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "`self` has no enclosing span or trace here; {}", rule.summary)
            }
            Self::RootVarBlockRef { rule, name } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "root var `{name}` cannot hold a block reference; {}",
                    rule.summary
                )
            }
            Self::StructuralBlockRef { rule, field } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "`{field}` cannot depend on a block reference; {}",
                    rule.summary
                )
            }
            Self::ForBlockRefCollection { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "a `for` collection cannot come from a block reference; {}",
                    rule.summary
                )
            }
            Self::EmptyShape { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "shape declares no traces; {}", rule.summary)
            }
            Self::UnknownFunction { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown function `{name}`; {}", rule.summary)
            }
            Self::EmptyChoice { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `choice` has no alternatives; {}", rule.summary)
            }
            Self::InvalidRangeArgs { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `range` expects two number arguments; {}", rule.summary)
            }
            Self::InvalidRangeBounds { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "range bounds are out of order; {}", rule.summary)
            }
            Self::FuncArity { rule, func, expected } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `{func}` expects {expected}; {}", rule.summary)
            }
            Self::FuncArgType {
                rule,
                func,
                expected,
                found,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "function `{func}` expects a {expected} argument, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantParam { rule, func, param } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "function `{func}` {param} must be a constant number; {}",
                    rule.summary
                )
            }
            Self::ParamOutOfRange {
                rule,
                func,
                param,
                expected,
            } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `{func}` {param} must be {expected}; {}", rule.summary)
            }
            Self::WeightedOptionShape { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "weighted option must be a `[value, weight]` pair with a constant non-negative weight; {}",
                    rule.summary
                )
            }
            Self::WeightedTotal { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "weighted options have no positive weight; {}", rule.summary)
            }
            Self::FormatPlaceholders {
                rule,
                placeholders,
                args,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "format template has {placeholders} `{{}}` placeholders but {args} arguments; {}",
                    rule.summary
                )
            }
            Self::EmptySplitSeparator { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "split separator is empty; {}", rule.summary)
            }
            Self::ClampBoundsOutOfOrder { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "clamp bounds are out of order; {}", rule.summary)
            }
            Self::OperandTypeMismatch {
                rule,
                op,
                expected,
                found,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "operator `{op}` expects {expected} operands, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonBooleanCondition { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "conditional expects a boolean condition, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonIndexableTarget { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot index into {found}; {}", rule.summary)
            }
            Self::IndexTypeMismatch { rule, expected, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "index expects {expected}, but found {found}; {}", rule.summary)
            }
            Self::NonIntegerIndex { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "array index must be an integer; {}", rule.summary)
            }
            Self::IndexOutOfBounds { rule, index, len } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "index {index} is out of bounds for an array of {len} elements; {}",
                    rule.summary
                )
            }
            Self::UnknownObjectKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "object has no key `{key}`; {}", rule.summary)
            }
            Self::DivisionByZero { rule, op } => {
                let rule = rule_desc(*rule);
                write!(formatter, "operator `{op}` divides by a constant zero; {}", rule.summary)
            }
            Self::NonFiniteResult { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "expression result is not a finite number; {}", rule.summary)
            }
            Self::NonSliceableTarget { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot slice {found}; {}", rule.summary)
            }
            Self::SliceTypeMismatch { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound expects a number, but found {found}; {}", rule.summary)
            }
            Self::NonIntegerSliceBound { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound must be an integer; {}", rule.summary)
            }
            Self::NegativeSliceBound { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound must be non-negative; {}", rule.summary)
            }
            Self::SpreadTypeMismatch { rule, expected, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot spread {found} into an {expected}; {}", rule.summary)
            }
            Self::ForCollectionMismatch { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for expression expects an array or object collection with a shape known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForFilter { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for filter expects a boolean known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForKey { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for key expects a string known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::EmptyDynamicBlock { rule, block } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "block `{}` has no child blocks; {}",
                    block_desc(*block).keyword,
                    rule.summary
                )
            }
            Self::NegativeRepeatCount { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "repeat count is negative; {}", rule.summary)
            }
            Self::NonIntegerRepeatCount { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "repeat count is not an integer; {}", rule.summary)
            }
            Self::ChanceOutOfRange { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "maybe chance is not between 0 and 1; {}", rule.summary)
            }
            Self::RepeatRefOutsideRepeat { rule, path } => {
                let rule = rule_desc(*rule);
                write!(formatter, "`{path}` is not inside a repeat block; {}", rule.summary)
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

fn block_desc(id: spec::Id) -> &'static spec::BlockDesc {
    spec::SPEC
        .block_by_id(id)
        .unwrap_or_else(|| panic!("error references unknown block {}", id.as_str()))
}

fn field_desc(block: spec::Id, field: spec::Id) -> &'static spec::FieldDesc {
    spec::SPEC.field(block, field).unwrap_or_else(|| {
        panic!(
            "error references unknown field {} in block {}",
            field.as_str(),
            block.as_str()
        )
    })
}

fn rule_desc(id: spec::Id) -> &'static spec::RuleDesc {
    spec::SPEC
        .rule(id)
        .unwrap_or_else(|| panic!("error references unknown rule {}", id.as_str()))
}

fn fmt_place(formatter: &mut fmt::Formatter<'_>, place: spec::Place) -> fmt::Result {
    match place {
        spec::Place::Root => formatter.write_str("at the root"),
        spec::Place::Block { id } => write!(formatter, "inside block `{}`", block_desc(id).keyword),
    }
}

fn fmt_places(formatter: &mut fmt::Formatter<'_>, places: &[spec::Place]) -> fmt::Result {
    for (index, place) in places.iter().enumerate() {
        if index > 0 {
            formatter.write_str(", ")?;
        }
        fmt_place(formatter, *place)?;
    }
    Ok(())
}

fn fmt_allowed_blocks(formatter: &mut fmt::Formatter<'_>, place: spec::Place) -> fmt::Result {
    let mut count = 0;
    for block in spec::SPEC.blocks.iter().filter(|block| block.allows(place)) {
        if count > 0 {
            formatter.write_str(", ")?;
        }
        write!(formatter, "`{}`", block.keyword)?;
        count += 1;
    }

    if count == 0 {
        formatter.write_str("none")?;
    }

    Ok(())
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
    #[cfg(test)]
    fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    #[cfg(test)]
    fn range(&self) -> SrcRange {
        self.range
    }
}

impl From<Error> for Diag {
    fn from(error: Error) -> Self {
        let Error { kind, range } = error;

        Diag {
            when: DiagPhase::Validation,
            what: kind.to_string(),
            r#where: range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(source: &str) -> Result<Model, Errors> {
        let tokens = crate::dsl::lexer::lex(source).unwrap();
        let ast = crate::dsl::parser::parse(tokens, source).unwrap();
        Modeler::new(ast).model()
    }

    fn tag_text(tag: &Template) -> &str {
        match tag.parts.as_slice() {
            [Part::Lit(value)] => value,
            _ => panic!("expected a literal tag"),
        }
    }

    fn as_span(child: &Child) -> &Span {
        match child {
            Child::Span(span) => span,
            other => panic!("expected a span child, found {other:?}"),
        }
    }

    #[test]
    fn models_fixture_as_typed_domain() {
        let model = model(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(model.traces.len(), 1);
        let trace = &model.traces[0];
        assert_eq!(trace.name, "support-sessions");
        assert_eq!(trace.children.len(), 2);
        assert!(
            trace
                .children
                .iter()
                .all(|child| matches!(&as_span(child).kind, SpanKind::Task))
        );
        assert!(trace.children.iter().all(|child| matches!(
            as_span(child).children.as_slice(),
            [Child::Span(Span { kind: SpanKind::Llm, .. })]
        )));
        assert_eq!(trace.fields.tags.iter().map(tag_text).collect::<Vec<_>>(), ["chat", "prod"]);
    }

    #[test]
    fn models_tool_and_function_spans() {
        let source = r#"
            trace "example" {
                function "handle_request" {
                    tool "search" {
                        llm "rerank" {}
                    }
                }
            }
        "#;
        let model = model(source).unwrap();

        let function = as_span(&model.traces[0].children[0]);
        assert!(matches!(function.kind, SpanKind::Function));
        let tool = as_span(&function.children[0]);
        assert!(matches!(tool.kind, SpanKind::Tool));
        assert!(matches!(as_span(&tool.children[0]).kind, SpanKind::Llm));
    }

    #[test]
    fn models_expected_and_error_fields() {
        let source = r#"
            trace "example" {
                llm "answer" {
                    expected = { answer = "4" }
                    error = choice(null, "timeout")
                }
            }
        "#;
        let model = model(source).unwrap();

        let fields = &as_span(&model.traces[0].children[0]).fields;
        assert!(matches!(&fields.expected, Some(Value::Object(object)) if object.elem[0].key == "answer"));
        assert!(matches!(&fields.error, Some(Value::Func { func: Func::Choice(options), .. }) if options.len() == 2));
    }

    #[test]
    fn rejects_invalid_domain_fields() {
        let source = r#"
            trace "example" {
                input = "first"
                input = "second"
                metadata = true
                tags = ["valid", false ]
            }
            "#;
        let result = model(source);
        let errors = match result {
            Err(errors) => errors,
            Ok(_) => panic!("expected semantic errors"),
        };

        let trace = spec::SPEC.block_by_id(spec::ids::TRACE).unwrap();
        let metadata = trace.field("metadata").unwrap();
        let tags = trace.field("tags").unwrap();
        let spec::ExprType::Array { items: tag_item } = *tags.value else {
            panic!("tags must be described as an array");
        };

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateField {
                block: spec::ids::TRACE,
                field: spec::ids::INPUT,
            }
        );
        assert_eq!(
            errors[1].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::METADATA,
                expected: metadata.value,
                found: ExprType::Boolean,
            }
        );
        assert_eq!(
            errors[2].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::TAGS,
                expected: tag_item,
                found: ExprType::Boolean,
            }
        );

        let range = errors[1].range();
        assert_eq!(&source[range.start..range.end], "true");
    }

    #[test]
    fn models_null_and_negative_numbers() {
        let model = model(r#"trace "example" { input = null metrics = { delta = -0.5, offset = -3 } }"#).unwrap();

        let fields = &model.traces[0].fields;
        assert!(matches!(fields.input, Some(Value::Null)));

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == -0.5));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(-3))));
    }

    #[test]
    fn models_templates_with_known_references() {
        let model = model(r#"trace "example" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Template(template)) = &fields.input else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::TraceIndex)));

        assert_eq!(fields.tags.len(), 1);
        assert!(matches!(fields.tags[0].parts[1], Part::Ref(CtxRef::TraceIndex)));
    }

    #[test]
    fn rejects_unknown_references() {
        // a trace head with an unknown segment is a reference to nothing
        let source = r#"trace "example" { input = "${trace.idx}" }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::InvalidRefSegment {
                rule: spec::ids::REF_FIELDS,
                segment: "idx".to_owned(),
            }
        );
        assert!(errors[0].to_string().contains("neither a field nor a child block kind"));

        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "trace.idx");
    }

    #[test]
    fn rejects_bare_unknown_references() {
        let errors = model(r#"trace "t" { input = nil }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownReference {
                rule: spec::ids::KNOWN_REFERENCES,
                path: "nil".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_loop_references_outside_their_scope() {
        // x is out of scope after the inner for expr
        let errors = model(r#"trace "t" { input = [[for x in [1] : x], x] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownReference {
                rule: spec::ids::KNOWN_REFERENCES,
                path: "x".to_owned(),
            }
        );
    }

    #[test]
    fn scopes_loop_bindings_over_context_references() {
        // the binding shadows the trace namespace, trailing segments select into the element
        let model = model(r#"trace "t" { input = [for trace in [{ index = 9 }] : trace.index] }"#).unwrap();
        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [9]);
    }

    #[test]
    fn selects_into_variables_by_path() {
        let model = model(r#"vars { m = { a = { b = 4 } } } trace "t" { input = var.m.a.b }"#).unwrap();
        assert!(matches!(
            model.traces[0].fields.input,
            Some(Value::Num(Number::Int(4)))
        ));
    }

    #[test]
    fn treats_templates_as_strings_in_type_checks() {
        let errors = model(r#"trace "example" { metadata = "${trace.index}" }"#).unwrap_err();

        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::String,
                ..
            }
        ));
    }

    #[test]
    fn resolves_variables_in_scalar_object_and_array_positions() {
        let model = model(
            r#"
            vars {
                greeting = "hey"
                meta = { model = "gpt" }
                n = 4
            }
            trace "example" {
                input = var.greeting
                output = [var.n, var.n]
                metadata = var.meta
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(&fields.input, Some(Value::Str(value)) if value == "hey"));

        let Some(Value::Array(output)) = &fields.output else {
            panic!("expected an array");
        };
        assert!(matches!(output.elem[1], ArrayElem::Item(Value::Num(Number::Int(4)))));

        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem[0].key, "model");
        assert!(matches!(&metadata.elem[0].value, Value::Str(value) if value == "gpt"));
    }

    #[test]
    fn interpolates_scalar_variables_and_collapses_static_templates() {
        let model = model(
            r#"
            vars { m = "gpt" t = 0.2 f = false }
            trace "example" { input = "m=${var.m} t=${var.t} f=${var.f}" }
            "#,
        )
        .unwrap();

        assert!(matches!(
            &model.traces[0].fields.input,
            Some(Value::Str(value)) if value == "m=gpt t=0.2 f=false"
        ));
    }

    #[test]
    fn lowers_template_variables_to_bindings_keeping_context_references() {
        let model = model(
            r#"
            vars { q = "q ${trace.index}" }
            trace "example" { input = "${var.q}!" }
            "#,
        )
        .unwrap();

        // the ctx ref makes the var dynamic: the definition becomes a root
        // binding and the use site references it
        assert_eq!(model.bindings.len(), 1);
        assert_eq!(model.bindings[0].name, "q");
        let Value::Template(definition) = &model.bindings[0].value else {
            panic!("expected a template binding");
        };
        assert!(matches!(&definition.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(definition.parts[1], Part::Ref(CtxRef::TraceIndex)));

        let Some(Value::Template(template)) = &model.traces[0].fields.input else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::VarRef(name) if name == "q"));
        assert!(matches!(&template.parts[1], Part::Lit(value) if value == "!"));
    }

    #[test]
    fn resolves_variables_defined_after_their_use() {
        let model = model(
            r#"
            trace "example" { input = var.greeting }
            vars { greeting = "hey" }
            "#,
        )
        .unwrap();

        assert!(matches!(&model.traces[0].fields.input, Some(Value::Str(value)) if value == "hey"));
    }

    #[test]
    fn reports_variable_type_mismatches_at_the_use_site() {
        let source = r#"vars { m = true } trace "example" { metadata = var.m }"#;
        let errors = model(source).unwrap_err();

        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Boolean,
                ..
            }
        ));
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "var.m");
    }

    #[test]
    fn rejects_unknown_variables() {
        for source in [
            r#"trace "example" { input = var.missing }"#,
            r#"trace "example" { input = "${var.missing}" }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::UnknownVariable {
                    rule: spec::ids::DEFINED_VARS,
                    name: "missing".to_owned(),
                }
            );
        }
    }

    #[test]
    fn rejects_duplicate_variables_across_vars_blocks() {
        let errors = model(r#"vars { a = 1 } vars { a = 2 } trace "example" {}"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateVar {
                rule: spec::ids::UNIQUE_VARS,
                name: "a".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_variables_referencing_same_scope_variables() {
        for source in [
            r#"vars { a = 1 b = var.a } trace "example" {}"#,
            r#"vars { a = 1 b = "${var.a}" } trace "example" {}"#,
            r#"vars { a = 1 b = { c = var.a } } trace "example" {}"#,
            r#"vars { a = 1 b = choice(var.a, 2) } trace "example" {}"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::VarNotInScope {
                    rule: spec::ids::VISIBLE_VARS,
                    name: "a".to_owned(),
                }
            );
        }
    }

    #[test]
    fn shares_dynamic_root_vars_as_a_single_binding() {
        let model = model(r#"vars { m = choice("a", "b") } trace "t" { input = var.m output = var.m }"#).unwrap();

        assert_eq!(model.bindings.len(), 1);
        assert_eq!(model.bindings[0].name, "m");
        assert!(matches!(&model.traces[0].fields.input, Some(Value::VarRef(name)) if name == "m"));
        assert!(matches!(&model.traces[0].fields.output, Some(Value::VarRef(name)) if name == "m"));
    }

    #[test]
    fn scopes_vars_to_spans_and_dynamic_blocks() {
        let model = model(
            r#"
            trace "t" {
                llm "Chat Completion" {
                    vars { pt = range(1, 9) }
                    metrics = { prompt_tokens = var.pt, tokens = var.pt + 4 }
                }
                repeat {
                    count = 2
                    vars { r = range(1, 9) }
                    task "turn" { input = var.r }
                }
            }
            "#,
        )
        .unwrap();

        assert!(model.bindings.is_empty());
        assert!(model.traces[0].bindings.is_empty());
        let children = &model.traces[0].children;
        let Child::Span(llm) = &children[0] else {
            panic!("expected a span");
        };
        assert_eq!(llm.bindings.len(), 1);
        assert_eq!(llm.bindings[0].name, "pt");
        let Child::Repeat(repeat) = &children[1] else {
            panic!("expected a repeat");
        };
        assert_eq!(repeat.bindings.len(), 1);
        assert_eq!(repeat.bindings[0].name, "r");
    }

    #[test]
    fn resolves_outer_scope_vars_in_var_values() {
        let model = model(
            r#"
            vars { base = 100 }
            trace "t" {
                vars { n = var.base + range(1, 9) }
                input = var.n
            }
            "#,
        )
        .unwrap();

        // base is constant and substitutes, n is dynamic and binds on the trace
        assert!(model.bindings.is_empty());
        assert_eq!(model.traces[0].bindings.len(), 1);
        assert_eq!(model.traces[0].bindings[0].name, "n");
        assert!(matches!(&model.traces[0].fields.input, Some(Value::VarRef(name)) if name == "n"));
    }

    #[test]
    fn resolves_outer_dynamic_vars_in_var_values_as_binding_refs() {
        let model = model(
            r#"
            vars { total = range(10, 20) }
            trace "t" {
                vars { half = var.total / 2 }
                input = var.half
            }
            "#,
        )
        .unwrap();

        assert_eq!(model.bindings.len(), 1);
        let Value::Binary { lhs, .. } = &model.traces[0].bindings[0].value else {
            panic!("expected a binary binding");
        };
        assert!(matches!(&**lhs, Value::VarRef(name) if name == "total"));
    }

    #[test]
    fn rejects_references_outside_the_declaring_block() {
        let errors = model(
            r#"
            trace "t" {
                task "a" {
                    vars { x = range(1, 2) }
                    input = var.x
                }
                task "b" { input = var.x }
            }
            "#,
        )
        .unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::VarNotInScope {
                rule: spec::ids::VISIBLE_VARS,
                name: "x".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_repeat_index_in_vars_outside_a_repeat() {
        let errors = model(r#"vars { q = "q ${repeat.index}" } trace "t" { repeat { count = 1 task "x" { input = var.q } } }"#)
            .unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::RepeatRefOutsideRepeat { .. }));
    }

    #[test]
    fn resolves_repeat_index_in_repeat_scope_vars() {
        let model = model(
            r#"
            trace "t" {
                repeat {
                    count = 2
                    vars { q = "q ${repeat.index}" }
                    task "turn" { input = var.q }
                }
            }
            "#,
        )
        .unwrap();

        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat");
        };
        assert_eq!(repeat.bindings[0].name, "q");
        let Value::Template(template) = &repeat.bindings[0].value else {
            panic!("expected a template binding");
        };
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::RepeatIndex)));
    }

    #[test]
    fn models_context_references_as_values() {
        let model = model(
            r#"
            trace "t" {
                input = trace.index
                repeat {
                    count = 2
                    task "turn" { metrics = { turn = repeat.index, of = repeat.count } }
                }
            }
            "#,
        )
        .unwrap();

        assert!(matches!(
            model.traces[0].fields.input,
            Some(Value::CtxRef(CtxRef::TraceIndex))
        ));
        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat");
        };
        let Child::Span(task) = &repeat.children[0] else {
            panic!("expected a span");
        };
        let metrics = task.fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::CtxRef(CtxRef::RepeatIndex)));
        assert!(matches!(metrics.elem[1].value, Value::CtxRef(CtxRef::RepeatCount)));
    }

    #[test]
    fn rejects_repeat_refs_in_exprs_outside_a_repeat() {
        let source = r#"trace "t" { input = repeat.count }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::RepeatRefOutsideRepeat {
                rule: spec::ids::REPEAT_REFS,
                path: "repeat.count".to_owned(),
            }
        );
        assert!(errors[0].to_string().contains("`repeat.count` is not inside a repeat block"));

        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "repeat.count");
    }

    #[test]
    fn rejects_unknown_context_fields_in_exprs() {
        let source = r#"trace "t" { input = trace.name }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::InvalidRefSegment {
                rule: spec::ids::REF_FIELDS,
                segment: "name".to_owned(),
            }
        );
    }

    #[test]
    fn keeps_context_reference_exprs_dynamic() {
        let model = model(
            r#"
            trace "t" {
                repeat {
                    count = 2
                    vars { turn = repeat.index + 1 }
                    task "x" { input = var.turn }
                }
            }
            "#,
        )
        .unwrap();

        // a ctx ref never folds, so the var stays a per-iteration binding
        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat");
        };
        assert_eq!(repeat.bindings[0].name, "turn");
        let Value::Binary { lhs, .. } = &repeat.bindings[0].value else {
            panic!("expected a dynamic binary binding");
        };
        assert!(matches!(**lhs, Value::CtxRef(CtxRef::RepeatIndex)));
    }

    #[test]
    fn resolves_repeat_count_in_templates() {
        let model = model(
            r#"trace "t" { repeat { count = 2 task "x" { input = "turn ${repeat.index} of ${repeat.count}" } } }"#,
        )
        .unwrap();

        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat");
        };
        let Child::Span(task) = &repeat.children[0] else {
            panic!("expected a span");
        };
        let Some(Value::Template(template)) = &task.fields.input else {
            panic!("expected a template input");
        };
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::RepeatIndex)));
        assert!(matches!(template.parts[3], Part::Ref(CtxRef::RepeatCount)));
    }

    #[test]
    fn rejects_repeat_counts_referencing_the_repeats_own_vars() {
        let errors = model(r#"trace "t" { repeat { vars { n = range(1, 3) } count = var.n task "x" {} } }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarNotInScope { .. }));
    }

    #[test]
    fn allows_repeat_counts_referencing_outer_dynamic_vars() {
        let model = model(
            r#"
            trace "t" {
                vars { turns = range(1, 4) }
                repeat { count = var.turns task "x" {} }
                task "summary" { input = { turns = var.turns } }
            }
            "#,
        )
        .unwrap();

        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat");
        };
        assert!(matches!(&repeat.count, Value::VarRef(name) if name == "turns"));
    }

    #[test]
    fn unrolls_for_exprs_over_dynamic_vars_as_binding_accessors() {
        let model = model(r#"vars { xs = [range(1, 5), range(6, 9)] } trace "t" { input = [for x in var.xs : x] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert_eq!(array.elem.len(), 2);
        assert!(array.elem.iter().all(|value| matches!(
            value,
            ArrayElem::Item(Value::Index { target, .. }) if matches!(&**target, Value::VarRef(name) if name == "xs")
        )));
    }

    #[test]
    fn rejects_interpolating_non_scalar_variables() {
        let source = r#"vars { m = { x = 1 } } trace "example" { input = "${var.m}" }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonScalarInterpolation {
                rule: spec::ids::SCALAR_INTERPOLATION,
                found: ExprType::Object,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "${var.m}");
    }

    #[test]
    fn models_choice_and_range_funcs() {
        let model = model(r#"trace "t" { input = choice("a", 1) metrics = { n = range(1, 5), x = range(0, 1.5) } }"#).unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Func {
            func: Func::Choice(options),
            ..
        }) = &fields.input
        else {
            panic!("expected a choice");
        };
        assert!(matches!(&options[0], Value::Str(value) if value == "a"));
        assert!(matches!(options[1], Value::Num(Number::Int(1))));

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(
            metrics.elem[0].value,
            Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            }
        ));
        assert!(matches!(
            metrics.elem[1].value,
            Value::Func { func: Func::Range(Range::Float { min, max }), .. } if min == 0.0 && max == 1.5
        ));
    }

    #[test]
    fn resolves_variables_inside_func_args() {
        let model = model(r#"vars { m = "gpt" } trace "t" { input = choice(var.m, "x") }"#).unwrap();

        let Some(Value::Func {
            func: Func::Choice(options),
            ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a choice");
        };
        assert!(matches!(&options[0], Value::Str(value) if value == "gpt"));
    }

    #[test]
    fn rejects_unknown_functions() {
        let source = r#"trace "t" { input = shuffle(1, 2) }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownFunction {
                rule: spec::ids::KNOWN_FUNCTIONS,
                name: "shuffle".to_owned(),
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "shuffle(1, 2)");
    }

    #[test]
    fn rejects_choices_without_alternatives() {
        let errors = model(r#"trace "t" { input = choice() }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::EmptyChoice {
                rule: spec::ids::CHOICE_ALTERNATIVES,
            }
        );
    }

    #[test]
    fn rejects_invalid_range_args_and_bounds() {
        for source in [
            r#"trace "t" { input = range(1) }"#,
            r#"trace "t" { input = range(1, 2, 3) }"#,
            r#"trace "t" { input = range(1, "x") }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::InvalidRangeArgs {
                    rule: spec::ids::RANGE_BOUNDS,
                }
            );
        }

        for source in [
            r#"trace "t" { input = range(5, 1) }"#,
            r#"trace "t" { input = range(1.5, 0.5) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::InvalidRangeBounds {
                    rule: spec::ids::RANGE_BOUNDS,
                }
            );
        }
    }

    #[test]
    fn models_weighted_funcs() {
        let model = model(r#"trace "t" { input = weighted(["a", 8], [range(1, 3), 2], ["c", 0]) }"#).unwrap();

        let Some(Value::Func {
            func: Func::Weighted(options),
            ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a weighted pick");
        };
        assert_eq!(options.len(), 3);
        assert!(matches!(&options[0].value, Value::Str(value) if value == "a"));
        assert!((options[0].weight - 8.0).abs() < f64::EPSILON);
        assert!(matches!(
            &options[1].value,
            Value::Func {
                func: Func::Range(_),
                ..
            }
        ));
        assert!(options[2].weight == 0.0);
    }

    #[test]
    fn rejects_invalid_weighted_options() {
        for source in [
            r#"trace "t" { input = weighted("a") }"#,
            r#"trace "t" { input = weighted(["a", 1, 2]) }"#,
            r#"trace "t" { input = weighted(["a", range(1, 2)]) }"#,
            r#"trace "t" { input = weighted(["a", 0 - 1]) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::WeightedOptionShape { .. }),
                "unexpected error for {source}: {}",
                errors[0]
            );
        }

        let errors = model(r#"trace "t" { input = weighted(["a", 0], ["b", 0]) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::WeightedTotal { .. }));

        let errors = model(r#"trace "t" { input = weighted() }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "weighted", .. }));
    }

    #[test]
    fn models_distribution_funcs() {
        let model = model(
            r#"
                trace "t" {
                    metrics = {
                        a = normal(0.5, 0.1)
                        b = lognormal(300, 0.5)
                        c = exponential(250)
                        d = pareto(100, 1.5)
                        e = beta(2, 5)
                        f = poisson(3)
                    }
                    input = chance(0.25)
                }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(
            metrics.elem[0].value,
            Value::Func { func: Func::Normal { mean, stddev }, .. } if mean == 0.5 && stddev == 0.1
        ));
        assert!(matches!(
            metrics.elem[1].value,
            Value::Func { func: Func::Lognormal { median, sigma }, .. } if median == 300.0 && sigma == 0.5
        ));
        assert!(matches!(
            metrics.elem[2].value,
            Value::Func { func: Func::Exponential { mean }, .. } if mean == 250.0
        ));
        assert!(matches!(
            metrics.elem[3].value,
            Value::Func { func: Func::Pareto { min, shape }, .. } if min == 100.0 && shape == 1.5
        ));
        assert!(matches!(
            metrics.elem[4].value,
            Value::Func { func: Func::Beta { alpha, beta }, .. } if alpha == 2.0 && beta == 5.0
        ));
        assert!(matches!(
            metrics.elem[5].value,
            Value::Func { func: Func::Poisson { mean }, .. } if mean == 3.0
        ));
        assert!(matches!(
            fields.input,
            Some(Value::Func { func: Func::Chance { probability }, .. }) if probability == 0.25
        ));
    }

    #[test]
    fn rejects_invalid_distribution_params() {
        let errors = model(r#"trace "t" { input = normal(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "normal", .. }));

        let errors = model(r#"trace "t" { input = normal(range(0, 1), 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantParam {
                func: "normal",
                param: "mean",
                ..
            }
        ));

        for (source, func, param) in [
            (r#"trace "t" { input = normal(0, 0 - 1) }"#, "normal", "stddev"),
            (r#"trace "t" { input = lognormal(0, 1) }"#, "lognormal", "median"),
            (r#"trace "t" { input = exponential(0) }"#, "exponential", "mean"),
            (r#"trace "t" { input = pareto(0, 1) }"#, "pareto", "min"),
            (r#"trace "t" { input = beta(1, 0) }"#, "beta", "beta"),
            (r#"trace "t" { input = poisson(0) }"#, "poisson", "mean"),
            (r#"trace "t" { input = chance(1.5) }"#, "chance", "probability"),
        ] {
            let errors = model(source).unwrap_err();
            match errors[0].kind() {
                ErrorKind::ParamOutOfRange {
                    func: found_func,
                    param: found_param,
                    ..
                } => {
                    assert_eq!((*found_func, *found_param), (func, param), "wrong param for {source}");
                }
                other => panic!("unexpected error for {source}: {other}"),
            }
        }
    }

    #[test]
    fn models_string_funcs() {
        let model = model(
            r#"
                vars { tags = ["a", "b"] }
                trace "t" {
                    input = upper(choice("get", "post"))
                    output = format("model={} n={}", "gpt", range(1, 5))
                    expected = join(var.tags, ", ")
                    error = contains(var.tags, "a")
                    metadata = { parts = split("a,b", ","), has = contains("abc", "b") }
                }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(
            &fields.input,
            Some(Value::Func {
                func: Func::Upper { .. },
                ..
            })
        ));
        assert!(matches!(
            &fields.output,
            Some(Value::Func { func: Func::Format { pieces, args }, .. })
                if *pieces == ["model=", " n=", ""] && args.len() == 2
        ));
        assert!(matches!(
            &fields.expected,
            Some(Value::Func {
                func: Func::Join { .. },
                ..
            })
        ));
        assert!(matches!(
            &fields.error,
            Some(Value::Func {
                func: Func::Contains { .. },
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_string_func_args() {
        for (source, func) in [
            (r#"trace "t" { input = upper(1) }"#, "upper"),
            (r#"trace "t" { input = trim(null) }"#, "trim"),
            (r#"trace "t" { input = split("a", 1) }"#, "split"),
            (r#"trace "t" { input = join("a", ",") }"#, "join"),
            (r#"trace "t" { input = contains(1, "x") }"#, "contains"),
            (r#"trace "t" { input = len(true) }"#, "len"),
            (r#"trace "t" { input = tokens(1) }"#, "tokens"),
            (r#"trace "t" { input = starts_with("a", 1) }"#, "starts_with"),
            (r#"trace "t" { input = upper(choice("a", 1)) }"#, "upper"),
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::FuncArgType { func: found, .. } if *found == func),
                "unexpected error for {source}: {}",
                errors[0]
            );
        }

        let errors = model(r#"trace "t" { input = replace("a", "b") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "replace", .. }));

        let errors = model(r#"trace "t" { input = split("a,b", "") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::EmptySplitSeparator { .. }));
    }

    #[test]
    fn rejects_invalid_format_calls() {
        let errors = model(r#"trace "t" { input = format(range(0, 1), 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::FuncArgType {
                func: "format",
                expected: "constant string",
                ..
            }
        ));

        let errors = model(r#"trace "t" { input = format("{} {}", 1) }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::FormatPlaceholders {
                rule: spec::ids::FORMAT_TEMPLATE,
                placeholders: 2,
                args: 1,
            }
        );

        let errors = model(r#"trace "t" { input = format("{}", [1]) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "format", .. }));
    }

    #[test]
    fn models_numeric_funcs() {
        let model = model(
            r#"
                trace "t" {
                    metrics = {
                        a = clamp(lognormal(300, 0.5), 20, 30000)
                        b = round(normal(0, 1))
                        c = min(range(1, 10), 5)
                        d = abs(0 - range(1, 5))
                    }
                }
            "#,
        )
        .unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(
            &metrics.elem[0].value,
            Value::Func {
                func: Func::Clamp { .. },
                ..
            }
        ));
        assert!(matches!(
            &metrics.elem[1].value,
            Value::Func {
                func: Func::Round { .. },
                ..
            }
        ));
        assert!(matches!(
            &metrics.elem[2].value,
            Value::Func { func: Func::Min(values), .. } if values.len() == 2
        ));
        assert!(matches!(
            &metrics.elem[3].value,
            Value::Func {
                func: Func::Abs { .. },
                ..
            }
        ));
    }

    #[test]
    fn rejects_invalid_numeric_func_args() {
        let errors = model(r#"trace "t" { input = clamp(range(1, 5), 10, 2) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::ClampBoundsOutOfOrder { .. }));

        let errors = model(r#"trace "t" { input = round("x") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "round", .. }));

        let errors = model(r#"trace "t" { input = min(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "min", .. }));

        let errors = model(r#"trace "t" { input = max(1, "x") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "max", .. }));
    }

    #[test]
    fn models_id_funcs_and_rejects_invalid_lengths() {
        let model_ok = model(r#"vars { n = 8 } trace "t" { input = [uuid(), hex(16), alphanum(var.n)] }"#).unwrap();
        let Some(Value::Array(array)) = &model_ok.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(array.elem[0], ArrayElem::Item(Value::Func { func: Func::Uuid, .. })));
        assert!(matches!(
            array.elem[1],
            ArrayElem::Item(Value::Func {
                func: Func::Hex { length: 16 },
                ..
            })
        ));
        assert!(matches!(
            array.elem[2],
            ArrayElem::Item(Value::Func {
                func: Func::Alphanum { length: 8 },
                ..
            })
        ));

        let errors = model(r#"trace "t" { input = uuid(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "uuid", .. }));

        let errors = model(r#"trace "t" { input = hex(range(1, 4)) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantParam {
                func: "hex",
                param: "length",
                ..
            }
        ));

        for source in [
            r#"trace "t" { input = hex(0 - 1) }"#,
            r#"trace "t" { input = alphanum(1.5) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(matches!(errors[0].kind(), ErrorKind::ParamOutOfRange { param: "length", .. }));
        }
    }

    #[test]
    fn types_func_results_for_operator_and_arg_checks() {
        // string and number results compose with operators and other funcs
        model(r#"trace "t" { input = upper("a") == "A" ? 1 : 2 }"#).unwrap();
        model(r#"trace "t" { input = len("abc") + poisson(2) }"#).unwrap();
        model(r#"trace "t" { input = lower(format("{}", weighted(["A", 1], ["B", 3]))) }"#).unwrap();

        // a mixed weighted pick has no static type, so typed args reject it
        let errors = model(r#"trace "t" { input = upper(weighted(["a", 1], [2, 1])) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "upper", .. }));
    }

    #[test]
    fn rejects_funcs_where_a_specific_type_is_expected() {
        let tags = model(r#"trace "t" { tags = [choice("a", "b")] }"#).unwrap_err();
        assert!(matches!(
            tags[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
        assert!(tags[0].to_string().contains("expects string, but found function"));

        let metadata = model(r#"trace "t" { metadata = choice({}, {}) }"#).unwrap_err();
        assert!(matches!(
            metadata[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn interpolates_func_variables_with_agreeing_scalar_types() {
        let model = model(r#"vars { s = choice("a", "b") } trace "t" { input = "${var.s}" }"#).unwrap();

        assert_eq!(model.bindings.len(), 1);
        let Some(Value::Template(template)) = &model.traces[0].fields.input else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::VarRef(name) if name == "s"));
    }

    #[test]
    fn rejects_named_vars_blocks_and_nested_blocks_in_vars() {
        let named = model(r#"vars "named" {} trace "example" {}"#).unwrap_err();
        assert_eq!(named[0].kind(), &ErrorKind::UnexpectedName { block: spec::ids::VARS });

        let nested = model(r#"vars { task "t" {} } trace "example" {}"#).unwrap_err();
        assert_eq!(
            nested[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TASK,
                parent: spec::Place::Block { id: spec::ids::VARS },
            }
        );
    }

    #[test]
    fn rejects_null_where_a_specific_type_is_expected() {
        let errors = model(r#"trace "example" { tags = [null] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::TAGS,
                expected: &spec::ExprType::String,
                found: ExprType::Null,
            }
        );
        assert!(errors[0].to_string().contains("expects string, but found null"));
    }

    #[test]
    fn collects_an_error_for_each_invalid_tag() {
        let errors = match model(r#"trace "example" { tags = [true , false , 1] }"#) {
            Err(errors) => errors,
            Ok(_) => panic!("expected semantic errors"),
        };

        assert_eq!(errors.len(), 3);
        assert!(errors.iter().all(|error| {
            matches!(
                error.kind(),
                ErrorKind::TypeMismatch {
                    block: spec::ids::TRACE,
                    field: spec::ids::TAGS,
                    expected: spec::ExprType::String,
                    ..
                }
            )
        }));
    }

    #[test]
    fn applies_block_rules_from_the_spec() {
        let root_errors = model(r#"task "root" {}"#).unwrap_err();
        assert_eq!(
            root_errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TASK,
                parent: spec::Place::Root,
            }
        );

        let nested_errors = model(r#"trace "outer" { trace "inner" {} }"#).unwrap_err();
        assert_eq!(
            nested_errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TRACE,
                parent: spec::Place::Block { id: spec::ids::TRACE },
            }
        );

        let name_errors = model("trace {}").unwrap_err();
        assert_eq!(name_errors[0].kind(), &ErrorKind::MissingName { block: spec::ids::TRACE });
    }

    #[test]
    fn distinguishes_unknown_blocks_and_root_attributes() {
        let root_attr = model("input = true").unwrap_err();
        assert_eq!(
            root_attr[0].kind(),
            &ErrorKind::RootAttribute {
                keyword: "input".to_owned(),
            }
        );

        let unknown = model(r#"trace "outer" { custom "inner" {} }"#).unwrap_err();
        assert_eq!(
            unknown[0].kind(),
            &ErrorKind::UnknownBlock {
                keyword: "custom".to_owned(),
                parent: spec::Place::Block { id: spec::ids::TRACE },
            }
        );
    }

    #[test]
    fn rejects_fields_absent_from_the_block_spec() {
        let errors = model(r#"trace "example" { unknown = true }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownField {
                block: spec::ids::TRACE,
                keyword: "unknown".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_reserved_metric_keys() {
        let source = r#"trace "example" { metrics = { start = 1, tokens = 4, end = 2 } }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ReservedMetricKey {
                rule: spec::ids::RESERVED_METRICS,
                key: "start".to_owned(),
            }
        );
        assert_eq!(
            errors[1].kind(),
            &ErrorKind::ReservedMetricKey {
                rule: spec::ids::RESERVED_METRICS,
                key: "end".to_owned(),
            }
        );

        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "start");
    }

    #[test]
    fn rejects_shapes_without_traces() {
        for source in ["", "   \n  "] {
            let errors = model(source).unwrap_err();

            assert_eq!(errors.len(), 1);
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::EmptyShape {
                    rule: spec::ids::NONEMPTY_SHAPE
                }
            );
        }
    }

    #[test]
    fn does_not_stack_the_empty_shape_error_onto_other_failures() {
        let errors = model("input = true").unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0].kind(), ErrorKind::RootAttribute { .. }));
    }

    #[test]
    fn associates_custom_validation_with_spec_rules() {
        let duplicate = model(r#"trace "example" { metadata = { key = 1, key = 2 } }"#).unwrap_err();
        assert_eq!(
            duplicate[0].kind(),
            &ErrorKind::DuplicateObjectKey {
                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                key: "key".to_owned(),
            }
        );
        assert!(
            duplicate[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::UNIQUE_OBJECT_KEYS).unwrap().summary)
        );

        let raw = "999999999999999999999999999999999999";
        let invalid_number = model(&format!(r#"trace "example" {{ input = {raw} }}"#)).unwrap_err();
        assert_eq!(
            invalid_number[0].kind(),
            &ErrorKind::InvalidNumber {
                rule: spec::ids::FINITE_NUMBERS,
                raw: raw.to_owned(),
            }
        );
        assert!(
            invalid_number[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::FINITE_NUMBERS).unwrap().summary)
        );
    }

    #[test]
    fn reports_validation_diagnostics_at_the_invalid_source() {
        let source = r#"trace "example" { metadata = true }"#;
        let tokens = crate::dsl::lexer::lex(source).unwrap();
        let ast = crate::dsl::parser::parse(tokens, source).unwrap();

        let diagnostics = match super::model(ast) {
            Err(diagnostics) => diagnostics,
            Ok(_) => panic!("expected a validation diagnostic"),
        };

        assert_eq!(
            diagnostics,
            vec![Diag {
                when: DiagPhase::Validation,
                what: "field `metadata` in block `trace` expects object, but found boolean".to_owned(),
                r#where: SrcRange::new(29, 33),
            }]
        );
    }

    #[test]
    fn folds_constant_arithmetic() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 2 * 3, b = 7 / 2, c = 7 % 2, d = 10 - 12 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(7))));
        // integer division truncates toward zero
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(3))));
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Int(1))));
        assert!(matches!(metrics.elem[3].value, Value::Num(Number::Int(-2))));
    }

    #[test]
    fn folds_float_promotion_and_keeps_floats_float() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 0.5, b = 3.0 / 2, c = 1.5 + 1.5 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == 1.5));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Float(value)) if value == 1.5));
        // a whole float result stays a float through re-literalization
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Float(value)) if value == 3.0));
    }

    #[test]
    fn folds_comparisons_equality_and_logic() {
        let source = r#"trace "t" { metrics = {
            a = 1 < 2
            b = "x" == "x"
            c = 1.0 == 1
            d = true && false
            e = !(1 > 2)
            f = 1 != 2
        } }"#;
        let model = model(source).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        let expected = [true, true, true, false, true, true];
        for (field, want) in metrics.elem.iter().zip(expected) {
            assert!(
                matches!(field.value, Value::Bool(value) if value == want),
                "field {}",
                field.key
            );
        }
    }

    #[test]
    fn folds_constant_ternaries_into_typed_positions() {
        let model = model(r#"trace "t" { tags = [true ? "a" : "b", false ? "c" : "d"] }"#).unwrap();
        let tags = &model.traces[0].fields.tags;

        assert!(matches!(&tags[0].parts[0], Part::Lit(value) if value == "a"));
        assert!(matches!(&tags[1].parts[0], Part::Lit(value) if value == "d"));
    }

    #[test]
    fn folds_negation_textually() {
        let model = model(r#"trace "t" { metrics = { a = -9223372036854775808, b = --5, c = -1.5 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        // i64::MIN only exists via textual negation, the bare literal overflows
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(i64::MIN))));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(5))));
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Float(value)) if value == -1.5));
    }

    #[test]
    fn folds_grouped_expressions() {
        let model = model(r#"trace "t" { input = (1 + 2) * 3 }"#).unwrap();

        assert!(matches!(model.traces[0].fields.input, Some(Value::Num(Number::Int(9)))));
    }

    #[test]
    fn rejects_mismatched_operand_types() {
        let source = r#"trace "t" { input = 1 + "a" }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "+".to_owned(),
                expected: "number",
                found: ExprType::String,
            }
        );
        assert_eq!(&source[errors[0].range().start..errors[0].range().end], "\"a\"");

        let errors = model(r#"trace "t" { input = 1 && true }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "&&".to_owned(),
                expected: "boolean",
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = !5 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "!".to_owned(),
                expected: "boolean",
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = [1] + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Array,
                ..
            }
        ));
    }

    #[test]
    fn rejects_null_and_cross_type_equality() {
        // null is not comparable, both sides report
        let errors = model(r#"trace "t" { input = null == null }"#).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                expected: "string, number, or boolean",
                found: ExprType::Null,
                ..
            }
        ));

        // the left operand fixes the comparison type
        let errors = model(r#"trace "t" { input = "a" == 1 }"#).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                expected: "string",
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_operands_without_a_static_type() {
        // heterogeneous choice has no unified type
        let errors = model(r#"trace "t" { input = choice("a", 1) + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));

        // a dynamic ternary with mixed branch types is untyped as an operand
        let errors = model(r#"trace "t" { input = (range(0, 1) == 1 ? 1 : "x") + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn rejects_non_boolean_conditions() {
        let errors = model(r#"trace "t" { input = 1 ? 2 : 3 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonBooleanCondition {
                rule: spec::ids::BOOLEAN_CONDITIONS,
                found: ExprType::Number,
            }
        );
    }

    #[test]
    fn rejects_constant_zero_divisors() {
        for source in [
            r#"trace "t" { input = 1 / 0 }"#,
            r#"trace "t" { input = 1 % 0 }"#,
            r#"trace "t" { input = 1 / 0.0 }"#,
            // static even with a dynamic lhs
            r#"trace "t" { input = range(1, 2) / 0 }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::DivisionByZero { .. }),
                "source: {source}"
            );
        }

        // the diagnostic points at the zero
        let source = r#"trace "t" { input = 1 / 0 }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(&source[errors[0].range().start..errors[0].range().end], "0");
    }

    #[test]
    fn rejects_non_finite_constant_results() {
        let errors = model(r#"trace "t" { input = 9223372036854775807 + 1 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonFiniteResult {
                rule: spec::ids::FINITE_NUMBERS,
            }
        );

        let big = format!("{}.0", "9".repeat(200));
        let errors = model(&format!(r#"trace "t" {{ input = {big} * {big} }}"#)).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::NonFiniteResult { .. }));
    }

    #[test]
    fn folds_variables_in_operator_exprs() {
        let model = model(r#"vars { n = 2 } trace "t" { input = var.n + 1 }"#).unwrap();

        assert!(matches!(model.traces[0].fields.input, Some(Value::Num(Number::Int(3)))));
    }

    #[test]
    fn rejects_operator_exprs_referencing_same_scope_vars_in_vars() {
        let errors = model(r#"vars { a = 1 b = 1 + var.a } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarNotInScope { .. }));
    }

    #[test]
    fn interpolates_folded_constant_vars() {
        let model = model(r#"vars { n = 1 + 2 } trace "t" { output = "n ${var.n}" }"#).unwrap();

        assert!(matches!(&model.traces[0].fields.output, Some(Value::Str(value)) if value == "n 3"));
    }

    #[test]
    fn interpolates_dynamic_scalar_vars_as_binding_refs() {
        let model = model(r#"vars { d = 1 + range(1, 2) } trace "t" { output = "d ${var.d}" }"#).unwrap();

        assert_eq!(model.bindings.len(), 1);
        assert_eq!(model.bindings[0].name, "d");
        let Some(Value::Template(template)) = &model.traces[0].fields.output else {
            panic!("expected a template output");
        };
        assert!(matches!(&template.parts[1], Part::VarRef(name) if name == "d"));
    }

    #[test]
    fn rejects_interpolating_dynamic_vars_without_a_scalar_type() {
        let errors = model(r#"vars { d = choice("a", 1) } trace "t" { output = "${var.d}" }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::NonScalarInterpolation { .. }));
    }

    #[test]
    fn rejects_dynamic_exprs_for_typed_fields() {
        let errors = model(r#"trace "t" { tags = [range(1, 2) > 1 ? "a" : "b"] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
        assert!(errors[0].to_string().contains("found expression"));

        let errors = model(r#"trace "t" { metadata = 1 + range(0, 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn models_dynamic_operator_exprs() {
        let source = r#"trace "t" { input = 1 + range(0, 1) }"#;
        let model = model(source).unwrap();

        let Some(Value::Binary { op, lhs, rhs, range }) = &model.traces[0].fields.input else {
            panic!("expected a dynamic binary expression");
        };
        assert_eq!(*op, BinOp::Add);
        assert!(matches!(**lhs, Value::Num(Number::Int(1))));
        assert!(matches!(
            **rhs,
            Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            }
        ));
        assert_eq!(&source[range.start..range.end], "1 + range(0, 1)");
    }

    #[test]
    fn models_dynamic_ternaries() {
        let model = model(r#"trace "t" { input = range(0, 1) == 0 ? "a" : "b" }"#).unwrap();

        let Some(Value::Cond {
            cond, then, otherwise, ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a dynamic conditional");
        };
        assert!(matches!(**cond, Value::Binary { op: BinOp::Eq, .. }));
        assert!(matches!(&**then, Value::Str(value) if value == "a"));
        assert!(matches!(&**otherwise, Value::Str(value) if value == "b"));
    }

    #[test]
    fn folds_constant_indexes_into_literal_containers() {
        let model = model(
            r#"
            vars {
                xs = [10, 20, 30]
                user = { name = "ada", langs = ["en", "fr"] }
            }
            trace "t" {
                input = var.xs[1 + 1]
                output = var.user["name"]
                metrics = { pick = [{ n = 1 }, { n = 2 }][0].n }
                tags = [var.user.langs[1]]
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(fields.input, Some(Value::Num(Number::Int(30)))));
        assert!(matches!(&fields.output, Some(Value::Str(value)) if value == "ada"));
        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(1))));
        assert_eq!(tag_text(&fields.tags[0]), "fr");
    }

    #[test]
    fn folds_a_constant_index_selecting_a_dynamic_element() {
        // selection is static even though the selected element is not
        let model = model(r#"trace "t" { input = [range(1, 5), "x"][0] }"#).unwrap();

        assert!(matches!(
            model.traces[0].fields.input,
            Some(Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            })
        ));
    }

    #[test]
    fn rejects_out_of_bounds_constant_indexes() {
        let source = r#"trace "t" { input = [1, 2][2] }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexOutOfBounds {
                rule: spec::ids::INDEX_BOUNDS,
                index: 2,
                len: 2,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "2");

        let errors = model(r#"trace "t" { input = [1, 2][-1] }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn rejects_unknown_constant_object_keys() {
        let errors = model(r#"trace "t" { input = { a = 1 }.b }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownObjectKey {
                rule: spec::ids::INDEX_BOUNDS,
                key: "b".to_owned(),
            }
        );
        assert!(errors[0].to_string().contains("object has no key `b`"));
    }

    #[test]
    fn rejects_non_integer_constant_indexes() {
        let errors = model(r#"trace "t" { input = [1, 2][0.5] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerIndex {
                rule: spec::ids::INDEXABLE_TARGETS,
            }
        );
    }

    #[test]
    fn rejects_indexing_non_containers() {
        for (source, found) in [
            (r#"trace "t" { input = 5[0] }"#, ExprType::Number),
            (r#"trace "t" { input = "x"[0] }"#, ExprType::String),
            (r#"trace "t" { input = true.a }"#, ExprType::Boolean),
            (r#"trace "t" { input = null[0] }"#, ExprType::Null),
            (r#"trace "t" { input = range(1, 2)[0] }"#, ExprType::Number),
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::NonIndexableTarget {
                    rule: spec::ids::INDEXABLE_TARGETS,
                    found,
                },
                "source: {source}"
            );
        }
    }

    #[test]
    fn rejects_mismatched_index_types() {
        let errors = model(r#"trace "t" { input = [1, 2]["a"] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexTypeMismatch {
                rule: spec::ids::INDEXABLE_TARGETS,
                expected: "number",
                found: ExprType::String,
            }
        );

        let errors = model(r#"trace "t" { input = { a = 1 }[0] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexTypeMismatch {
                rule: spec::ids::INDEXABLE_TARGETS,
                expected: "string",
                found: ExprType::Number,
            }
        );

        // a heterogeneous choice has no static type, so it can't index
        let errors = model(r#"trace "t" { input = [1, 2][choice(0, "a")] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::IndexTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn models_dynamic_index_selections() {
        let source = r#"trace "t" { input = [1, 2][range(0, 1)] }"#;
        let model = model(source).unwrap();

        let Some(Value::Index { target, index, range }) = &model.traces[0].fields.input else {
            panic!("expected a dynamic index expression");
        };
        assert!(matches!(&**target, Value::Array(array) if array.elem.len() == 2));
        assert!(matches!(
            **index,
            Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            }
        ));
        assert_eq!(&source[range.start..range.end], "[1, 2][range(0, 1)]");
    }

    #[test]
    fn types_dynamic_indexes_by_their_unified_element_type() {
        // homogeneous elements type the index, so it can be an operand
        let model = model(r#"trace "t" { metrics = { n = [1, 2][range(0, 1)] * 10 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Binary { .. }));
    }

    #[test]
    fn rejects_untyped_dynamic_indexes_as_operands_and_typed_fields() {
        let errors = model(r#"trace "t" { input = [1, "a"][range(0, 1)] + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));

        let errors = model(r#"trace "t" { tags = [["a", "b"][range(0, 1)]] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn rejects_index_exprs_referencing_same_scope_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = var.a[0] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarNotInScope { .. }));
    }

    fn ints(value: &Value) -> Vec<i64> {
        let Value::Array(array) = value else {
            panic!("expected an array");
        };
        array
            .elem
            .iter()
            .map(|value| match value {
                ArrayElem::Item(Value::Num(Number::Int(value))) => *value,
                other => panic!("expected an integer, found {other:?}"),
            })
            .collect()
    }

    #[test]
    fn folds_constant_slices_with_clamped_bounds() {
        let model = model(
            r#"
            vars { xs = [1, 2, 3, 4] }
            trace "t" {
                input = var.xs[1:3]
                output = var.xs[:2]
                metadata = {
                    tail = var.xs[2:]
                    all = var.xs[:]
                    clamped = var.xs[1:99]
                    empty = var.xs[3:1]
                }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert_eq!(ints(fields.input.as_ref().unwrap()), [2, 3]);
        assert_eq!(ints(fields.output.as_ref().unwrap()), [1, 2]);
        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(ints(&metadata.elem[0].value), [3, 4]);
        assert_eq!(ints(&metadata.elem[1].value), [1, 2, 3, 4]);
        assert_eq!(ints(&metadata.elem[2].value), [2, 3, 4]);
        assert!(ints(&metadata.elem[3].value).is_empty());
    }

    #[test]
    fn folds_slices_into_typed_positions() {
        let model = model(r#"trace "t" { tags = ["a", "b", "c"][1:] }"#).unwrap();

        assert_eq!(
            model.traces[0].fields.tags.iter().map(tag_text).collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    #[test]
    fn models_dynamic_slice_selections() {
        let source = r#"trace "t" { input = [1, 2, 3][range(0, 1):] }"#;
        let model = model(source).unwrap();

        let Some(Value::Slice {
            target,
            start,
            end,
            range,
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a dynamic slice expression");
        };
        assert!(matches!(&**target, Value::Array(array) if array.elem.len() == 3));
        assert!(matches!(
            start.as_deref(),
            Some(Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            })
        ));
        assert!(end.is_none());
        assert_eq!(&source[range.start..range.end], "[1, 2, 3][range(0, 1):]");
    }

    #[test]
    fn rejects_invalid_slice_targets_and_bounds() {
        let errors = model(r#"trace "t" { input = 5[0:1] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonSliceableTarget {
                rule: spec::ids::SLICEABLE_TARGETS,
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = { a = 1 }[0:1] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonSliceableTarget {
                found: ExprType::Object,
                ..
            }
        ));

        let errors = model(r#"trace "t" { input = [1, 2]["a":] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SliceTypeMismatch {
                rule: spec::ids::SLICE_BOUNDS,
                found: ExprType::String,
            }
        );

        let errors = model(r#"trace "t" { input = [1, 2][0.5:] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerSliceBound {
                rule: spec::ids::SLICE_BOUNDS,
            }
        );

        // constant bounds check even when another bound is dynamic
        let source = r#"trace "t" { input = [1, 2][range(0, 1):-1] }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NegativeSliceBound {
                rule: spec::ids::SLICE_BOUNDS,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "-1");
    }

    #[test]
    fn splices_constant_array_spreads() {
        let model = model(
            r#"
            vars { xs = [2, 3] }
            trace "t" { input = [1, ...var.xs, 4] }
            "#,
        )
        .unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [1, 2, 3, 4]);
    }

    #[test]
    fn splices_spreads_of_dynamic_vars_as_binding_accessors() {
        let model = model(r#"vars { xs = [range(1, 5)] } trace "t" { input = [...var.xs] }"#).unwrap();

        // the sampled element is drawn once in the binding; the spliced element
        // indexes into it instead of re-sampling
        assert_eq!(model.bindings.len(), 1);
        let Value::Array(definition) = &model.bindings[0].value else {
            panic!("expected an array binding");
        };
        assert!(matches!(
            definition.elem[0],
            ArrayElem::Item(Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            })
        ));

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(
            &array.elem[0],
            ArrayElem::Item(Value::Index { target, .. }) if matches!(&**target, Value::VarRef(name) if name == "xs")
        ));
    }

    #[test]
    fn merges_object_spreads_with_later_entries_winning() {
        let model = model(
            r#"
            vars { meta = { model = "gpt", temperature = 0.2 } }
            trace "t" {
                metadata = { ...var.meta, temperature = 0.9 }
                metrics = { temperature = 1, ...var.meta }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        // the explicit key overrides the spread but keeps its position
        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem.len(), 2);
        assert_eq!(metadata.elem[0].key, "model");
        assert_eq!(metadata.elem[1].key, "temperature");
        assert!(matches!(metadata.elem[1].value, Value::Num(Number::Float(value)) if value == 0.9));

        // a later spread also overrides an earlier explicit key
        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == 0.2));
    }

    #[test]
    fn rejects_explicit_duplicate_keys_even_through_a_merge() {
        let errors = model(r#"trace "t" { metadata = { ...{}, a = 1, a = 2 } }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateObjectKey {
                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                key: "a".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_mismatched_and_dynamic_spread_operands() {
        let errors = model(r#"trace "t" { input = [...{ a = 1 }] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SpreadTypeMismatch {
                rule: spec::ids::SPREAD_OPERANDS,
                expected: "array",
                found: ExprType::Object,
            }
        );

        let errors = model(r#"trace "t" { metadata = { ...[1] } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SpreadTypeMismatch {
                rule: spec::ids::SPREAD_OPERANDS,
                expected: "object",
                found: ExprType::Array,
            }
        );

        // an array-typed dynamic spread stays residual and splices at generation
        let model_ok = model(r#"trace "t" { input = [...choice([1], [2])] }"#).unwrap();
        let Some(Value::Array(array)) = &model_ok.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(&array.elem[0], ArrayElem::Spread(Value::Func { .. })));

        // a spread whose type never settles is still rejected
        let errors = model(r#"trace "t" { input = [...choice([1], "x")] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::SpreadTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn rejects_spreads_referencing_same_scope_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = [...var.a] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarNotInScope { .. }));
    }

    #[test]
    fn unrolls_array_for_exprs() {
        let model = model(r#"trace "t" { input = [for x in [1, 2, 3] : x * 2] }"#).unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [2, 4, 6]);
    }

    #[test]
    fn unrolls_for_exprs_with_index_bindings_and_template_splices() {
        let model = model(r#"trace "t" { input = [for i, x in ["a", "b"] : "${i}-${x}"] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(&array.elem[0], ArrayElem::Item(Value::Str(value)) if value == "0-a"));
        assert!(matches!(&array.elem[1], ArrayElem::Item(Value::Str(value)) if value == "1-b"));
    }

    #[test]
    fn unrolls_for_exprs_with_constant_filters() {
        let model = model(r#"trace "t" { input = [for x in [1, 2, 3, 4] : x if x % 2 == 0] }"#).unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [2, 4]);
    }

    #[test]
    fn unrolls_for_exprs_over_objects() {
        let model = model(
            r#"
            vars { meta = { a = 1, b = 2, c = 3 } }
            trace "t" {
                input = [for k in var.meta : k]
                metadata = { for k, v in var.meta : k => v if k != "b" }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Array(keys)) = &fields.input else {
            panic!("expected an array");
        };
        let keys: Vec<_> = keys
            .elem
            .iter()
            .map(|value| match value {
                ArrayElem::Item(Value::Str(value)) => value.as_str(),
                other => panic!("expected a string, found {other:?}"),
            })
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);

        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem.len(), 2);
        assert_eq!((metadata.elem[0].key.as_str(), metadata.elem[1].key.as_str()), ("a", "c"));
    }

    #[test]
    fn unrolls_object_for_exprs_over_arrays() {
        let model = model(r#"trace "t" { metadata = { for i, x in ["a", "b"] : x => i } }"#).unwrap();

        let metadata = model.traces[0].fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem[0].key, "a");
        assert!(matches!(metadata.elem[0].value, Value::Num(Number::Int(0))));
        assert_eq!(metadata.elem[1].key, "b");
        assert!(matches!(metadata.elem[1].value, Value::Num(Number::Int(1))));
    }

    #[test]
    fn unrolls_for_exprs_into_typed_positions() {
        let model = model(r#"trace "t" { tags = [for x in ["a", "b"] : "t-${x}"] }"#).unwrap();

        // spliced tags stay templates of literal parts, joined during generation
        let tags: Vec<String> = model.traces[0]
            .fields
            .tags
            .iter()
            .map(|tag| {
                tag.parts
                    .iter()
                    .map(|part| match part {
                        Part::Lit(value) => value.as_str(),
                        Part::Ref(_) | Part::VarRef(_) | Part::Dynamic(_) => panic!("expected literal parts"),
                    })
                    .collect()
            })
            .collect();
        assert_eq!(tags, ["t-a", "t-b"]);
    }

    #[test]
    fn unrolls_for_exprs_keeping_dynamic_bodies() {
        let model = model(r#"trace "t" { input = [for x in [1, 2] : x + range(0, 1)] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert_eq!(array.elem.len(), 2);
        assert!(array.elem.iter().all(|value| matches!(value, ArrayElem::Item(Value::Binary { .. }))));
    }

    #[test]
    fn unrolls_nested_for_exprs_with_shadowing() {
        let model = model(r#"trace "t" { input = [for x in [[1, 2], [3]] : [for x in x : x * 10]] }"#).unwrap();

        let Some(Value::Array(outer)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        let ArrayElem::Item(first) = &outer.elem[0] else {
            panic!("expected an item");
        };
        let ArrayElem::Item(second) = &outer.elem[1] else {
            panic!("expected an item");
        };
        assert_eq!(ints(first), [10, 20]);
        assert_eq!(ints(second), [30]);
    }

    #[test]
    fn rejects_for_exprs_over_dynamic_or_scalar_collections() {
        let errors = model(r#"trace "t" { input = [for x in choice([1], [2]) : x] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ForCollectionMismatch {
                rule: spec::ids::FOR_COLLECTIONS,
                found: ExprType::Func,
            }
        );

        let errors = model(r#"trace "t" { input = [for x in 5 : x] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::ForCollectionMismatch {
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_for_filters_that_are_not_constant_booleans() {
        let errors = model(r#"trace "t" { input = [for x in [range(0, 1)] : x if x > 0] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonConstantForFilter {
                rule: spec::ids::STATIC_FOR,
                found: ExprType::Abstract,
            }
        );

        let errors = model(r#"trace "t" { input = [for x in [1] : x if 5] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantForFilter {
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_for_keys_that_are_not_constant_strings() {
        let errors = model(r#"trace "t" { metadata = { for i, x in [1] : i => x } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonConstantForKey {
                rule: spec::ids::STATIC_FOR,
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { metadata = { for x in [1] : "${trace.index}" => x } }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantForKey {
                found: ExprType::String,
                ..
            }
        ));
    }

    #[test]
    fn rejects_duplicate_keys_produced_by_unrolling() {
        let errors = model(r#"trace "t" { metadata = { for x in ["a", "a"] : x => 1 } }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::DuplicateObjectKey { key, .. } if key == "a"));
    }

    #[test]
    fn rejects_interpolating_non_scalar_bindings() {
        let errors = model(r#"trace "t" { input = [for x in [[1]] : "${x}"] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonScalarInterpolation {
                rule: spec::ids::SCALAR_INTERPOLATION,
                found: ExprType::Array,
            }
        );
    }

    #[test]
    fn splices_binding_templates_and_keeps_context_references() {
        let model = model(r#"trace "t" { input = [for x in ["q ${trace.index}"] : "${x}!"] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        let ArrayElem::Item(Value::Template(template)) = &array.elem[0] else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::TraceIndex)));
        assert!(matches!(&template.parts[2], Part::Lit(value) if value == "!"));
    }

    #[test]
    fn rejects_for_exprs_referencing_same_scope_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = [for x in var.a : x] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarNotInScope { .. }));
    }

    #[test]
    fn associates_operator_errors_with_spec_rules() {
        let errors = model(r#"trace "t" { input = 1 + "a" }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::OPERAND_TYPES).unwrap().summary)
        );

        let errors = model(r#"trace "t" { input = 1 / 0 }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::NONZERO_DIVISORS).unwrap().summary)
        );

        let errors = model(r#"trace "t" { input = 1 ? 2 : 3 }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::BOOLEAN_CONDITIONS).unwrap().summary)
        );
    }

    #[test]
    fn models_dynamic_blocks_with_optional_names() {
        let model = model(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();

        let children = &model.traces[0].children;
        let Child::Repeat(repeat) = &children[0] else {
            panic!("expected a repeat child");
        };
        assert_eq!(repeat.name.as_deref(), Some("turns"));
        assert!(matches!(&repeat.count, Value::VarRef(name) if name == "turns"));
        assert!(matches!(
            repeat.children.as_slice(),
            [Child::Span(Span {
                kind: SpanKind::Task,
                ..
            })]
        ));

        let Child::Choice(choice) = &children[1] else {
            panic!("expected a choice child");
        };
        assert_eq!(choice.name, None);
        assert_eq!(choice.children.len(), 2);

        let Child::Maybe(maybe) = &children[2] else {
            panic!("expected a maybe child");
        };
        assert!(matches!(maybe.chance, Value::Num(Number::Float(value)) if value == 0.25));
    }

    #[test]
    fn defaults_a_maybe_without_a_chance_to_a_coin_flip() {
        let model = model(r#"trace "t" { maybe { task "extra" {} } }"#).unwrap();

        let Child::Maybe(maybe) = &model.traces[0].children[0] else {
            panic!("expected a maybe child");
        };
        assert!(matches!(maybe.chance, Value::Num(Number::Float(value)) if value == 0.5));
    }

    #[test]
    fn rejects_dynamic_blocks_at_the_root_and_span_blocks_they_forbid() {
        let errors = model(r#"repeat { count = 1 task "turn" {} }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::REPEAT,
                parent: spec::Place::Root,
            }
        );

        let errors = model(r#"trace "t" { choice { trace "inner" {} task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TRACE,
                parent: spec::Place::Block { id: spec::ids::CHOICE },
            }
        );
    }

    #[test]
    fn rejects_invalid_dynamic_bodies() {
        let errors = model(r#"trace "t" { repeat { task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::MissingField {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
            }
        );

        let errors = model(r#"trace "t" { repeat { count = 1 count = 2 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateField {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
            }
        );

        // span fields don't leak into dynamic bodies
        let errors = model(r#"trace "t" { maybe { input = "hi" task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownField {
                block: spec::ids::MAYBE,
                keyword: "input".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_invalid_counts_and_chances() {
        let source = r#"trace "t" { repeat { count = "three" task "turn" {} } }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
                expected: &spec::ExprType::Number,
                found: ExprType::String,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "\"three\"");

        let errors = model(r#"trace "t" { repeat { count = 0 - 2 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NegativeRepeatCount {
                rule: spec::ids::REPEAT_COUNT,
            }
        );

        let errors = model(r#"trace "t" { repeat { count = 1.5 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerRepeatCount {
                rule: spec::ids::REPEAT_COUNT,
            }
        );

        let errors = model(r#"trace "t" { maybe { chance = 1.5 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ChanceOutOfRange {
                rule: spec::ids::MAYBE_CHANCE,
            }
        );
    }

    #[test]
    fn allows_zero_counts_and_dynamic_bounds() {
        let model = model(
            r#"
            trace "t" {
                repeat { count = 0 task "turn" {} }
                maybe { chance = range(0.0, 1.0) task "extra" {} }
            }
            "#,
        )
        .unwrap();

        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat child");
        };
        assert!(matches!(repeat.count, Value::Num(Number::Int(0))));
    }

    #[test]
    fn rejects_empty_dynamic_blocks() {
        let errors = model(r#"trace "t" { choice {} }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::EmptyDynamicBlock {
                rule: spec::ids::DYNAMIC_CHILDREN,
                block: spec::ids::CHOICE,
            }
        );
    }

    #[test]
    fn scopes_repeat_index_to_repeat_blocks() {
        let source = r#"trace "t" { input = "turn ${repeat.index}" }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::RepeatRefOutsideRepeat {
                rule: spec::ids::REPEAT_REFS,
                path: "repeat.index".into(),
            }
        );
        // the range points at the reference expression inside the hole
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "repeat.index");

        // valid inside a repeat, including through nested dynamic blocks
        let model = model(
            r#"
            trace "t" {
                repeat {
                    count = 2
                    maybe { task "turn" { input = "turn ${repeat.index}" } }
                }
            }
            "#,
        )
        .unwrap();
        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat child");
        };
        let Child::Maybe(maybe) = &repeat.children[0] else {
            panic!("expected a maybe child");
        };
        let Some(Value::Template(template)) = &as_span(&maybe.children[0]).fields.input else {
            panic!("expected a template input");
        };
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::RepeatIndex)));
    }

    #[test]
    fn resolves_block_refs_to_sibling_fields() {
        let model = model(
            r#"
            trace "t" {
                output = llm.chat.output.content
                llm "chat" { output = { content = "hi" } }
            }
            "#,
        )
        .unwrap();

        let Some(Value::BlockRef { ref_id, .. }) = &model.traces[0].fields.output else {
            panic!("expected a block ref output");
        };
        let resolved = &model.refs[ref_id.0 as usize];
        assert_eq!(resolved.up, 0);
        assert_eq!(resolved.steps.len(), 1);
        assert!(matches!(resolved.accessor, Accessor::Field(Field::Output)));
        // .content drills into the field's json
        assert!(matches!(&resolved.path[..], [Selection::Index(Value::Str(key))] if key == "content"));
    }

    #[test]
    fn resolves_self_and_trace_heads() {
        let model = model(
            r#"
            trace "t" {
                input = "q"
                llm "chat" {
                    input = trace.input
                    output = "a"
                    metrics = { exact = tokens(self.output) }
                }
            }
            "#,
        )
        .unwrap();

        let Child::Span(llm) = &model.traces[0].children[0] else {
            panic!("expected a span");
        };
        // trace.input climbs one scope, self.output stays put; neither descends
        let Some(Value::BlockRef { ref_id, .. }) = &llm.fields.input else {
            panic!("expected a block ref input");
        };
        let resolved = &model.refs[ref_id.0 as usize];
        assert_eq!((resolved.up, resolved.steps.len()), (1, 0));

        let Some(metrics) = &llm.fields.metrics else {
            panic!("expected metrics");
        };
        let Value::Func {
            func: Func::Tokens { value },
            ..
        } = &metrics.elem[0].value
        else {
            panic!("expected a tokens call");
        };
        let Value::BlockRef { ref_id, .. } = &**value else {
            panic!("expected a block ref argument");
        };
        let resolved = &model.refs[ref_id.0 as usize];
        assert_eq!((resolved.up, resolved.steps.len()), (0, 0));
        assert!(matches!(resolved.accessor, Accessor::Field(Field::Output)));
    }

    #[test]
    fn allows_sibling_metric_keys_to_reference_each_other() {
        let model = model(
            r#"
            trace "t" {
                llm "chat" {
                    metrics = {
                        prompt_tokens = round(lognormal(600, 0.4)),
                        completion_tokens = round(lognormal(90, 0.7)),
                        tokens = self.metrics.prompt_tokens + self.metrics.completion_tokens,
                    }
                }
            }
            "#,
        );
        assert!(model.is_ok());
    }

    #[test]
    fn rejects_metric_keys_referencing_themselves() {
        let errors = model(
            r#"
            trace "t" {
                llm "chat" { metrics = { tokens = self.metrics.tokens + 1 } }
            }
            "#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::CircularReference { .. }));
    }

    #[test]
    fn rejects_static_reference_cycles() {
        let errors = model(
            r#"
            trace "t" {
                task "a" { input = task.b.output output = task.a.input }
                task "b" { input = task.a.output output = task.b.input }
            }
            "#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::CircularReference { .. }));

        // cycles through a var are still cycles
        let errors = model(
            r#"
            trace "t" {
                task "a" {
                    vars { echo = task.a.output }
                    output = var.echo
                }
            }
            "#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::CircularReference { .. }));
    }

    #[test]
    fn resolves_duplicate_names_by_position() {
        // extension leaves dead intermediate records that must stay silent
        let model = model(
            r#"
            trace "t" {
                llm "chat" { output = "one" }
                llm "chat" { output = "two" }
                task "sum" { input = [llm.chat[0].output, llm["chat"][trace.index % 2].output] }
            }
            "#,
        );
        assert!(model.is_ok());
    }

    #[test]
    fn rejects_bad_block_refs_with_precise_diagnostics() {
        type Matcher = fn(&ErrorKind) -> bool;
        let cases: &[(&str, Matcher)] = &[
            // no llm anywhere in scope
            (r#"trace "t" { input = llm.chat.output task "x" {} }"#, |kind| {
                matches!(kind, ErrorKind::UnknownBlockRef { .. })
            }),
            // two siblings, no index
            (
                r#"trace "t" { input = task.x.output task "x" { output = 1 } task "x" { output = 2 } }"#,
                |kind| matches!(kind, ErrorKind::AmbiguousBlockRef { count: 2, .. }),
            ),
            // never reaches a field
            (r#"trace "t" { input = task.x task "x" { output = 1 } }"#, |kind| {
                matches!(kind, ErrorKind::IncompleteBlockRef { .. })
            }),
            // the target never sets the field
            (r#"trace "t" { input = task.x.output task "x" { input = 1 } }"#, |kind| {
                matches!(kind, ErrorKind::AbsentFieldRef { .. })
            }),
            // not a field or child kind
            (r#"trace "t" { input = task.x.wat task "x" { output = 1 } }"#, |kind| {
                matches!(kind, ErrorKind::InvalidRefSegment { .. })
            }),
            // constant position out of bounds
            (r#"trace "t" { input = task.x[2].output task "x" { output = 1 } }"#, |kind| {
                matches!(kind, ErrorKind::IndexOutOfBounds { .. })
            }),
        ];

        for (source, matches_kind) in cases {
            let errors = model(source).unwrap_err();
            assert!(matches_kind(errors[0].kind()), "unexpected error for {source}: {:?}", errors[0].kind());
        }
    }

    #[test]
    fn rejects_self_outside_a_block_and_refs_in_root_vars() {
        let errors = model(r#"vars { x = self.output } trace "t" { input = var.x }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::SelfOutsideBlock { .. }));

        let errors = model(r#"vars { x = llm.chat.output } trace "t" { input = var.x llm "chat" { output = 1 } }"#)
            .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::RootVarBlockRef { .. }));
    }

    #[test]
    fn keeps_structure_independent_of_references() {
        // count cannot reach a reference, directly or through a var
        let errors = model(
            r#"trace "t" { task "n" { output = 3 } repeat { count = task.n.output task "x" {} } }"#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::StructuralBlockRef { .. }));

        let errors = model(
            r#"
            trace "t" {
                task "n" { output = 3 }
                vars { n = task.n.output + 0 }
                repeat { count = var.n task "x" {} }
            }
            "#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::StructuralBlockRef { .. }));

        let errors = model(
            r#"trace "t" { task "n" { output = [1] } input = [for x in task.n.output : x] }"#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::ForBlockRefCollection { .. }));
    }

    #[test]
    fn models_block_refs_in_template_holes() {
        let model = model(
            r#"
            trace "t" {
                llm "chat" { output = { content = "hi" } }
                input = "assistant said ${llm.chat.output.content}"
            }
            "#,
        )
        .unwrap();

        let Some(Value::Template(template)) = &model.traces[0].fields.input else {
            panic!("expected a template input");
        };
        assert!(matches!(&template.parts[1], Part::Dynamic(Value::BlockRef { .. })));
    }

    #[test]
    fn resolves_repeat_collections_and_accessors() {
        let model = model(
            r#"
            trace "t" {
                repeat "rounds" {
                    count = 3
                    llm "chat" {
                        input = [...repeat.rounds[:repeat.index].llm.chat.output]
                        output = "r ${repeat.rounds.index} of ${repeat.rounds.count}"
                    }
                }
                metadata = { last = repeat.rounds[0].llm.chat.output }
            }
            "#,
        )
        .unwrap();

        // the metadata ref selects one iteration then descends
        let last = model
            .refs
            .iter()
            .find(|resolved| matches!(&resolved.steps[..], [Step::Child { .. }, Step::Iteration(_), Step::Child { .. }]));
        assert!(last.is_some(), "expected an iteration-selecting ref: {:?}", model.refs);

        // the history ref projects over an iteration slice
        let history = model
            .refs
            .iter()
            .find(|resolved| matches!(&resolved.steps[..], [Step::Child { .. }, Step::Iterations { .. }, Step::Child { .. }]));
        assert!(history.is_some(), "expected a projecting ref: {:?}", model.refs);

        // named accessors close without a field
        assert!(model.refs.iter().any(|resolved| matches!(resolved.accessor, Accessor::Index)));
        assert!(model.refs.iter().any(|resolved| matches!(resolved.accessor, Accessor::Count)));
    }

    #[test]
    fn rejects_bad_repeat_references() {
        // reserved names
        let errors = model(r#"trace "t" { repeat "index" { count = 1 task "x" {} } }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::ReservedRepeatName { .. }));

        // descending without selecting an iteration
        let errors = model(
            r#"trace "t" { repeat "r" { count = 1 llm "c" { output = 1 } } input = repeat.r.llm.c.output }"#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::RepeatIterationRequired { .. }));

        // index/count of a repeat that does not enclose the reference
        let errors = model(
            r#"trace "t" { repeat "r" { count = 1 task "x" {} } input = repeat.r.index }"#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::RepeatRefOutsideRepeat { .. }));
    }

    #[test]
    fn resolves_branch_accessors_and_crossings() {
        // crossing an unnamed dynamic block finds nothing
        let errors = model(
            r#"
            trace "t" {
                choice { task "a" { output = 1 } task "b" { output = 2 } }
                input = task.a.output
            }
            "#,
        )
        .unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::UnknownBlockRef { .. }));

        let modeled = model(
            r#"
            trace "t" {
                choice "outcome" {
                    task "resolved" { output = "ok" }
                    task "escalated" { output = "paged" }
                }
                maybe "retry" { chance = 0.5 task "again" { output = 1 } }
                metadata = {
                    picked = choice.outcome.chosen,
                    retried = maybe.retry.included,
                    resolved = choice.outcome.task.resolved.output != null,
                }
            }
            "#,
        )
        .unwrap();

        assert!(modeled.refs.iter().any(|resolved| matches!(resolved.accessor, Accessor::Chosen)));
        assert!(modeled.refs.iter().any(|resolved| matches!(resolved.accessor, Accessor::Included)));
    }

    #[test]
    fn models_dynamic_spreads_of_reference_projections() {
        let model = model(
            r#"
            trace "t" {
                repeat "r" { count = 2 llm "c" { output = { role = "assistant" } } }
                input = [{ role = "user" }, ...repeat.r[:].llm.c.output]
            }
            "#,
        )
        .unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array input");
        };
        assert!(matches!(&array.elem[0], ArrayElem::Item(Value::Object(_))));
        assert!(matches!(&array.elem[1], ArrayElem::Spread(Value::BlockRef { .. })));
    }

    #[test]
    fn rejects_statically_non_scalar_template_holes() {
        // a constant slice is an array no matter what feeds it
        let errors = model(r#"vars { xs = [1, 2, 3] } trace "t" { input = "${var.xs[0:2]}" }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonScalarInterpolation {
                found: ExprType::Array,
                ..
            }
        ));
    }
}
