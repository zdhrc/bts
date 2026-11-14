use crate::{
    Diag, DiagPhase, Diags, SrcRange, ast,
    expr::{self, ReferenceValue},
    model::*,
};
use std::collections::{BTreeSet, HashMap, HashSet};

type Result<T> = std::result::Result<T, Diags>;

fn error(range: SrcRange, what: impl Into<String>) -> Diags {
    vec![Diag {
        when: DiagPhase::Validation,
        what: what.into(),
        r#where: range,
    }]
}

#[derive(Clone)]
enum Anchor {
    Node(NodeId),
    Reference(ast::Expr, Option<ElementId>),
}
#[derive(Clone)]
struct RawEdge {
    source: Anchor,
    target: Anchor,
}
#[derive(Clone)]
struct RawFork {
    source: Anchor,
}
#[derive(Clone)]
struct RawJunction {
    inputs: Vec<ast::Expr>,
    target: Anchor,
}
#[derive(Clone)]
struct RawPath {
    origin: RawOrigin,
    steps: Vec<PathStep>,
    range: SrcRange,
}
#[derive(Clone)]
enum RawOrigin {
    Node(Anchor),
    Selector(ForkId, Anchor),
    Junction(JunctionId),
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Slot {
    owner: ElementId,
    property: bool,
    path: Vec<String>,
}
#[derive(Clone)]
enum Cursor {
    Element(ElementId),
    Properties(ElementId),
    Slot(Slot),
    Value(ReferenceValue),
    Context(ElementId, Vec<String>),
}

struct Modeler {
    model: Model,
    declarations: HashMap<String, SrcRange>,
    fields: HashMap<(ElementId, bool), Vec<ast::Attr>>,
    raw_edges: Vec<RawEdge>,
    raw_forks: Vec<RawFork>,
    raw_junctions: Vec<RawJunction>,
    continuations: Vec<Vec<RawPath>>,
    edge_state: Vec<u8>,
    fork_state: Vec<u8>,
    junction_state: Vec<u8>,
    slots: HashMap<Slot, BindingId>,
    visiting: HashSet<Slot>,
    errors: Diags,
}

impl Modeler {
    fn new() -> Self {
        Self {
            model: Model::default(),
            declarations: HashMap::new(),
            fields: HashMap::new(),
            raw_edges: Vec::new(),
            raw_forks: Vec::new(),
            raw_junctions: Vec::new(),
            continuations: Vec::new(),
            edge_state: Vec::new(),
            fork_state: Vec::new(),
            junction_state: Vec::new(),
            slots: HashMap::new(),
            visiting: HashSet::new(),
            errors: Vec::new(),
        }
    }

    fn name(&mut self, name: &Option<String>, id: ElementId, range: SrcRange) {
        if let Some(name) = name {
            if matches!(
                name.as_str(),
                "self" | "source" | "target" | "context" | "true" | "false" | "null"
            ) {
                self.errors.extend(error(range, format!("reserved graph name `{name}`")));
                return;
            }
            if let Some(previous) = self.declarations.get(name) {
                self.errors.extend(error(range, format!("duplicate graph name `{name}`")));
                self.errors
                    .extend(error(*previous, format!("previous declaration of `{name}`")));
            } else {
                self.declarations.insert(name.clone(), range);
                self.model.names.insert(name.clone(), id);
            }
        }
    }

    fn fields(&mut self, owner: ElementId, property: bool, fields: &[ast::Attr]) {
        let mut seen = HashMap::new();
        for field in fields {
            if let Some(previous) = seen.insert(field.key.clone(), field.range) {
                self.errors
                    .extend(error(field.range, format!("duplicate field `{}`", field.key)));
                self.errors.extend(error(previous, format!("previous field `{}`", field.key)));
            }
            if !property && field.key == "props" {
                self.errors
                    .extend(error(field.range, "`props` is reserved for graph element properties"));
            }
        }
        self.fields.insert((owner, property), fields.to_vec());
    }

    fn node(&mut self, node: &ast::GraphNode, owner: Option<ElementId>) -> Anchor {
        match node {
            ast::GraphNode::Reference(expr) => Anchor::Reference(expr.clone(), owner),
            ast::GraphNode::Declaration(node) => {
                let id = NodeId(self.model.nodes.len());
                let element = ElementId::Node(id);
                self.model.nodes.push(Node {
                    name: node.header.name.clone(),
                    kind: node.header.kind.clone(),
                    props: Vec::new(),
                    fields: Vec::new(),
                    range: node.range,
                });
                self.name(
                    &node.header.name,
                    element,
                    node.header.name_range.unwrap_or(node.header.range),
                );
                self.fields(element, true, node.header.props.as_deref().unwrap_or(&[]));
                self.fields(element, false, node.body.as_deref().unwrap_or(&[]));
                Anchor::Node(id)
            }
        }
    }

    fn edge(&mut self, step: &ast::GraphEdgeStep, source: Anchor, fork: Option<ForkId>) -> Vec<EdgeId> {
        let target = self.node(&step.target, fork.map(ElementId::Fork));
        step.edges
            .iter()
            .map(|header| {
                let id = EdgeId(self.model.edges.len());
                let element = ElementId::Edge(id);
                self.model.edges.push(Edge {
                    name: header.name.clone(),
                    kind: header.kind.clone(),
                    source: NodeId(usize::MAX),
                    target: NodeId(usize::MAX),
                    props: Vec::new(),
                    cycle: step.cycle,
                    fork,
                    range: header.range,
                });
                self.raw_edges.push(RawEdge {
                    source: source.clone(),
                    target: target.clone(),
                });
                self.edge_state.push(0);
                self.name(&header.name, element, header.name_range.unwrap_or(header.range));
                self.fields(element, true, header.props.as_deref().unwrap_or(&[]));
                id
            })
            .collect()
    }

