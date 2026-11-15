use crate::{
    Diag, DiagPhase, Diags, SrcRange, ast,
    expr::{self, ReferenceValue},
    model::*,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    defaults: HashMap<Slot, Value>,
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
            defaults: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn kinds(&mut self, declarations: &[ast::KindDecl]) -> Result<()> {
        for (index, declaration) in declarations.iter().enumerate() {
            if builtin_construct(&declaration.name).is_some() || reserved_name(&declaration.name) {
                return Err(error(declaration.range, "kind name is reserved"));
            }
            if self
                .model
                .kind_names
                .insert(declaration.name.clone(), KindId(index))
                .is_some()
            {
                return Err(error(declaration.range, "duplicate kind name"));
            }
        }
        let mut kinds = vec![None; declarations.len()];
        let mut states = vec![0; declarations.len()];
        for index in 0..declarations.len() {
            resolve_kind(index, declarations, &self.model.kind_names, &mut states, &mut kinds)?;
        }
        self.model.kinds = kinds.into_iter().map(|kind| kind.expect("resolved kind")).collect();
        Ok(())
    }

    fn element_kind(&self, owner: ElementId) -> (&str, Construct) {
        match owner {
            ElementId::Node(id) => (&self.model.nodes[id.0].kind, Construct::Node),
            ElementId::Edge(id) => (&self.model.edges[id.0].kind, Construct::Edge),
            ElementId::Fork(id) => (&self.model.forks[id.0].kind, Construct::Fork),
            ElementId::Junction(id) => (&self.model.junctions[id.0].kind, Construct::Junction),
        }
    }

    fn kind_schema(&self, owner: ElementId) -> Option<&Kind> {
        let (name, _) = self.element_kind(owner);
        self.model.kind_names.get(name).map(|id| &self.model.kinds[id.0])
    }

    fn primitive(&self, owner: ElementId) -> &str {
        primitive(&self.model, owner)
    }

    fn field_type(&self, slot: &Slot) -> FieldType {
        self.kind_schema(slot.owner)
            .and_then(|kind| {
                let fields = if slot.property { &kind.props } else { &kind.fields };
                fields.iter().find(|field| slot.path.len() == 1 && slot.path[0] == field.name)
            })
            .map(|field| field.ty)
            .or_else(|| {
                if !slot.property || slot.path.len() != 1 {
                    return None;
                }
                let (name, construct) = self.element_kind(slot.owner);
                primitive_properties(construct, name, self.element_range(slot.owner))
                    .into_iter()
                    .find(|field| field.name == slot.path[0])
                    .map(|field| field.ty)
            })
            .unwrap_or(FieldType::Any)
    }

    fn input_role(&self, owner: ElementId, property: bool, path: &[String]) -> bool {
        !property
            && self.kind_schema(owner).is_some_and(|kind| {
                kind.fields
                    .iter()
                    .any(|field| path.first() == Some(&field.name) && field.role.as_deref() == Some("input"))
            })
    }

    fn output_role(&self, owner: ElementId, property: bool, path: &[String]) -> bool {
        !property
            && path.len() == 1
            && self.kind_schema(owner).is_some_and(|kind| {
                kind.fields
                    .iter()
                    .any(|field| path.first() == Some(&field.name) && field.role.as_deref() == Some("output"))
            })
    }

    fn name(&mut self, name: &Option<String>, id: ElementId, range: SrcRange) {
        if let Some(name) = name {
            if matches!(
                name.as_str(),
                "self" | "source" | "target" | "left" | "right" | "context" | "true" | "false" | "null"
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
        let mut fields = fields.to_vec();
        let (name, construct) = self.element_kind(owner);
        let schema = self.kind_schema(owner).cloned();
        if property
            && schema
                .as_ref()
                .map(|kind| kind.construct)
                .or_else(|| builtin_construct(name))
                .is_some_and(|category| category != construct)
        {
            self.errors.extend(error(
                self.element_range(owner),
                "kind does not belong to this graph construct",
            ));
        }
        if let Some(kind) = schema {
            let entries = if property { kind.props } else { kind.fields };
            if property {
                for field in &fields {
                    if !entries.iter().any(|entry| entry.name == field.key) {
                        self.errors.extend(error(
                            field.range,
                            format!("unknown property `{}` for declared kind", field.key),
                        ));
                    }
                }
            }
            for entry in entries {
                if !fields.iter().any(|field| field.key == entry.name) {
                    if let Some(value) = entry.default {
                        self.defaults.insert(
                            Slot {
                                owner,
                                property,
                                path: vec![entry.name.clone()],
                            },
                            value,
                        );
                        fields.push(ast::Attr {
                            key: entry.name,
                            value: ast::Expr::new(ast::ExprKind::Null, entry.range),
                            range: entry.range,
                        });
                    } else if entry.required {
                        self.errors.extend(error(
                            self.element_range(owner),
                            format!(
                                "missing required {} `{}`",
                                if property { "property" } else { "field" },
                                entry.name
                            ),
                        ));
                    }
                }
            }
        }
        let mut seen = HashMap::new();
        for field in &fields {
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
        self.fields.insert((owner, property), fields);
    }

    fn element_range(&self, owner: ElementId) -> SrcRange {
        match owner {
            ElementId::Node(id) => self.model.nodes[id.0].range,
            ElementId::Edge(id) => self.model.edges[id.0].range,
            ElementId::Fork(id) => self.model.forks[id.0].range,
            ElementId::Junction(id) => self.model.junctions[id.0].range,
        }
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
                    direction: step.direction,
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
            ty: self.field_type(&slot),
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
                expr::schema_type(
                    self.reference_value(self.model.bindings[id.0].value.clone()),
                    self.field_type(&slot),
                    range,
                )?;
                Ok(id)
            }
            Err(errors) => {
                self.slots.remove(&slot);
                Err(errors)
            }
        }
    }

    fn lower_binding_expr(&mut self, mut expr: ast::Expr, slot: &Slot) -> Result<Value> {
        if let Some(value) = self.defaults.get(slot) {
            return Ok(value.clone());
        }
        if matches!(expr.kind, ast::ExprKind::Upstream) {
            if slot.property || !matches!(slot.owner, ElementId::Node(_)) || slot.path.len() != 1 {
                return Err(error(expr.range, "upstream markers belong on direct node content fields"));
            }
            return Ok(Value::Input {
                binding: self.slots[slot],
                ty: self.field_type(slot),
            });
        }
        if slot.property && slot.path == ["into"] && self.primitive(slot.owner) == "feeds" {
            let ElementId::Edge(edge) = slot.owner else {
                return Err(error(expr.range, "destination requires an edge"));
            };
            self.resolve_edge(edge)?;
            let Cursor::Slot(destination) = self.cursor(&expr, Some(slot.owner))? else {
                return Err(error(expr.range, "into must reference a target input field"));
            };
            if destination.owner != ElementId::Node(self.model.edges[edge.0].target)
                || destination.property
                || destination.path.len() != 1
            {
                return Err(error(
                    expr.range,
                    "into must reference a direct content field on the edge target",
                ));
            }
            let id = self.binding(destination, expr.range)?;
            if !matches!(self.model.bindings[id.0].value, Value::Input { .. }) {
                return Err(error(expr.range, "destination field must declare an upstream input with <-"));
            }
            return Ok(Value::FieldRef(id));
        }
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
                let id = self.binding(child.clone(), expr.range)?;
                let value = self.model.bindings[id.0].value.clone();
                if expr::static_value(&value) {
                    return Ok(ReferenceValue {
                        value,
                        bindings: Vec::new(),
                    });
                }
                self.finish(Cursor::Slot(child), expr.range)
            });
        }
        expr::lower(expr, &mut |expr| self.resolve_value(expr, Some(slot.owner)))
    }

    fn finish(&mut self, cursor: Cursor, range: SrcRange) -> Result<ReferenceValue> {
        match cursor {
            Cursor::Element(element) => Ok(self.graph_value(element, range)),
            Cursor::Slot(slot) => {
                let input = self.input_role(slot.owner, slot.property, &slot.path);
                let output = !slot.property
                    && self.kind_schema(slot.owner).is_some_and(|kind| {
                        kind.fields
                            .iter()
                            .any(|field| slot.path.first() == Some(&field.name) && field.role.as_deref() == Some("output"))
                    });
                let id = self.binding(slot, range)?;
                let value = self.model.bindings[id.0].value.clone();
                if !input && !output && expr::static_value(&value) {
                    Ok(ReferenceValue {
                        value,
                        bindings: Vec::new(),
                    })
                } else {
                    Ok(self.reference_value(Value::BindingRef(id, range)))
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
                Value::BindingRef(id, _) if visited.insert(id) => value = self.model.bindings[id.0].value.clone(),
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
                "source" | "left" if (name == "source") == (self.model.edges[edge.0].direction == Direction::Directed) => {
                    let source = self.raw_edges[edge.0].source.clone();
                    Ok(Cursor::Element(ElementId::Node(self.endpoint(&source)?)))
                }
                "target" | "right" if (name == "target") == (self.model.edges[edge.0].direction == Direction::Directed) => {
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
                    "source" | "target" | "left" | "right" => {
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
                    if edge.cycle != step.cycle
                        || edge.direction != step.direction
                        || !step.edges.iter().any(|constraint| constraint.kind == edge.kind)
                    {
                        continue;
                    }
                    let anchor = self.raw_edges[i].source.clone();
                    let left = self.endpoint(&anchor)?;
                    if left == source {
                        self.resolve_edge(EdgeId(i))?;
                        targets.insert(self.model.edges[i].target);
                    } else if step.direction == Direction::Undirected {
                        self.resolve_edge(EdgeId(i))?;
                        if self.model.edges[i].target == source {
                            targets.insert(left);
                        }
                    }
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
                            if ((edge.source == source && edge.target == target)
                                || (edge.direction == Direction::Undirected && edge.source == target && edge.target == source))
                                && edge.direction == step.direction
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
            let binding = &self.model.bindings[id.0];
            let definition = if self.input_role(binding.owner, binding.property, &binding.path)
                || self.output_role(binding.owner, binding.property, &binding.path)
            {
                Value::Input {
                    binding: id,
                    ty: binding.ty,
                }
            } else {
                binding.value.clone()
            };
            pending.extend(expr::dependencies(&definition));
            bindings.push((id, definition, self.model.bindings[id.0].ty));
        }
        ReferenceValue { value, bindings }
    }

    fn validate_primitives(&self) -> Result<()> {
        let mut errors = Vec::new();
        for (i, junction) in self.model.junctions.iter().enumerate() {
            if matches!(
                self.primitive(ElementId::Junction(JunctionId(i))),
                "accumulate" | "merge" | "first" | "quorum" | "reduce"
            ) {
                let distinct: HashSet<_> = junction.inputs.iter().copied().collect();
                if distinct.len() != junction.inputs.len() {
                    errors.extend(error(
                        junction.range,
                        "built-in junction inputs must reference distinct edges",
                    ));
                }
            }
            if self.primitive(ElementId::Junction(JunctionId(i))) == "quorum" {
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
        for (i, _node) in self.model.nodes.iter().enumerate() {
            if self.primitive(ElementId::Node(NodeId(i))) == "vars"
                && let Some(id) = self.property(ElementId::Node(NodeId(i)), "evaluate")
            {
                let binding = &self.model.bindings[id.0];
                if !matches!(&binding.value,Value::Str(value) if value=="lazy"||value=="eager") {
                    errors.extend(error(binding.range, "vars evaluate must be `lazy` or `eager`"));
                }
            }
        }
        for i in 0..self.model.edges.len() {
            let owner = ElementId::Edge(EdgeId(i));
            let direction = self
                .kind_schema(owner)
                .and_then(|kind| kind.direction)
                .unwrap_or_else(|| primitive_direction(self.primitive(owner)));
            if !direction.allows(self.model.edges[i].direction) {
                errors.extend(error(
                    self.model.edges[i].range,
                    "edge direction is not permitted by its kind",
                ));
            }
            if self.primitive(owner) == "feeds"
                && let Some(into) = self.property(owner, "into")
            {
                let destination = &self.model.bindings[into.0];
                if let Value::FieldRef(target) = destination.value {
                    if let Some(value) = self.property(owner, "value") {
                        let target = &self.model.bindings[target.0];
                        let Value::Input { ty, .. } = target.value else {
                            unreachable!()
                        };
                        let payload = &self.model.bindings[value.0];
                        if let Err(diags) = expr::schema_type(self.reference_value(payload.value.clone()), ty, payload.range) {
                            errors.extend(diags);
                        }
                    } else {
                        errors.extend(error(destination.range, "feeds with a destination requires a value property"));
                    }
                } else {
                    errors.extend(error(destination.range, "feeds destination must be a field reference"));
                }
            }
            if let Some(id) = self.property(ElementId::Edge(EdgeId(i)), "when") {
                let binding = &self.model.bindings[id.0];
                if let Err(diags) = expr::boolean(self.reference_value(binding.value.clone()), binding.range) {
                    errors.extend(diags)
                }
            }
        }
        let mut predicate_bindings = self.model.bindings.clone();
        for (index, binding) in predicate_bindings.iter_mut().enumerate() {
            if self.input_role(binding.owner, binding.property, &binding.path)
                || self.output_role(binding.owner, binding.property, &binding.path)
            {
                binding.value = Value::Input {
                    binding: BindingId(index),
                    ty: binding.ty,
                };
            }
        }
        for (fork_index, fork) in self.model.forks.iter().enumerate() {
            if self.primitive(ElementId::Fork(ForkId(fork_index))) == "parallel" {
                for edge in fork.branches.iter().flatten() {
                    if let Some(id) = self.property(ElementId::Edge(*edge), "when") {
                        errors.extend(error(
                            self.model.bindings[id.0].range,
                            "parallel fork branches cannot have guards",
                        ));
                    }
                }
            }
            if self.primitive(ElementId::Fork(ForkId(fork_index))) == "exclusive" {
                for i in 0..fork.branches.len() {
                    for j in i + 1..fork.branches.len() {
                        for left in &fork.branches[i] {
                            for right in &fork.branches[j] {
                                let guard = |edge| {
                                    self.property(ElementId::Edge(edge), "when")
                                        .map(|id| self.model.bindings[id.0].value.clone())
                                        .unwrap_or(Value::Bool(true))
                                };
                                if !disjoint(&guard(*left), &guard(*right), &predicate_bindings) {
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

fn reserved_name(name: &str) -> bool {
    matches!(
        name,
        "self" | "source" | "target" | "left" | "right" | "context" | "true" | "false" | "null"
    )
}

fn builtin_construct(name: &str) -> Option<Construct> {
    [Construct::Node, Construct::Edge, Construct::Fork, Construct::Junction]
        .into_iter()
        .find(|construct| name == construct.name() || crate::spec::SPEC.primitive(construct.name(), name).is_some())
}

fn schema_entries(expr: &ast::Expr) -> Result<Vec<ast::Attr>> {
    let ast::ExprKind::Object(items) = &expr.kind else {
        return Err(error(expr.range, "schema descriptors must be objects"));
    };
    let mut seen = HashSet::new();
    let mut fields = Vec::new();
    for item in items {
        let ast::ObjectItem::Attr(field) = item else {
            return Err(error(expr.range, "schema spreads are not supported"));
        };
        if !seen.insert(&field.key) {
            return Err(error(field.range, "duplicate schema key"));
        }
        fields.push(field.clone());
    }
    Ok(fields)
}

fn schema_string(expr: &ast::Expr) -> Result<String> {
    match &expr.kind {
        ast::ExprKind::Str(value) => Ok(value.clone()),
        _ => Err(error(expr.range, "expected a literal schema string")),
    }
}

fn schema_fields(expr: &ast::Expr, inherited: &mut Vec<FieldSchema>) -> Result<()> {
    for field in schema_entries(expr)? {
        let previous = inherited.iter().find(|entry| entry.name == field.key).cloned();
        let mut schema = previous.clone().unwrap_or(FieldSchema {
            name: field.key.clone(),
            ty: FieldType::Any,
            required: false,
            default: None,
            role: None,
            range: field.range,
        });
        schema.range = field.range;
        for descriptor in schema_entries(&field.value)? {
            match descriptor.key.as_str() {
                "type" => {
                    schema.ty = match schema_string(&descriptor.value)?.as_str() {
                        "any" => FieldType::Any,
                        "string" => FieldType::String,
                        "number" => FieldType::Number,
                        "boolean" => FieldType::Boolean,
                        "array" => FieldType::Array,
                        "object" => FieldType::Object,
                        _ => return Err(error(descriptor.range, "unknown schema value type")),
                    };
                }
                "required" => match descriptor.value.kind {
                    ast::ExprKind::Bool(value) => schema.required = value,
                    _ => return Err(error(descriptor.range, "required must be a literal boolean")),
                },
                "role" => schema.role = Some(schema_string(&descriptor.value)?),
                "default" => {
                    let value = expr::lower(descriptor.value, &mut |expr| {
                        Err(error(expr.range, "schema defaults cannot reference instances"))
                    })?;
                    if !expr::constant(&value) {
                        return Err(error(descriptor.range, "schema defaults must be constant"));
                    }
                    schema.default = Some(value);
                }
                _ => {
                    return Err(error(
                        descriptor.range,
                        format!("unknown schema descriptor `{}`", descriptor.key),
                    ));
                }
            }
        }
        if let Some(previous) = &previous {
            if previous.ty != FieldType::Any && schema.ty != previous.ty {
                return Err(error(field.range, "an extension cannot change an inherited field type"));
            }
            if previous.required && !schema.required {
                return Err(error(field.range, "an extension cannot make a required field optional"));
            }
        }
        if let Some(default) = &schema.default {
            expr::schema_type(
                ReferenceValue {
                    value: default.clone(),
                    bindings: Vec::new(),
                },
                schema.ty,
                schema.range,
            )?;
        }
        if let Some(index) = inherited.iter().position(|entry| entry.name == schema.name) {
            inherited[index] = schema;
        } else {
            inherited.push(schema);
        }
    }
    Ok(())
}

fn primitive_direction(kind: &str) -> Directions {
    match kind {
        "next" | "requires" | "contains" | "feeds" => Directions::Directed,
        "incidence" => Directions::Undirected,
        _ => Directions::Either,
    }
}

fn primitive_properties(construct: Construct, primitive: &str, range: SrcRange) -> Vec<FieldSchema> {
    let mut names = Vec::new();
    if construct == Construct::Edge {
        names.push(("when", FieldType::Boolean, false));
        names.push(("weight", FieldType::Number, false));
        names.push(("max_traversals", FieldType::Number, false));
    }
    match primitive {
        "feeds" => names.extend([("value", FieldType::Any, false), ("into", FieldType::Any, false)]),
        "accumulate" | "first" => names.push(("correlate", FieldType::Any, false)),
        "quorum" => names.extend([("correlate", FieldType::Any, false), ("count", FieldType::Number, true)]),
        "vars" => names.push(("evaluate", FieldType::String, false)),
        "network" => names.extend([
            ("nodes", FieldType::Array, true),
            ("edges", FieldType::Array, true),
            ("validate", FieldType::String, false),
            ("acyclic", FieldType::String, false),
            ("connected", FieldType::String, false),
            ("self_loops", FieldType::Boolean, false),
            ("max_parallel", FieldType::Number, false),
            ("incoming", FieldType::Object, false),
            ("outgoing", FieldType::Object, false),
            ("degree", FieldType::Object, false),
        ]),
        "relation" => names.extend([
            ("participants", FieldType::Object, false),
            ("inputs", FieldType::Object, false),
            ("outputs", FieldType::Object, false),
            ("unique", FieldType::Boolean, false),
        ]),
        "incidence" => names.push(("role", FieldType::String, false)),

        "repeat" => names.push(("count", FieldType::Number, true)),
        "each" => names.extend([("items", FieldType::Array, true), ("concurrency", FieldType::Number, false)]),
        "reduce" => names.extend([
            ("correlate", FieldType::Any, false),
            ("initial", FieldType::Any, true),
            ("combine", FieldType::Any, true),
            ("order", FieldType::String, false),
        ]),
        _ => {}
    }
    if construct == Construct::Junction && matches!(primitive, "accumulate" | "merge" | "first" | "quorum" | "reduce") {
        names.extend([
            ("observe", FieldType::String, false),
            ("on_failure", FieldType::String, false),
            ("on_cancel", FieldType::String, false),
        ]);
    }
    names
        .into_iter()
        .map(|(name, ty, required)| FieldSchema {
            name: name.to_owned(),
            ty,
            required,
            default: None,
            role: None,
            range,
        })
        .collect()
}

fn resolve_kind(
    index: usize,
    declarations: &[ast::KindDecl],
    names: &std::collections::BTreeMap<String, KindId>,
    states: &mut [u8],
    kinds: &mut [Option<Kind>],
) -> Result<Kind> {
    if let Some(kind) = &kinds[index] {
        return Ok(kind.clone());
    }
    let declaration = &declarations[index];
    if states[index] == 1 {
        return Err(error(declaration.range, "circular kind extension"));
    }
    states[index] = 1;
    let mut kind = if let Some(base) = names.get(&declaration.base) {
        let mut kind = resolve_kind(base.0, declarations, names, states, kinds)?;
        if kind.construct != declaration.construct {
            return Err(error(declaration.range, "kind extension crosses graph construct categories"));
        }
        kind.base_kind = Some(*base);
        kind
    } else {
        let construct = builtin_construct(&declaration.base)
            .ok_or_else(|| error(declaration.range, format!("unknown base kind `{}`", declaration.base)))?;
        if construct != declaration.construct {
            return Err(error(declaration.range, "kind extension crosses graph construct categories"));
        }
        Kind {
            name: String::new(),
            base: declaration.base.clone(),
            base_kind: None,
            construct,
            primitive: declaration.base.clone(),
            direction: (construct == Construct::Edge).then(|| primitive_direction(&declaration.base)),
            props: primitive_properties(construct, &declaration.base, declaration.range),
            fields: Vec::new(),
            range: declaration.range,
        }
    };
    kind.name = declaration.name.clone();
    kind.base = declaration.base.clone();
    kind.range = declaration.range;
    let mut seen = HashSet::new();
    for field in &declaration.schema {
        if !seen.insert(&field.key) {
            return Err(error(field.range, "duplicate kind schema section"));
        }
        match field.key.as_str() {
            "direction" if kind.construct == Construct::Edge => {
                let direction = match schema_string(&field.value)?.as_str() {
                    "directed" => Directions::Directed,
                    "undirected" => Directions::Undirected,
                    "either" => Directions::Either,
                    _ => return Err(error(field.range, "direction must be directed, undirected or either")),
                };
                if kind.direction != Some(Directions::Either) && kind.direction != Some(direction) {
                    return Err(error(
                        field.range,
                        "an extension cannot widen or contradict its base direction",
                    ));
                }
                kind.direction = Some(direction);
            }
            "props" => schema_fields(&field.value, &mut kind.props)?,
            "fields" if kind.construct == Construct::Node => schema_fields(&field.value, &mut kind.fields)?,
            _ => return Err(error(field.range, "expected props, or fields for a node kind")),
        }
    }
    if kind.fields.iter().any(|field| field.name == "props") {
        return Err(error(kind.range, "props is reserved for node properties"));
    }
    states[index] = 2;
    kinds[index] = Some(kind.clone());
    Ok(kind)
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
        Value::BindingRef(id, _) => format!("binding:{}", id.0),
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
        Value::BindingRef(id, _) => match &bindings[id.0].value {
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
    modeler.kinds(&ast.kinds)?;
    let paths: Vec<_> = ast.paths.iter().map(|path| modeler.path(path, None)).collect();
    if !modeler.errors.is_empty() {
        return Err(modeler.errors);
    }
    if paths.is_empty() && ast.kinds.is_empty() {
        return Err(error(SrcRange::new(0, 0), "expected at least one graph statement"));
    }
    let mut resolved = Vec::new();
    for path in paths {
        resolved.push(modeler.lower_path(path)?)
    }
    modeler.model.paths = resolved;
    modeler.lower_fields()?;
    modeler.validate_primitives()?;
    validate_graph(&mut modeler.model)?;
    Ok(modeler.model)
}

struct GraphCheck<'a> {
    model: &'a Model,
    owner: ElementId,
    range: SrcRange,
}
impl<'a> GraphCheck<'a> {
    fn error<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(vec![Diag {
            when: DiagPhase::Validation,
            what: message.into(),
            r#where: self.range,
        }])
    }

    fn value(&self, mut value: &'a Value) -> &'a Value {
        while let Value::BindingRef(id, _) = value {
            value = &self.model.bindings[id.0].value;
        }
        value
    }

    fn prop(&self, name: &str) -> Option<&'a Value> {
        properties(self.model, self.owner)
            .iter()
            .map(|id| &self.model.bindings[id.0])
            .find(|binding| binding.path == [name])
            .map(|binding| self.value(&binding.value))
    }

    fn required(&self, name: &str) -> Result<&'a Value> {
        self.prop(name)
            .map(Ok)
            .unwrap_or_else(|| self.error(format!("missing required property `{name}`")))
    }

    fn choice(&self, name: &str, default: &'a str, allowed: &[&str]) -> Result<&'a str> {
        match self.prop(name) {
            None => Ok(default),
            Some(Value::Str(value)) if allowed.contains(&value.as_str()) => Ok(value),
            _ => self.error(format!("{name} must be a constant value from {}", allowed.join(", "))),
        }
    }

    fn boolean(&self, name: &str, default: bool) -> Result<bool> {
        match self.prop(name) {
            None => Ok(default),
            Some(Value::Bool(value)) => Ok(*value),
            _ => self.error(format!("{name} must be a constant boolean")),
        }
    }

    fn count(&self, value: &Value, name: &str) -> Result<usize> {
        if let Value::Num(Number::Int(value)) = value
            && let Ok(value) = usize::try_from(*value)
        {
            return Ok(value);
        }
        self.error(format!("{name} must be a constant nonnegative integer"))
    }

    fn positive_count(&self, name: &str, default: Option<usize>) -> Result<Option<usize>> {
        let Some(value) = self.prop(name) else { return Ok(default) };
        self.typed(value, FieldType::Number)?;
        if !expr::constant(value) {
            return Ok(None);
        }
        let count = self.count(value, name)?;
        if count == 0 {
            return self.error(format!("{name} must be positive"));
        }
        Ok(Some(count))
    }

    fn bounds(&self, name: &str) -> Result<DegreeBounds> {
        let Some(value) = self.prop(name) else {
            return Ok(DegreeBounds::default());
        };
        let Value::Object(object) = value else {
            return self.error(format!("{name} must contain min or max bounds"));
        };
        let mut bounds = DegreeBounds::default();
        for field in &object.elem {
            let value = self.count(self.value(&field.value), name)?;
            match field.key.as_str() {
                "min" => bounds.min = Some(value),
                "max" => bounds.max = Some(value),
                _ => return self.error(format!("unknown {name} bound `{}`", field.key)),
            }
        }
        if matches!((bounds.min, bounds.max), (Some(min), Some(max)) if min > max) {
            return self.error(format!("{name} minimum exceeds maximum"));
        }
        Ok(bounds)
    }

    fn references(&self, name: &str, node: bool) -> Result<Vec<ElementId>> {
        let Value::Array(array) = self.required(name)? else {
            return self.error(format!("{name} must be an array of references"));
        };
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for item in &array.elem {
            let ArrayElem::Item(value) = item else {
                return self.error("network membership must resolve statically");
            };
            let Value::GraphRef { ref_id, .. } = self.value(value) else {
                return self.error("network membership requires graph references");
            };
            let id = self.model.refs[ref_id.0].target;
            if !matches!((node, id), (true, ElementId::Node(_)) | (false, ElementId::Edge(_))) {
                return self.error(format!("wrong reference category in {name}"));
            }
            if !seen.insert(id) {
                return self.error(format!("duplicate reference in {name}"));
            }
            selected.push(id);
        }
        Ok(selected)
    }

    fn typed(&self, value: &Value, ty: FieldType) -> Result<()> {
        let mut pending = expr::dependencies(value);
        let mut bindings = Vec::new();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if seen.insert(id) {
                let binding = &self.model.bindings[id.0];
                pending.extend(expr::dependencies(&binding.value));
                bindings.push((id, binding.value.clone(), binding.ty));
            }
        }
        expr::schema_type(
            expr::ReferenceValue {
                value: value.clone(),
                bindings,
            },
            ty,
            self.range,
        )
    }

    fn properties(&self) -> Result<()> {
        let (kind, construct) = match self.owner {
            ElementId::Node(id) => (&self.model.nodes[id.0].kind, Construct::Node),
            ElementId::Edge(id) => (&self.model.edges[id.0].kind, Construct::Edge),
            ElementId::Fork(id) => (&self.model.forks[id.0].kind, Construct::Fork),
            ElementId::Junction(id) => (&self.model.junctions[id.0].kind, Construct::Junction),
        };
        let base = primitive(self.model, self.owner);
        let descriptors = primitive_properties(construct, base, self.range);
        for descriptor in &descriptors {
            if let Some(value) = self.prop(&descriptor.name) {
                self.typed(value, descriptor.ty)?;
            } else if descriptor.required {
                return self.error(format!("missing required property `{}`", descriptor.name));
            }
        }
        if !self.model.kind_names.contains_key(kind)
            && matches!(
                base,
                "network" | "relation" | "incidence" | "weighted" | "each" | "repeat" | "reduce"
            )
        {
            for id in properties(self.model, self.owner) {
                let binding = &self.model.bindings[id.0];
                if !descriptors.iter().any(|entry| binding.path == [entry.name.clone()]) {
                    return self.error(format!(
                        "unknown property `{}` for built-in kind `{base}`",
                        binding.path.join(".")
                    ));
                }
            }
        }
        Ok(())
    }

    fn network(&self, node: NodeId) -> Result<Network> {
        let nodes: Vec<_> = self
            .references("nodes", true)?
            .into_iter()
            .map(|id| match id {
                ElementId::Node(id) => id,
                _ => unreachable!(),
            })
            .collect();
        let edges: Vec<_> = self
            .references("edges", false)?
            .into_iter()
            .map(|id| match id {
                ElementId::Edge(id) => id,
                _ => unreachable!(),
            })
            .collect();
        let members: HashSet<_> = nodes.iter().copied().collect();
        for id in &edges {
            let edge = &self.model.edges[id.0];
            if !members.contains(&edge.source) || !members.contains(&edge.target) {
                return self.error("selected edge endpoints must be explicitly included in network nodes");
            }
        }
        let network = Network {
            node,
            nodes,
            edges,
            validate: match self.choice("validate", "definition", &["definition", "materialized", "both"])? {
                "definition" => ValidationStage::Definition,
                "materialized" => ValidationStage::Materialized,
                _ => ValidationStage::Both,
            },
            acyclic: match self.choice("acyclic", "none", &["none", "directed", "underlying"])? {
                "directed" => Acyclic::Directed,
                "underlying" => Acyclic::Underlying,
                _ => Acyclic::None,
            },
            connected: match self.choice("connected", "none", &["none", "weak", "strong"])? {
                "weak" => Connectivity::Weak,
                "strong" => Connectivity::Strong,
                _ => Connectivity::None,
            },
            self_loops: self.boolean("self_loops", true)?,
            max_parallel: self
                .prop("max_parallel")
                .map(|value| self.count(value, "max_parallel"))
                .transpose()?,
            incoming: self.bounds("incoming")?,
            outgoing: self.bounds("outgoing")?,
            degree: self.bounds("degree")?,
        };
        if network.validate != ValidationStage::Materialized {
            self.structure(&network)?;
        }
        Ok(network)
    }

    fn structure(&self, network: &Network) -> Result<()> {
        let positions: HashMap<_, _> = network.nodes.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let n = positions.len();
        let mut outgoing = vec![0; n];
        let mut incoming = vec![0; n];
        let mut degree = vec![0; n];
        let mut forward = vec![Vec::new(); n];
        let mut reverse = vec![Vec::new(); n];
        let mut weak = vec![Vec::new(); n];
        let mut pairs = BTreeMap::new();
        let mut parents: Vec<_> = (0..n).collect();
        fn root(parents: &mut [usize], mut i: usize) -> usize {
            while parents[i] != i {
                parents[i] = parents[parents[i]];
                i = parents[i];
            }
            i
        }
        for id in &network.edges {
            let edge = &self.model.edges[id.0];
            let a = positions[&edge.source];
            let b = positions[&edge.target];
            if !network.self_loops && a == b {
                return self.error("network prohibits self-connections");
            }
            let pair = if edge.direction == Direction::Undirected && a > b {
                (b, a)
            } else {
                (a, b)
            };
            let count = pairs.entry((edge.direction, pair)).or_insert(0);
            *count += 1;
            if network.max_parallel.is_some_and(|max| *count > max) {
                return self.error("network exceeds max_parallel");
            }
            forward[a].push(b);
            reverse[b].push(a);
            weak[a].push(b);
            weak[b].push(a);
            if edge.direction == Direction::Directed {
                outgoing[a] += 1;
                incoming[b] += 1;
            } else {
                if network.acyclic == Acyclic::Directed {
                    return self.error("directed acyclicity requires directed selected edges");
                }
                degree[a] += 1;
                degree[b] += 1;
                forward[b].push(a);
                reverse[a].push(b);
            }
            if network.acyclic == Acyclic::Underlying {
                let a = root(&mut parents, a);
                let b = root(&mut parents, b);
                if a == b {
                    return self.error("network contains an underlying cycle");
                }
                parents[a] = b;
            }
        }
        for (name, values, bounds) in [
            ("incoming", &incoming, network.incoming),
            ("outgoing", &outgoing, network.outgoing),
            ("degree", &degree, network.degree),
        ] {
            if values.iter().any(|value| !within(*value, bounds)) {
                return self.error(format!("network violates {name} bounds"));
            }
        }
        if network.acyclic == Acyclic::Directed {
            let mut remaining = incoming;
            let mut pending: Vec<_> = remaining
                .iter()
                .enumerate()
                .filter_map(|(i, count)| (*count == 0).then_some(i))
                .collect();
            let mut visited = 0;
            while let Some(i) = pending.pop() {
                visited += 1;
                for next in &forward[i] {
                    remaining[*next] -= 1;
                    if remaining[*next] == 0 {
                        pending.push(*next);
                    }
                }
            }
            if visited != n {
                return self.error("network contains a directed cycle");
            }
        }
        fn connected(adj: &[Vec<usize>]) -> bool {
            if adj.is_empty() {
                return false;
            }
            let mut seen = vec![false; adj.len()];
            let mut pending = vec![0];
            while let Some(i) = pending.pop() {
                if !seen[i] {
                    seen[i] = true;
                    pending.extend(&adj[i]);
                }
            }
            seen.into_iter().all(|seen| seen)
        }
        let valid = match network.connected {
            Connectivity::None => true,
            Connectivity::Weak => connected(&weak),
            Connectivity::Strong => connected(&forward) && connected(&reverse),
        };
        if !valid {
            return self.error("network does not satisfy connected policy");
        }
        Ok(())
    }
}

fn within(count: usize, bounds: DegreeBounds) -> bool {
    bounds.min.is_none_or(|min| count >= min) && bounds.max.is_none_or(|max| count <= max)
}

fn primitive(model: &Model, owner: ElementId) -> &str {
    let kind = match owner {
        ElementId::Node(id) => &model.nodes[id.0].kind,
        ElementId::Edge(id) => &model.edges[id.0].kind,
        ElementId::Fork(id) => &model.forks[id.0].kind,
        ElementId::Junction(id) => &model.junctions[id.0].kind,
    };
    model
        .kind_names
        .get(kind)
        .map(|id| model.kinds[id.0].primitive.as_str())
        .unwrap_or(kind)
}

fn properties(model: &Model, owner: ElementId) -> &[BindingId] {
    match owner {
        ElementId::Node(id) => &model.nodes[id.0].props,
        ElementId::Edge(id) => &model.edges[id.0].props,
        ElementId::Fork(id) => &model.forks[id.0].props,
        ElementId::Junction(id) => &model.junctions[id.0].props,
    }
}

fn validate_graph(model: &mut Model) -> Result<()> {
    for (owner, range) in model
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (ElementId::Node(NodeId(i)), node.range))
        .chain(
            model
                .edges
                .iter()
                .enumerate()
                .map(|(i, edge)| (ElementId::Edge(EdgeId(i)), edge.range)),
        )
        .chain(
            model
                .forks
                .iter()
                .enumerate()
                .map(|(i, fork)| (ElementId::Fork(ForkId(i)), fork.range)),
        )
        .chain(
            model
                .junctions
                .iter()
                .enumerate()
                .map(|(i, junction)| (ElementId::Junction(JunctionId(i)), junction.range)),
        )
    {
        GraphCheck { model, owner, range }.properties()?;
    }
    let mut networks = Vec::new();
    let mut incidences: HashMap<NodeId, Vec<(NodeId, String)>> = HashMap::new();
    for (i, edge) in model.edges.iter().enumerate() {
        let owner = ElementId::Edge(EdgeId(i));
        let check = GraphCheck {
            model,
            owner,
            range: edge.range,
        };
        if let Some(limit) = check.prop("max_traversals") {
            if !edge.cycle {
                return check.error("max_traversals requires a cycle edge");
            }
            check.typed(limit, FieldType::Number)?;
            if expr::constant(limit) {
                check.count(limit, "max_traversals")?;
            }
        }
        if primitive(model, owner) == "incidence" {
            let check = GraphCheck {
                model,
                owner,
                range: edge.range,
            };
            let left = primitive(model, ElementId::Node(edge.source)) == "relation";
            let right = primitive(model, ElementId::Node(edge.target)) == "relation";
            if left == right {
                return check.error("incidence requires exactly one relation endpoint");
            }
            let role = check.choice("role", "member", &["member", "input", "output"])?;
            let (relation, participant) = if left {
                (edge.source, edge.target)
            } else {
                (edge.target, edge.source)
            };
            incidences.entry(relation).or_default().push((participant, role.to_owned()));
        }
    }
    for (i, node) in model.nodes.iter().enumerate() {
        let id = NodeId(i);
        let owner = ElementId::Node(id);
        let check = GraphCheck {
            model,
            owner,
            range: node.range,
        };
        match primitive(model, owner) {
            "network" => networks.push(check.network(id)?),
            "relation" => {
                let entries = incidences.get(&id).map(Vec::as_slice).unwrap_or_default();
                let inputs = entries.iter().filter(|(_, role)| role == "input").count();
                let outputs = entries.iter().filter(|(_, role)| role == "output").count();
                if inputs + outputs > 0 && (inputs == 0 || outputs == 0 || inputs + outputs != entries.len()) {
                    return check.error("directed relations require input and output incidences without member roles");
                }
                if check.boolean("unique", true)? {
                    let mut seen = HashSet::new();
                    if entries.iter().any(|entry| !seen.insert(entry)) {
                        return check.error("duplicate relation participant in the same role");
                    }
                }
                for (name, count) in [("participants", entries.len()), ("inputs", inputs), ("outputs", outputs)] {
                    if !within(count, check.bounds(name)?) {
                        return check.error(format!("relation violates {name} bounds"));
                    }
                }
            }
            _ => {}
        }
    }
    for (i, fork) in model.forks.iter().enumerate() {
        let owner = ElementId::Fork(ForkId(i));
        let check = GraphCheck {
            model,
            owner,
            range: fork.range,
        };
        match primitive(model, owner) {
            "each" | "repeat" => {
                if primitive(model, owner) == "repeat" {
                    let count = check.required("count")?;
                    check.typed(count, FieldType::Number)?;
                    if expr::constant(count) {
                        check.count(count, "count")?;
                    }
                } else {
                    check.typed(check.required("items")?, FieldType::Array)?;
                    check.positive_count("concurrency", None)?;
                }
                for edge in fork.branches.iter().flatten() {
                    let edge_check = GraphCheck {
                        model,
                        owner: ElementId::Edge(*edge),
                        range: model.edges[edge.0].range,
                    };
                    if edge_check.prop("when").is_some() {
                        return edge_check.error("each and repeat run every listed branch and do not allow branch guards");
                    }
                }
            }
            "weighted" => {
                if check.prop("count").is_some() {
                    return check.error("weighted selects exactly one branch and does not accept count");
                }
                for branch in &fork.branches {
                    let mut branch_weight = None;
                    for edge in branch {
                        let edge_check = GraphCheck {
                            model,
                            owner: ElementId::Edge(*edge),
                            range: model.edges[edge.0].range,
                        };
                        let weight = match edge_check.prop("weight") {
                            None => return edge_check.error("weighted branches require an explicit weight"),
                            Some(Value::Num(Number::Int(value))) => Some(*value as f64),
                            Some(Value::Num(Number::Float(value))) => Some(*value),
                            Some(value) if !expr::constant(value) => None,
                            _ => return edge_check.error("weighted branch weight must be a nonnegative number"),
                        };
                        if let Some(weight) = weight {
                            if !weight.is_finite() || weight < 0.0 {
                                return edge_check.error("weighted branch weight must be finite and nonnegative");
                            }
                            if branch_weight.is_some_and(|previous| previous != weight) {
                                return edge_check.error("all edges in a weighted branch must have the same weight");
                            }
                            branch_weight = Some(weight);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (i, junction) in model.junctions.iter().enumerate() {
        let owner = ElementId::Junction(JunctionId(i));
        if matches!(
            primitive(model, owner),
            "accumulate" | "merge" | "first" | "quorum" | "reduce"
        ) {
            let check = GraphCheck {
                model,
                owner,
                range: junction.range,
            };
            check.choice("observe", "arrival", &["arrival", "result"])?;
            check.choice("on_failure", "fail", &["fail", "skip"])?;
            check.choice("on_cancel", "fail", &["fail", "skip"])?;
            if primitive(model, owner) == "reduce" {
                check.required("initial")?;
                check.required("combine")?;
                check.choice("order", "source", &["source", "arrival"])?;
            }
        }
    }
    model.networks = networks;
    Ok(())
}