    fn junction(&mut self, junction: &ast::GraphJunction, fork: Option<ForkId>) -> JunctionId {
        let id = JunctionId(self.model.junctions.len());
        let target = self.node(&junction.target, fork.map(ElementId::Fork));
        self.model.junctions.push(Junction {
            kind: junction.kind.clone(),
            inputs: Vec::new(),
            props: Vec::new(),
            target: NodeId(usize::MAX),
            fork,
            range: junction.range,
        });
        self.fields(ElementId::Junction(id), true, junction.props.as_deref().unwrap_or(&[]));
        self.raw_junctions.push(RawJunction {
            inputs: junction.inputs.clone(),
            target,
        });
        self.junction_state.push(0);
        id
    }

    fn path(&mut self, path: &ast::GraphPath, parent: Option<ForkId>) -> RawPath {
        let (origin, mut current) = match &path.start {
            ast::GraphOrigin::Node(node) => {
                let anchor = self.node(node, parent.map(ElementId::Fork));
                (RawOrigin::Node(anchor.clone()), anchor)
            }
            ast::GraphOrigin::Selector(expr) => {
                let fork = parent.expect("parser attaches selectors to forks");
                let anchor = Anchor::Reference(expr.clone(), Some(ElementId::Fork(fork)));
                (RawOrigin::Selector(fork, anchor.clone()), anchor)
            }
            ast::GraphOrigin::Junction(junction) => {
                let id = self.junction(junction, parent);
                (RawOrigin::Junction(id), self.raw_junctions[id.0].target.clone())
            }
        };
        let mut steps = Vec::new();
        for step in &path.steps {
            match step {
                ast::GraphStep::Edge(step) => {
                    let edges = self.edge(step, current.clone(), None);
                    current = self.raw_edges[edges[0].0].target.clone();
                    steps.push(PathStep::Edges(edges));
                }
                ast::GraphStep::Fork(fork) => {
                    let id = ForkId(self.model.forks.len());
                    let element = ElementId::Fork(id);
                    self.model.forks.push(Fork {
                        name: fork.header.name.clone(),
                        kind: fork.header.kind.clone(),
                        source: NodeId(usize::MAX),
                        props: Vec::new(),
                        branches: Vec::new(),
                        continuations: Vec::new(),
                        range: fork.range,
                    });
                    self.raw_forks.push(RawFork { source: current.clone() });
                    self.fork_state.push(0);
                    self.continuations.push(Vec::new());
                    self.name(
                        &fork.header.name,
                        element,
                        fork.header.name_range.unwrap_or(fork.header.range),
                    );
                    self.fields(element, true, fork.header.props.as_deref().unwrap_or(&[]));
                    let branches = fork
                        .entries
                        .iter()
                        .map(|entry| self.edge(entry, current.clone(), Some(id)))
                        .collect();
                    self.model.forks[id.0].branches = branches;
                    let paths = fork.continuations.iter().map(|path| self.path(path, Some(id))).collect();
                    self.continuations[id.0] = paths;
                    steps.push(PathStep::Fork(id));
                }
            }
        }
        RawPath {
            origin,
            steps,
            range: path.range,
        }
    }

    fn endpoint(&mut self, anchor: &Anchor) -> Result<NodeId> {
        match anchor {
            Anchor::Node(node) => Ok(*node),
            Anchor::Reference(expr, owner) => {
                let cursor = self.cursor(expr, *owner)?;
                match self.element(cursor, expr.range)? {
                    ElementId::Node(node) => Ok(node),
                    _ => Err(error(expr.range, "path endpoint must reference a node")),
                }
            }
        }
    }

    fn resolve_edge(&mut self, id: EdgeId) -> Result<()> {
        if self.edge_state[id.0] == 2 {
            return Ok(());
        }
        if self.edge_state[id.0] == 1 {
            return Err(error(self.model.edges[id.0].range, "circular static endpoint dependency"));
        }
        self.edge_state[id.0] = 1;
        let raw = self.raw_edges[id.0].clone();
        let source = self.endpoint(&raw.source)?;
        let target = self.endpoint(&raw.target)?;
        self.model.edges[id.0].source = source;
        self.model.edges[id.0].target = target;
        self.edge_state[id.0] = 2;
        Ok(())
    }

    fn resolve_fork(&mut self, id: ForkId) -> Result<()> {
        if self.fork_state[id.0] == 2 {
            return Ok(());
        }
        if self.fork_state[id.0] == 1 {
            return Err(error(self.model.forks[id.0].range, "circular static fork dependency"));
        }
        self.fork_state[id.0] = 1;
        let raw = self.raw_forks[id.0].clone();
        self.model.forks[id.0].source = self.endpoint(&raw.source)?;
        for branch in self.model.forks[id.0].branches.clone() {
            for edge in branch {
                self.resolve_edge(edge)?
            }
        }
        self.fork_state[id.0] = 2;
        Ok(())
    }

    fn resolve_junction(&mut self, id: JunctionId) -> Result<()> {
        if self.junction_state[id.0] == 2 {
            return Ok(());
        }
        if self.junction_state[id.0] == 1 {
            return Err(error(self.model.junctions[id.0].range, "circular static junction dependency"));
        }
        self.junction_state[id.0] = 1;
        let raw = self.raw_junctions[id.0].clone();
        let owner = Some(ElementId::Junction(id));
        let mut inputs = Vec::new();
        for expr in &raw.inputs {
            let cursor = self.cursor(expr, owner)?;
            let ElementId::Edge(edge) = self.element(cursor, expr.range)? else {
                return Err(error(expr.range, "junction inputs must reference edges"));
            };
            self.resolve_edge(edge)?;
            if let Some(fork) = self.model.junctions[id.0].fork
                && self.model.edges[edge.0].fork != Some(fork)
            {
                return Err(error(expr.range, "junction input does not belong to the attached fork"));
            }
            inputs.push(edge);
        }
        self.model.junctions[id.0].inputs = inputs;
        self.model.junctions[id.0].target = self.endpoint(&raw.target)?;
        self.junction_state[id.0] = 2;
        Ok(())
    }

    fn lower_path(&mut self, path: RawPath) -> Result<GraphPath> {
        let origin = match path.origin {
            RawOrigin::Node(anchor) => PathOrigin::Node(self.endpoint(&anchor)?),
            RawOrigin::Selector(fork, anchor) => {
                self.resolve_fork(fork)?;
                let target = self.endpoint(&anchor)?;
                if !self.model.forks[fork.0]
                    .branches
                    .iter()
                    .flatten()
                    .any(|edge| self.model.edges[edge.0].target == target)
                {
                    return Err(error(path.range, "selector target does not belong to the attached fork"));
                }
                PathOrigin::Selector { fork, target }
            }
            RawOrigin::Junction(id) => {
                self.resolve_junction(id)?;
                PathOrigin::Junction(id)
            }
        };
        for step in &path.steps {
            match step {
                PathStep::Edges(edges) => {
                    for edge in edges {
                        self.resolve_edge(*edge)?
                    }
                }
                PathStep::Fork(fork) => {
                    self.resolve_fork(*fork)?;
                    let continuations = self.continuations[fork.0]
                        .clone()
                        .into_iter()
                        .map(|path| self.lower_path(path))
                        .collect::<Result<Vec<_>>>()?;
                    self.model.forks[fork.0].continuations = continuations;
                }
            }
        }
        Ok(GraphPath {
            origin,
            steps: path.steps,
            range: path.range,
        })
    }

    fn graph_value(&mut self, target: ElementId, range: SrcRange) -> ReferenceValue {
        let id = RefId(self.model.refs.len());
        self.model.refs.push(ResolvedRef { target, range });
        ReferenceValue {
            value: Value::GraphRef { ref_id: id, range },
            bindings: Vec::new(),
        }
    }

    fn raw_slot(&self, slot: &Slot, range: SrcRange) -> Result<ast::Expr> {
        let first = slot.path.first().ok_or_else(|| error(range, "expected field name"))?;
        let field = self
            .fields
            .get(&(slot.owner, slot.property))
            .and_then(|fields| fields.iter().find(|field| &field.key == first))
            .ok_or_else(|| {
                error(
                    range,
                    format!("unknown {} `{first}`", if slot.property { "property" } else { "field" }),
                )
            })?;
        let mut expr = field.value.clone();
        for key in slot.path.iter().skip(1) {
            let ast::ExprKind::Object(items) = &expr.kind else {
                return Err(error(range, format!("field path cannot select `{key}`")));
            };
            if items.iter().any(|item| matches!(item, ast::ObjectItem::Spread(_))) {
                return Err(error(range, "object spreads require evaluation of the containing field"));
            }
            expr = items
                .iter()
                .find_map(|item| match item {
                    ast::ObjectItem::Attr(field) if &field.key == key => Some(field.value.clone()),
                    _ => None,
                })
                .ok_or_else(|| error(range, format!("unknown object key `{key}`")))?;
        }
        Ok(expr)
    }

    fn binding(&mut self, slot: Slot, range: SrcRange) -> Result<BindingId> {
        if self.visiting.contains(&slot) {
            return Err(error(
                range,
                format!("circular field dependency through `{}`", slot.path.join(".")),
            ));
        }
        if let Some(id) = self.slots.get(&slot) {
            return Ok(*id);
        }
        let expr = self.raw_slot(&slot, range)?;
        let id = BindingId(self.model.bindings.len());
        self.model.bindings.push(Binding {
            owner: slot.owner,
            property: slot.property,
            path: slot.path.clone(),
            value: Value::Null,
            range: expr.range,
        });
        self.slots.insert(slot.clone(), id);
        self.visiting.insert(slot.clone());
        let result = self.lower_binding_expr(expr, &slot);
        self.visiting.remove(&slot);
        match result {
            Ok(value) => {
                self.model.bindings[id.0].value = value;
                Ok(id)
            }
            Err(errors) => {
                self.slots.remove(&slot);
                Err(errors)
            }
        }
    }

    fn lower_binding_expr(&mut self, mut expr: ast::Expr, slot: &Slot) -> Result<Value> {
        if let ast::ExprKind::Object(items) = &mut expr.kind
            && items.iter().all(|item| matches!(item, ast::ObjectItem::Attr(_)))
        {
            // each explicit object field has one binding even when read through a sibling
            for item in items {
                let ast::ObjectItem::Attr(field) = item else { unreachable!() };
                field.value.kind = ast::ExprKind::Ref {
                    path: vec![field.key.clone()],
                };
            }
            return expr::lower(expr, &mut |expr| {
                let ast::ExprKind::Ref { path } = expr.kind else {
                    unreachable!()
                };
                let mut child = slot.clone();
                child.path.extend(path);
                self.finish(Cursor::Slot(child), expr.range)
            });
        }
        expr::lower(expr, &mut |expr| self.resolve_value(expr, Some(slot.owner)))
    }

    fn finish(&mut self, cursor: Cursor, range: SrcRange) -> Result<ReferenceValue> {
        match cursor {
            Cursor::Element(element) => Ok(self.graph_value(element, range)),
            Cursor::Slot(slot) => {
                let id = self.binding(slot, range)?;
                let value = self.model.bindings[id.0].value.clone();
                if expr::static_value(&value) {
                    Ok(ReferenceValue {
                        value,
                        bindings: Vec::new(),
                    })
                } else {
                    Ok(self.reference_value(Value::BindingRef(id)))
                }
            }
            Cursor::Value(value) => Ok(value),
            Cursor::Context(owner, path) => Ok(ReferenceValue {
                value: Value::Context { owner, path },
                bindings: Vec::new(),
            }),
            Cursor::Properties(element) => {
                let fields = self.fields.get(&(element, true)).cloned().unwrap_or_default();
                let mut values = Vec::new();
                for field in fields {
                    let value = self
                        .finish(
                            Cursor::Slot(Slot {
                                owner: element,
                                property: true,
                                path: vec![field.key.clone()],
                            }),
                            field.range,
                        )?
                        .value;
                    values.push(ObjectField { key: field.key, value });
                }
                Ok(self.reference_value(Value::Object(Object { elem: values })))
            }
        }
    }

    fn element(&mut self, cursor: Cursor, range: SrcRange) -> Result<ElementId> {
        if let Cursor::Element(element) = cursor {
            return Ok(element);
        }
        let mut value = self.finish(cursor, range)?.value;
        let mut visited = HashSet::new();
        loop {
            match value {
                Value::GraphRef { ref_id, .. } => return Ok(self.model.refs[ref_id.0].target),
                Value::BindingRef(id) if visited.insert(id) => value = self.model.bindings[id.0].value.clone(),
                _ => return Err(error(range, "expected a statically resolved graph reference")),
            }
        }
    }

    fn member(&mut self, cursor: Cursor, name: &str, range: SrcRange) -> Result<Cursor> {
        match cursor {
            Cursor::Element(element) if name == "props" => Ok(Cursor::Properties(element)),
            Cursor::Element(ElementId::Node(node)) => Ok(Cursor::Slot(Slot {
                owner: ElementId::Node(node),
                property: false,
                path: vec![name.to_owned()],
            })),
            Cursor::Properties(owner) => Ok(Cursor::Slot(Slot {
                owner,
                property: true,
                path: vec![name.to_owned()],
            })),
            Cursor::Context(owner, mut path) => {
                path.push(name.to_owned());
                Ok(Cursor::Context(owner, path))
            }
            Cursor::Element(ElementId::Edge(edge)) => match name {
                "source" => {
                    let source = self.raw_edges[edge.0].source.clone();
                    Ok(Cursor::Element(ElementId::Node(self.endpoint(&source)?)))
                }
                "target" => {
                    self.resolve_edge(edge)?;
                    Ok(Cursor::Element(ElementId::Node(self.model.edges[edge.0].target)))
                }
                "scope" => Ok(Cursor::Context(ElementId::Edge(edge), Vec::new())),
                _ => Err(error(range, format!("unknown edge accessor `{name}`"))),
            },
            Cursor::Element(ElementId::Fork(fork)) => match name {
                "source" => {
                    let source = self.raw_forks[fork.0].source.clone();
                    Ok(Cursor::Element(ElementId::Node(self.endpoint(&source)?)))
                }
                "paths" => {
                    self.resolve_fork(fork)?;
                    let mut paths = Vec::new();
                    for edges in self.model.forks[fork.0].branches.clone() {
                        let elem = edges
                            .into_iter()
                            .map(|edge| ArrayElem::Item(self.graph_value(ElementId::Edge(edge), range).value))
                            .collect();
                        paths.push(ArrayElem::Item(Value::Array(Array { elem })));
                    }
                    Ok(Cursor::Value(ReferenceValue {
                        value: Value::Array(Array { elem: paths }),
                        bindings: Vec::new(),
                    }))
                }
                _ => Err(error(range, format!("unknown fork accessor `{name}`"))),
            },
            Cursor::Slot(slot) => {
                let mut child = slot.clone();
                child.path.push(name.to_owned());
                if self.raw_slot(&child, range).is_ok() {
                    return Ok(Cursor::Slot(child));
                }
                // graph-valued fields keep the referenced identity when followed
                let value = self.finish(Cursor::Slot(slot.clone()), range)?;
                if let Value::GraphRef { ref_id, .. } = value.value {
                    return self.member(Cursor::Element(self.model.refs[ref_id.0].target), name, range);
                }
                Ok(Cursor::Value(expr::index(value, Value::Str(name.to_owned()), range)?))
            }
            Cursor::Value(value) => {
                if let Value::GraphRef { ref_id, .. } = value.value {
                    self.member(Cursor::Element(self.model.refs[ref_id.0].target), name, range)
                } else {
                    Ok(Cursor::Value(expr::index(value, Value::Str(name.to_owned()), range)?))
                }
            }
            Cursor::Element(ElementId::Junction(_)) => Err(error(range, "junctions have no value accessors")),
        }
    }

    fn cursor(&mut self, expr: &ast::Expr, owner: Option<ElementId>) -> Result<Cursor> {
        match &expr.kind {
            ast::ExprKind::Ref { path } => {
                let name = path.first().ok_or_else(|| error(expr.range, "empty reference"))?;
                let mut cursor = match name.as_str() {
                    "self" => match owner {
                        Some(ElementId::Node(node)) => Cursor::Element(ElementId::Node(node)),
                        _ => return Err(error(expr.range, "self requires a node-owned expression")),
                    },
                    "context" => Cursor::Context(
                        owner.ok_or_else(|| error(expr.range, "context requires an owning graph element"))?,
                        Vec::new(),
                    ),
                    "source" | "target" => {
                        let element = owner.ok_or_else(|| error(expr.range, "endpoint access requires an edge or fork"))?;
                        if !matches!(element, ElementId::Edge(_) | ElementId::Fork(_)) {
                            return Err(error(expr.range, "endpoint access requires an edge or fork"));
                        }
                        self.member(Cursor::Element(element), name, expr.range)?
                    }
                    _ => Cursor::Element(
                        *self
                            .model
                            .names
                            .get(name)
                            .ok_or_else(|| error(expr.range, format!("unresolved graph name `{name}`")))?,
                    ),
                };
                for name in path.iter().skip(1) {
                    cursor = self.member(cursor, name, expr.range)?
                }
                Ok(cursor)
            }
            ast::ExprKind::GraphRef { kind, target } => {
                let cursor = self.cursor(target, owner)?;
                let element = self.element(cursor, expr.range)?;
                let valid = match kind {
                    ast::GraphReferenceKind::Node => matches!(element, ElementId::Node(_)),
                    ast::GraphReferenceKind::Edge => matches!(element, ElementId::Edge(_) | ElementId::Fork(_)),
                };
                if !valid {
                    return Err(error(expr.range, "reference boundary does not match the graph element kind"));
                }
                Ok(Cursor::Element(element))
            }
            ast::ExprKind::GraphMember { target, name } => {
                let cursor = self.cursor(target, owner)?;
                let ElementId::Fork(fork) = self.element(cursor, expr.range)? else {
                    return Err(error(expr.range, "qualified edge lookup requires a fork"));
                };
                let Some(ElementId::Edge(edge)) = self.model.names.get(name).copied() else {
                    return Err(error(expr.range, format!("unresolved edge `{name}`")));
                };
                if self.model.edges[edge.0].fork != Some(fork) {
                    return Err(error(expr.range, "qualified edge is not a member of that fork"));
                }
                Ok(Cursor::Element(ElementId::Edge(edge)))
            }
            ast::ExprKind::PatternRef(pattern) => Ok(Cursor::Element(self.pattern(pattern, owner)?)),
            ast::ExprKind::Index { target, index } => {
                let cursor = self.cursor(target, owner)?;
                let index = expr::lower((**index).clone(), &mut |expr| self.resolve_value(expr, owner))?;
                if let Value::Str(name) = &index {
                    return self.member(cursor, name, expr.range);
                }
                let mut value = self.finish(cursor, expr.range)?;
                value.bindings.extend(self.reference_value(index.clone()).bindings);
                Ok(Cursor::Value(expr::index(value, index, expr.range)?))
            }
            ast::ExprKind::Slice { target, start, end } => {
                let cursor = self.cursor(target, owner)?;
                let value = self.finish(cursor, expr.range)?;
                let start = start
                    .as_ref()
                    .map(|expr| expr::lower((**expr).clone(), &mut |expr| self.resolve_value(expr, owner)))
                    .transpose()?;
                let end = end
                    .as_ref()
                    .map(|expr| expr::lower((**expr).clone(), &mut |expr| self.resolve_value(expr, owner)))
                    .transpose()?;
                let mut value = value;
                for bound in start.iter().chain(end.iter()) {
                    value.bindings.extend(self.reference_value(bound.clone()).bindings);
                }
                Ok(Cursor::Value(expr::slice(value, start, end, expr.range)?))
            }
            _ => {
                let value = expr::lower(expr.clone(), &mut |expr| self.resolve_value(expr, owner))?;
                Ok(Cursor::Value(self.reference_value(value)))
            }
        }
    }

    fn resolve_value(&mut self, expr: ast::Expr, owner: Option<ElementId>) -> Result<ReferenceValue> {
        let cursor = self.cursor(&expr, owner)?;
        self.finish(cursor, expr.range)
    }

    fn matches_constraint(
        &mut self,
        element: ElementId,
        constraint: &ast::GraphPatternConstraint,
        owner: Option<ElementId>,
    ) -> Result<bool> {
        let kind = match element {
            ElementId::Node(id) => &self.model.nodes[id.0].kind,
            ElementId::Edge(id) => &self.model.edges[id.0].kind,
            _ => return Ok(false),
        };
        if kind != &constraint.kind {
            return Ok(false);
        }
        for prop in constraint.props.as_deref().unwrap_or(&[]) {
            let slot = Slot {
                owner: element,
                property: true,
                path: vec![prop.key.clone()],
            };
            if self.raw_slot(&slot, prop.range).is_err() {
                return Ok(false);
            }
            let id = self.binding(slot, prop.range)?;
            let actual = self.model.bindings[id.0].value.clone();
            let expected = expr::lower(prop.value.clone(), &mut |expr| self.resolve_value(expr, owner))?;
            if !expr::constant(&actual) || !expr::constant(&expected) {
                return Err(error(
                    prop.range,
                    "pattern property constraints must be statically comparable",
                ));
            }
            if !same_value(&actual, &expected) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn matches_node(&mut self, node: NodeId, pattern: &ast::GraphPatternNode, owner: Option<ElementId>) -> Result<bool> {
        match pattern {
            ast::GraphPatternNode::Reference(expr) => {
                let cursor = self.cursor(expr, owner)?;
                Ok(self.element(cursor, expr.range)? == ElementId::Node(node))
            }
            ast::GraphPatternNode::Constraint(constraint) => self.matches_constraint(ElementId::Node(node), constraint, owner),
        }
    }

    fn pattern(&mut self, pattern: &ast::GraphPattern, owner: Option<ElementId>) -> Result<ElementId> {
        let mut paths = Vec::new();
        for i in 0..self.model.nodes.len() {
            let node = NodeId(i);
            if self.matches_node(node, &pattern.start, owner)? {
                let selected = matches!(&pattern.start,ast::GraphPatternNode::Constraint(c) if c.selected)
                    .then_some(ElementId::Node(node));
                paths.push((node, selected));
            }
        }
        for step in &pattern.steps {
            let mut next = BTreeSet::new();
            for (source, selected) in paths {
                let mut targets: BTreeSet<NodeId> = BTreeSet::new();
                for i in 0..self.model.edges.len() {
                    let edge = &self.model.edges[i];
                    if edge.cycle != step.cycle || !step.edges.iter().any(|constraint| constraint.kind == edge.kind) {
                        continue;
                    }
                    let anchor = self.raw_edges[i].source.clone();
                    if self.endpoint(&anchor)? != source {
                        continue;
                    }
                    self.resolve_edge(EdgeId(i))?;
                    targets.insert(self.model.edges[i].target);
                }
                for target in targets {
                    if !self.matches_node(target, &step.target, owner)? {
                        continue;
                    }
                    let selected = if matches!(&step.target,ast::GraphPatternNode::Constraint(c) if c.selected) {
                        Some(ElementId::Node(target))
                    } else {
                        selected
                    };
                    let mut candidates = Vec::new();
                    for constraint in &step.edges {
                        let mut matched = Vec::new();
                        for i in 0..self.model.edges.len() {
                            let edge = &self.model.edges[i];
                            if edge.source == source
                                && edge.target == target
                                && edge.cycle == step.cycle
                                && self.matches_constraint(ElementId::Edge(EdgeId(i)), constraint, owner)?
                            {
                                matched.push(EdgeId(i));
                            }
                        }
                        candidates.push(matched);
                    }
                    if let Some(index) = step.edges.iter().position(|constraint| constraint.selected) {
                        for edge in &candidates[index] {
                            if distinct_edge_assignment(&candidates, Some((index, *edge))) {
                                next.insert((target, Some(ElementId::Edge(*edge))));
                            }
                        }
                    } else if distinct_edge_assignment(&candidates, None) {
                        next.insert((target, selected));
                    }
                }
            }
            paths = next.into_iter().collect();
        }
        let selected: BTreeSet<_> = paths.into_iter().filter_map(|(_, selected)| selected).collect();
        if selected.len() != 1 {
            return Err(error(
                pattern.range,
                format!(
                    "pattern reference selects {} distinct graph elements, expected one",
                    selected.len()
                ),
            ));
        }
        Ok(*selected.iter().next().expect("one distinct element"))
    }

    fn lower_fields(&mut self) -> Result<()> {
        let owners: Vec<_> = (0..self.model.nodes.len())
            .map(|i| ElementId::Node(NodeId(i)))
            .chain((0..self.model.edges.len()).map(|i| ElementId::Edge(EdgeId(i))))
            .chain((0..self.model.forks.len()).map(|i| ElementId::Fork(ForkId(i))))
            .chain((0..self.model.junctions.len()).map(|i| ElementId::Junction(JunctionId(i))))
            .collect();
        let mut errors = Vec::new();
        for owner in owners {
            for property in [true, false] {
                let mut ids = Vec::new();
                for field in self.fields.get(&(owner, property)).cloned().unwrap_or_default() {
                    match self.binding(
                        Slot {
                            owner,
                            property,
                            path: vec![field.key],
                        },
                        field.range,
                    ) {
                        Ok(id) => ids.push(id),
                        Err(diags) => errors.extend(diags),
                    }
                }
                match owner {
                    ElementId::Node(id) => {
                        if property {
                            self.model.nodes[id.0].props = ids
                        } else {
                            self.model.nodes[id.0].fields = ids
                        }
                    }
                    ElementId::Edge(id) if property => self.model.edges[id.0].props = ids,
                    ElementId::Fork(id) if property => self.model.forks[id.0].props = ids,
                    ElementId::Junction(id) if property => self.model.junctions[id.0].props = ids,
                    _ => {}
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    fn property(&self, owner: ElementId, name: &str) -> Option<BindingId> {
        self.slots
            .get(&Slot {
                owner,
                property: true,
                path: vec![name.to_owned()],
            })
            .copied()
    }

    fn reference_value(&self, value: Value) -> ReferenceValue {
        let mut pending = expr::dependencies(&value);
        let mut seen = HashSet::new();
        let mut bindings = Vec::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let definition = self.model.bindings[id.0].value.clone();
            pending.extend(expr::dependencies(&definition));
            bindings.push((id, definition));
        }
        ReferenceValue { value, bindings }
    }

    fn validate_primitives(&self) -> Result<()> {
        let mut errors = Vec::new();
        for (i, junction) in self.model.junctions.iter().enumerate() {
            if matches!(junction.kind.as_str(), "accumulate" | "merge" | "first" | "quorum") {
                let distinct: HashSet<_> = junction.inputs.iter().copied().collect();
                if distinct.len() != junction.inputs.len() {
                    errors.extend(error(
                        junction.range,
                        "built-in junction inputs must reference distinct edges",
                    ));
                }
            }
            if junction.kind == "quorum" {
                match self.property(ElementId::Junction(JunctionId(i)), "count") {
                    Some(id) => {
                        let binding = &self.model.bindings[id.0];
                        if !matches!(&binding.value, Value::Num(Number::Int(count)) if *count >= 1 && usize::try_from(*count).is_ok_and(|count| count <= junction.inputs.len()))
                        {
                            errors.extend(error(
                                binding.range,
                                "quorum count must be a constant integer from one through the number of inputs",
                            ));
                        }
                    }
                    None => errors.extend(error(junction.range, "quorum junction requires a count property")),
                }
            }
        }
        for (i, node) in self.model.nodes.iter().enumerate() {
            if node.kind == "vars"
                && let Some(id) = self.property(ElementId::Node(NodeId(i)), "evaluate")
            {
                let binding = &self.model.bindings[id.0];
                if !matches!(&binding.value,Value::Str(value) if value=="lazy"||value=="eager") {
                    errors.extend(error(binding.range, "vars evaluate must be `lazy` or `eager`"));
                }
            }
        }
        for i in 0..self.model.edges.len() {
            if let Some(id) = self.property(ElementId::Edge(EdgeId(i)), "when") {
                let binding = &self.model.bindings[id.0];
                if let Err(diags) = expr::boolean(self.reference_value(binding.value.clone()), binding.range) {
                    errors.extend(diags)
                }
            }
        }
        for fork in &self.model.forks {
            if fork.kind == "parallel" {
                for edge in fork.branches.iter().flatten() {
                    if let Some(id) = self.property(ElementId::Edge(*edge), "when") {
                        errors.extend(error(
                            self.model.bindings[id.0].range,
                            "parallel fork branches cannot have guards",
                        ));
                    }
                }
            }
            if fork.kind == "exclusive" {
                for i in 0..fork.branches.len() {
                    for j in i + 1..fork.branches.len() {
                        for left in &fork.branches[i] {
                            for right in &fork.branches[j] {
                                let guard = |edge| {
                                    self.property(ElementId::Edge(edge), "when")
                                        .map(|id| self.model.bindings[id.0].value.clone())
                                        .unwrap_or(Value::Bool(true))
                                };
                                if !disjoint(&guard(*left), &guard(*right), &self.model.bindings) {
                                    errors.extend(error(
                                        self.model.edges[right.0].range,
                                        "exclusive fork guards overlap or cannot be proven disjoint",
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

// find one distinct edge per constraint without enumerating their permutations
fn distinct_edge_assignment(candidates: &[Vec<EdgeId>], fixed: Option<(usize, EdgeId)>) -> bool {
    fn assign(
        index: usize,
        candidates: &[Vec<EdgeId>],
        fixed: Option<(usize, EdgeId)>,
        owners: &mut HashMap<EdgeId, usize>,
        visited: &mut HashSet<EdgeId>,
    ) -> bool {
        for edge in &candidates[index] {
            if fixed.is_some_and(|(_, fixed_edge)| *edge == fixed_edge) || !visited.insert(*edge) {
                continue;
            }
            let previous = owners.get(edge).copied();
            if previous.is_none_or(|previous| assign(previous, candidates, fixed, owners, visited)) {
                owners.insert(*edge, index);
                return true;
            }
        }
        false
    }
    let mut owners = HashMap::new();
    for index in 0..candidates.len() {
        if fixed.is_some_and(|(fixed_index, _)| index == fixed_index) {
            continue;
        }
        if !assign(index, candidates, fixed, &mut owners, &mut HashSet::new()) {
            return false;
        }
    }
    true
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Num(Number::Int(a)), Value::Num(Number::Int(b))) => a == b,
        (Value::Num(a), Value::Num(b)) => {
            let number = |n: &Number| match n {
                Number::Int(n) => *n as f64,
                Number::Float(n) => *n,
            };
            number(a) == number(b)
        }
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        (Value::Array(a), Value::Array(b)) => {
            a.elem.len() == b.elem.len()
                && a.elem.iter().zip(&b.elem).all(|(a, b)| match (a, b) {
                    (ArrayElem::Item(a), ArrayElem::Item(b)) => same_value(a, b),
                    _ => false,
                })
        }
        (Value::Object(a), Value::Object(b)) => {
            a.elem.len() == b.elem.len()
                && a.elem.iter().all(|a| {
                    b.elem
                        .iter()
                        .find(|b| b.key == a.key)
                        .is_some_and(|b| same_value(&a.value, &b.value))
                })
        }
        _ => false,
    }
}

#[derive(Clone)]
enum Predicate {
    Constant(bool),
    Atom(usize),
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}
fn predicate_key(value: &Value) -> String {
    match value {
        Value::Unary { op, operand, .. } => format!("{op:?}({})", predicate_key(operand)),
        Value::Binary { op, lhs, rhs, .. } => format!("{op:?}({},{})", predicate_key(lhs), predicate_key(rhs)),
        Value::Index { target, index, .. } => format!("index({},{})", predicate_key(target), predicate_key(index)),
        // function calls retain their source positions because they may sample independently
        _ => format!("{value:?}"),
    }
}

struct PredicateAtom {
    key: String,
    equality: Option<(String, Value)>,
}

fn normalize_predicate(value: &Value, bindings: &[Binding]) -> Value {
    match value {
        Value::BindingRef(id) => match &bindings[id.0].value {
            // a shared draw keeps its identity rather than becoming another call
            Value::Func { .. } => value.clone(),
            definition => normalize_predicate(definition, bindings),
        },
        Value::Unary { op, operand, range } => Value::Unary {
            op: *op,
            operand: Box::new(normalize_predicate(operand, bindings)),
            range: *range,
        },
        Value::Binary { op, lhs, rhs, range } => {
            let mut lhs = normalize_predicate(lhs, bindings);
            let mut rhs = normalize_predicate(rhs, bindings);
            let (op, negate) = match op {
                BinOp::Ne => (BinOp::Eq, true),
                BinOp::Ge => (BinOp::Lt, true),
                BinOp::Gt => {
                    std::mem::swap(&mut lhs, &mut rhs);
                    (BinOp::Lt, false)
                }
                BinOp::Le => {
                    std::mem::swap(&mut lhs, &mut rhs);
                    (BinOp::Lt, true)
                }
                op => (*op, false),
            };
            if op == BinOp::Eq && predicate_key(&lhs) > predicate_key(&rhs) {
                std::mem::swap(&mut lhs, &mut rhs);
            }
            let comparison = Value::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                range: *range,
            };
            if negate {
                Value::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(comparison),
                    range: *range,
                }
            } else {
                comparison
            }
        }
        Value::Index { target, index, range } => Value::Index {
            target: Box::new(normalize_predicate(target, bindings)),
            index: Box::new(normalize_predicate(index, bindings)),
            range: *range,
        },
        _ => value.clone(),
    }
}

fn equality_atom(value: &Value) -> Option<(String, Value)> {
    let Value::Binary {
        op: BinOp::Eq, lhs, rhs, ..
    } = value
    else {
        return None;
    };
    for (subject, literal) in [(lhs, rhs), (rhs, lhs)] {
        if matches!(literal.as_ref(), Value::Str(_) | Value::Num(_) | Value::Bool(_)) {
            return Some((predicate_key(subject), (**literal).clone()));
        }
    }
    None
}

fn predicate(value: &Value, atoms: &mut Vec<PredicateAtom>) -> Predicate {
    match value {
        Value::Bool(value) => Predicate::Constant(*value),
        Value::Unary {
            op: UnaryOp::Not,
            operand,
            ..
        } => Predicate::Not(Box::new(predicate(operand, atoms))),
        Value::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => Predicate::And(Box::new(predicate(lhs, atoms)), Box::new(predicate(rhs, atoms))),
        Value::Binary {
            op: BinOp::Or, lhs, rhs, ..
        } => Predicate::Or(Box::new(predicate(lhs, atoms)), Box::new(predicate(rhs, atoms))),
        _ => {
            // binding identities are stable, independent dynamic calls retain their source locations
            let key = predicate_key(value);
            let index = atoms.iter().position(|atom| atom.key == key).unwrap_or_else(|| {
                atoms.push(PredicateAtom {
                    key,
                    equality: equality_atom(value),
                });
                atoms.len() - 1
            });
            Predicate::Atom(index)
        }
    }
}
fn evaluate(predicate: &Predicate, bits: usize) -> bool {
    match predicate {
        Predicate::Constant(value) => *value,
        Predicate::Atom(index) => bits & (1 << index) != 0,
        Predicate::Not(inner) => !evaluate(inner, bits),
        Predicate::And(a, b) => evaluate(a, bits) && evaluate(b, bits),
        Predicate::Or(a, b) => evaluate(a, bits) || evaluate(b, bits),
    }
}
fn disjoint(a: &Value, b: &Value, bindings: &[Binding]) -> bool {
    let mut atoms = Vec::new();
    let a = predicate(&normalize_predicate(a, bindings), &mut atoms);
    let b = predicate(&normalize_predicate(b, bindings), &mut atoms);
    if atoms.len() > 12 {
        return false;
    }
    let mut incompatible = Vec::new();
    for (i, left) in atoms.iter().enumerate() {
        for (j, right) in atoms.iter().enumerate().skip(i + 1) {
            if let (Some((left_subject, left_value)), Some((right_subject, right_value))) = (&left.equality, &right.equality)
                && left_subject == right_subject
                && !same_value(left_value, right_value)
            {
                incompatible.push((i, j));
            }
        }
    }
    (0..(1usize << atoms.len())).all(|bits| {
        incompatible.iter().any(|(i, j)| bits & (1 << i) != 0 && bits & (1 << j) != 0)
            || !evaluate(&a, bits)
            || !evaluate(&b, bits)
    })
}

pub(super) fn model(ast: ast::GraphDocument) -> Result<Model> {
    let mut modeler = Modeler::new();
    let paths: Vec<_> = ast.paths.iter().map(|path| modeler.path(path, None)).collect();
    if !modeler.errors.is_empty() {
        return Err(modeler.errors);
    }
    if paths.is_empty() {
        return Err(error(SrcRange::new(0, 0), "expected at least one graph statement"));
    }
    let mut resolved = Vec::new();
    for path in paths {
        resolved.push(modeler.lower_path(path)?)
    }
    modeler.model.paths = resolved;
    modeler.lower_fields()?;
    modeler.validate_primitives()?;
    Ok(modeler.model)
}
