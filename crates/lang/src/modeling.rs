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

// keeps the diagnostics of both sides when either fails
fn both<A, B>(a: Result<A>, b: Result<B>) -> Result<(A, B)> {
    match (a, b) {
        (Ok(a), Ok(b)) => Ok((a, b)),
        (a, b) => Err(a.err().into_iter().chain(b.err()).flatten().collect()),
    }
}

// the first of each identical diagnostic, in reporting order
fn dedup(diags: Diags) -> Diags {
    let mut seen = HashSet::new();
    diags
        .into_iter()
        .filter(|diag| seen.insert((diag.what.clone(), diag.r#where.source, diag.r#where.start, diag.r#where.end)))
        .collect()
}

// resolution states shared by kinds, edges, forks and junctions
const PENDING: u8 = 0;
const RESOLVING: u8 = 1;
const RESOLVED: u8 = 2;
// the failure was already reported, dependents fail with no diagnostics of their own
const FAILED: u8 = 3;

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
    // elements whose kind or properties could not be established, checks that read them are skipped
    unchecked: HashSet<ElementId>,
    // every name a definition statement introduced, including definitions that failed
    defined: HashSet<String>,
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
            unchecked: HashSet::new(),
            defined: HashSet::new(),
            errors: Vec::new(),
        }
    }

    fn definitions(&mut self, definitions: &[ast::Definition]) {
        let mut resolver = Definitions::default();
        for definition in definitions {
            let header = &definition.head.header;
            let defined = match construct_named(&header.kind) {
                Some(construct) => Defined::Kind(construct),
                None if header.kind == "trait" => Defined::Trait,
                None => {
                    self.errors.extend(error(
                        header.range,
                        "a definition head names its construct: node, edge, fork, junction or trait",
                    ));
                    continue;
                }
            };
            // the parser admits only named heads
            let Some(name) = header.name.as_deref() else { continue };
            self.defined.insert(name.to_owned());
            let range = header.name_range.unwrap_or(header.range);
            let (relationships, shape_errors) = relationships(&definition.steps);
            let broken = !shape_errors.is_empty();
            self.errors.extend(shape_errors);
            if builtin_construct(name).is_some() || reserved_name(name) || name == "trait" {
                self.errors.extend(error(range, "definition name is reserved"));
            } else if resolver.names.contains_key(name) {
                self.errors.extend(error(range, "duplicate kind or trait name"));
            } else {
                resolver.add(name, defined, &definition.head, relationships, definition.range, broken);
            }
        }
        for index in 0..resolver.entries.len() {
            let resolved = match resolver.entries[index].defined {
                Defined::Kind(construct) => resolver.resolve_kind(index, construct).map(drop),
                Defined::Trait => resolver.resolve_trait(index).map(drop),
            };
            if let Err(diags) = resolved {
                self.errors.extend(diags);
            }
        }
        // an instance of a definition that failed to resolve is left unchecked rather than checked against a partial schema
        for (index, entry) in resolver.entries.iter().enumerate() {
            let name = entry.name.to_owned();
            match entry.defined {
                Defined::Kind(construct) => {
                    let id = KindId(self.model.kinds.len());
                    let kind = resolver.kinds[index].take();
                    if kind.is_some() {
                        self.model.kind_names.insert(name.clone(), id);
                    }
                    // a placeholder is only reachable with errors recorded, so the model holding it is never returned
                    self.model.kinds.push(kind.unwrap_or_else(|| Kind {
                        name,
                        base: construct.name().to_owned(),
                        base_kind: None,
                        construct,
                        primitive: construct.name().to_owned(),
                        direction: None,
                        props: Vec::new(),
                        fields: Vec::new(),
                        open: false,
                        traits: Vec::new(),
                        range: entry.range,
                    }));
                }
                Defined::Trait => {
                    let id = TraitId(self.model.traits.len());
                    let resolved = resolver.traits[index].take();
                    if resolved.is_some() {
                        self.model.trait_names.insert(name.clone(), id);
                    }
                    self.model.traits.push(resolved.unwrap_or_else(|| Trait {
                        name,
                        base: None,
                        on: Vec::new(),
                        props: Vec::new(),
                        fields: Vec::new(),
                        range: entry.range,
                    }));
                }
            }
        }
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

    // establishes that an instance names a built-in or declared kind of its own construct
    fn kind(&mut self, owner: ElementId, at: SrcRange) {
        let (name, construct) = self.element_kind(owner);
        let declared = self.model.kind_names.get(name).map(|id| self.model.kinds[id.0].construct);
        let message = if self.model.trait_names.contains_key(name) {
            format!("`{name}` is a trait, not a kind")
        } else if let Some(category) = declared.or_else(|| builtin_construct(name)) {
            if category == construct {
                return;
            }
            "kind does not belong to this graph construct".to_owned()
        } else if self.defined.contains(name) {
            // the definition already reported its own failure
            self.unchecked.insert(owner);
            return;
        } else {
            let known = crate::spec::SPEC
                .primitives
                .iter()
                .filter(|primitive| primitive.construct == construct.name())
                .map(|primitive| primitive.name)
                .chain([construct.name()])
                .chain(
                    self.model
                        .kinds
                        .iter()
                        .filter(|kind| kind.construct == construct)
                        .map(|kind| kind.name.as_str()),
                );
            format!("unknown {} kind `{name}`{}", construct.name(), suggestion(name, known))
        };
        self.errors.extend(error(at, message));
        self.unchecked.insert(owner);
    }

    fn fields(&mut self, owner: ElementId, property: bool, fields: &[ast::Attr]) {
        let mut fields = fields.to_vec();
        let (name, construct) = self.element_kind(owner);
        let name = name.to_owned();
        let schema = self.kind_schema(owner).cloned();
        // the root node kind takes any property, every other built-in has a fixed set
        if property && schema.is_none() && name != "node" && builtin_construct(&name) == Some(construct) {
            let known = primitive_properties(construct, &name, self.element_range(owner));
            for field in &fields {
                if !known.iter().any(|entry| entry.name == field.key) {
                    let hint = suggestion(&field.key, known.iter().map(|entry| entry.name.as_str()));
                    self.errors.extend(error(
                        key_range(field),
                        format!("unknown property `{}` for built-in kind `{name}`{hint}", field.key),
                    ));
                }
            }
        }
        if let Some(kind) = schema {
            let entries = if property { kind.props } else { kind.fields };
            if property || !kind.open {
                let what = if property { "property" } else { "field" };
                for field in &fields {
                    if !entries.iter().any(|entry| entry.name == field.key) {
                        let hint = suggestion(&field.key, entries.iter().map(|entry| entry.name.as_str()));
                        self.errors.extend(error(
                            key_range(field),
                            format!("unknown {what} `{}` for kind `{}`{hint}", field.key, kind.name),
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
                        self.unchecked.insert(owner);
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
                self.kind(element, node.header.kind_range);
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
                self.kind(element, header.kind_range);
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
        self.kind(ElementId::Junction(id), junction.kind_range);
        self.fields(ElementId::Junction(id), true, junction.props.as_deref().unwrap_or(&[]));
        self.raw_junctions.push(RawJunction {
            inputs: junction.inputs.clone(),
            target,
        });
        self.junction_state.push(0);
        id
    }

    fn path(&mut self, path: &ast::GraphPath, parent: Option<ForkId>) -> RawPath {
        let (origin, current) = match &path.start {
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
        RawPath {
            origin,
            steps: self.steps(&path.steps, current),
            range: path.range,
        }
    }

    fn steps(&mut self, authored: &[ast::GraphStep], mut current: Anchor) -> Vec<PathStep> {
        let mut steps = Vec::new();
        for step in authored {
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
                    self.kind(element, fork.header.kind_range);
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
                // every arm starts from the node reached before the fan-out
                ast::GraphStep::Arms(arms) => {
                    let arms = arms.iter().map(|arm| self.steps(arm, current.clone())).collect();
                    steps.push(PathStep::Arms(arms));
                }
            }
        }
        steps
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

    // reports a resolution failure once, later dependents fail without repeating it
    fn unresolved<T>(&mut self, element: ElementId, diags: Diags) -> Result<T> {
        match element {
            ElementId::Edge(id) => self.edge_state[id.0] = FAILED,
            ElementId::Fork(id) => self.fork_state[id.0] = FAILED,
            ElementId::Junction(id) => self.junction_state[id.0] = FAILED,
            ElementId::Node(_) => {}
        }
        self.errors.extend(diags);
        Err(Vec::new())
    }

    fn resolve_edge(&mut self, id: EdgeId) -> Result<()> {
        match self.edge_state[id.0] {
            RESOLVED => return Ok(()),
            FAILED => return Err(Vec::new()),
            RESOLVING => return Err(error(self.model.edges[id.0].range, "circular static endpoint dependency")),
            _ => {}
        }
        self.edge_state[id.0] = RESOLVING;
        let raw = self.raw_edges[id.0].clone();
        let source = self.endpoint(&raw.source);
        let target = self.endpoint(&raw.target);
        match both(source, target) {
            Ok((source, target)) => {
                self.model.edges[id.0].source = source;
                self.model.edges[id.0].target = target;
                self.edge_state[id.0] = RESOLVED;
                Ok(())
            }
            Err(diags) => self.unresolved(ElementId::Edge(id), diags),
        }
    }

    fn resolve_fork(&mut self, id: ForkId) -> Result<()> {
        match self.fork_state[id.0] {
            RESOLVED => return Ok(()),
            FAILED => return Err(Vec::new()),
            RESOLVING => return Err(error(self.model.forks[id.0].range, "circular static fork dependency")),
            _ => {}
        }
        self.fork_state[id.0] = RESOLVING;
        let raw = self.raw_forks[id.0].clone();
        let mut diags = Vec::new();
        let mut failed = false;
        match self.endpoint(&raw.source) {
            Ok(source) => self.model.forks[id.0].source = source,
            Err(more) => {
                failed = true;
                diags.extend(more);
            }
        }
        for edge in self.model.forks[id.0].branches.clone().into_iter().flatten() {
            if let Err(more) = self.resolve_edge(edge) {
                failed = true;
                diags.extend(more);
            }
        }
        if failed {
            return self.unresolved(ElementId::Fork(id), diags);
        }
        self.fork_state[id.0] = RESOLVED;
        Ok(())
    }

    fn junction_input(&mut self, id: JunctionId, expr: &ast::Expr) -> Result<EdgeId> {
        let cursor = self.cursor(expr, Some(ElementId::Junction(id)))?;
        let ElementId::Edge(edge) = self.element(cursor, expr.range)? else {
            return Err(error(expr.range, "junction inputs must reference edges"));
        };
        self.resolve_edge(edge)?;
        if let Some(fork) = self.model.junctions[id.0].fork
            && self.model.edges[edge.0].fork != Some(fork)
        {
            return Err(error(expr.range, "junction input does not belong to the attached fork"));
        }
        Ok(edge)
    }

    fn resolve_junction(&mut self, id: JunctionId) -> Result<()> {
        match self.junction_state[id.0] {
            RESOLVED => return Ok(()),
            FAILED => return Err(Vec::new()),
            RESOLVING => return Err(error(self.model.junctions[id.0].range, "circular static junction dependency")),
            _ => {}
        }
        self.junction_state[id.0] = RESOLVING;
        let raw = self.raw_junctions[id.0].clone();
        let mut diags = Vec::new();
        let mut failed = false;
        let mut inputs = Vec::new();
        for expr in &raw.inputs {
            match self.junction_input(id, expr) {
                Ok(edge) => inputs.push(edge),
                Err(more) => {
                    failed = true;
                    diags.extend(more);
                }
            }
        }
        match self.endpoint(&raw.target) {
            Ok(target) => self.model.junctions[id.0].target = target,
            Err(more) => {
                failed = true;
                diags.extend(more);
            }
        }
        if failed {
            return self.unresolved(ElementId::Junction(id), diags);
        }
        self.model.junctions[id.0].inputs = inputs;
        self.junction_state[id.0] = RESOLVED;
        Ok(())
    }

    fn lower_origin(&mut self, origin: RawOrigin, range: SrcRange) -> Result<PathOrigin> {
        match origin {
            RawOrigin::Node(anchor) => Ok(PathOrigin::Node(self.endpoint(&anchor)?)),
            RawOrigin::Selector(fork, anchor) => {
                // the selector is resolved even under a failed fork so its own mistake is reported
                let target = self.endpoint(&anchor);
                let resolved = self.resolve_fork(fork);
                let (target, ()) = both(target, resolved)?;
                if !self.model.forks[fork.0]
                    .branches
                    .iter()
                    .flatten()
                    .any(|edge| self.model.edges[edge.0].target == target)
                {
                    return Err(error(range, "selector target does not belong to the attached fork"));
                }
                Ok(PathOrigin::Selector { fork, target })
            }
            RawOrigin::Junction(id) => {
                self.resolve_junction(id)?;
                Ok(PathOrigin::Junction(id))
            }
        }
    }

    // every step is resolved even after a failure, `false` means a diagnostic was recorded
    fn resolve_steps(&mut self, steps: &[PathStep]) -> bool {
        let mut resolved = true;
        for step in steps {
            match step {
                PathStep::Edges(edges) => {
                    for edge in edges {
                        if let Err(diags) = self.resolve_edge(*edge) {
                            self.errors.extend(diags);
                            resolved = false;
                        }
                    }
                }
                PathStep::Fork(fork) => {
                    if let Err(diags) = self.resolve_fork(*fork) {
                        self.errors.extend(diags);
                        resolved = false;
                    }
                    let continuations: Vec<_> = self.continuations[fork.0]
                        .clone()
                        .into_iter()
                        .map(|path| self.lower_path(path))
                        .collect();
                    if continuations.iter().all(Option::is_some) {
                        self.model.forks[fork.0].continuations = continuations.into_iter().flatten().collect();
                    } else {
                        resolved = false;
                    }
                }
                PathStep::Arms(arms) => {
                    for arm in arms {
                        resolved &= self.resolve_steps(arm);
                    }
                }
            }
        }
        resolved
    }

    // every step is resolved even after a failure, `None` means a diagnostic was recorded
    fn lower_path(&mut self, path: RawPath) -> Option<GraphPath> {
        let origin = match self.lower_origin(path.origin, path.range) {
            Ok(origin) => Some(origin),
            Err(diags) => {
                self.errors.extend(diags);
                None
            }
        };
        let resolved = self.resolve_steps(&path.steps);
        Some(GraphPath {
            origin: origin.filter(|_| resolved)?,
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

    fn lower_fields(&mut self) {
        let owners: Vec<_> = (0..self.model.nodes.len())
            .map(|i| ElementId::Node(NodeId(i)))
            .chain((0..self.model.edges.len()).map(|i| ElementId::Edge(EdgeId(i))))
            .chain((0..self.model.forks.len()).map(|i| ElementId::Fork(ForkId(i))))
            .chain((0..self.model.junctions.len()).map(|i| ElementId::Junction(JunctionId(i))))
            .collect();
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
                        Err(diags) => {
                            self.errors.extend(diags);
                            self.unchecked.insert(owner);
                        }
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
            // these checks read the junction's inputs and properties
            if self.junction_state[i] != RESOLVED || self.unchecked.contains(&ElementId::Junction(JunctionId(i))) {
                continue;
            }
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
            if !self.unchecked.contains(&ElementId::Node(NodeId(i)))
                && self.primitive(ElementId::Node(NodeId(i))) == "vars"
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
            if self.unchecked.contains(&owner) {
                continue;
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
                                // a guard that failed to lower cannot be compared
                                if [left, right]
                                    .iter()
                                    .any(|edge| self.unchecked.contains(&ElementId::Edge(**edge)))
                                {
                                    continue;
                                }
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

// descriptors written directly in a definition's header braces or body
fn schema_attrs(attrs: &[ast::Attr], inherited: &mut Vec<FieldSchema>) -> Result<()> {
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    for attr in attrs {
        if !seen.insert(&attr.key) {
            errors.extend(error(attr.range, "duplicate schema key"));
        } else if let Err(diags) = schema_field(attr.clone(), inherited) {
            errors.extend(diags);
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

// an entry replacing an inherited one may only restrict it
fn narrows(previous: &FieldSchema, schema: &FieldSchema, range: SrcRange) -> Result<()> {
    if previous.ty != FieldType::Any && schema.ty != previous.ty {
        return Err(error(range, "an extension cannot change an inherited field type"));
    }
    if previous.required && !schema.required {
        return Err(error(range, "an extension cannot make a required field optional"));
    }
    Ok(())
}

fn schema_field(field: ast::Attr, inherited: &mut Vec<FieldSchema>) -> Result<()> {
    {
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
            narrows(previous, &schema, field.range)?;
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

// the key of a property or field, without its value
fn key_range(field: &ast::Attr) -> SrcRange {
    SrcRange {
        end: field.range.start + field.key.len(),
        ..field.range
    }
}

fn distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let substitution = diagonal + usize::from(a != *b);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[right.len()]
}

// names the closest known name when a misspelling is within two edits of it
fn suggestion<'a>(name: &str, known: impl Iterator<Item = &'a str>) -> String {
    known
        .map(|candidate| (distance(name, candidate), candidate))
        .filter(|(distance, candidate)| *distance <= 2 && *distance < candidate.len())
        .min()
        .map(|(_, candidate)| format!(", did you mean `{candidate}`?"))
        .unwrap_or_default()
}

fn construct_named(name: &str) -> Option<Construct> {
    [Construct::Node, Construct::Edge, Construct::Fork, Construct::Junction]
        .into_iter()
        .find(|construct| construct.name() == name)
}

// what a definition head declares
#[derive(Clone, Copy, PartialEq, Eq)]
enum Defined {
    Kind(Construct),
    Trait,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    Extends,
    Has,
    On,
}

#[derive(Clone)]
struct Relationship {
    relation: Relation,
    target: String,
    // the connector and its target
    range: SrcRange,
}

struct DefinitionEntry<'a> {
    name: &'a str,
    defined: Defined,
    head: &'a ast::GraphNodeDecl,
    relationships: Vec<Relationship>,
    range: SrcRange,
}

// kinds and traits resolve together because they share one namespace
#[derive(Default)]
struct Definitions<'a> {
    entries: Vec<DefinitionEntry<'a>>,
    names: HashMap<&'a str, usize>,
    // position within the model's kind or trait table
    ids: Vec<usize>,
    states: Vec<u8>,
    kinds: Vec<Option<Kind>>,
    traits: Vec<Option<Trait>>,
    // the trait entry that supplied a property (true) or field (false) of a kind
    origins: Vec<HashMap<(bool, String), usize>>,
}

impl<'a> Definitions<'a> {
    fn add(
        &mut self,
        name: &'a str,
        defined: Defined,
        head: &'a ast::GraphNodeDecl,
        relationships: Vec<Relationship>,
        range: SrcRange,
        broken: bool,
    ) {
        let id = self
            .entries
            .iter()
            .filter(|entry| (entry.defined == Defined::Trait) == (defined == Defined::Trait))
            .count();
        self.names.insert(name, self.entries.len());
        self.entries.push(DefinitionEntry {
            name,
            defined,
            head,
            relationships,
            range,
        });
        self.ids.push(id);
        // malformed relationships were reported where they were read
        self.states.push(if broken { FAILED } else { PENDING });
        self.kinds.push(None);
        self.traits.push(None);
        self.origins.push(HashMap::new());
    }

    fn enter(&mut self, index: usize, cycle: &str) -> Result<()> {
        match self.states[index] {
            FAILED => Err(Vec::new()),
            RESOLVING => Err(error(self.entries[index].range, cycle)),
            _ => {
                self.states[index] = RESOLVING;
                Ok(())
            }
        }
    }

    fn resolve_trait(&mut self, index: usize) -> Result<Trait> {
        if let Some(resolved) = &self.traits[index] {
            return Ok(resolved.clone());
        }
        self.enter(index, "circular trait extension")?;
        let resolved = self.resolve_trait_definition(index);
        self.states[index] = if resolved.is_ok() { RESOLVED } else { FAILED };
        self.traits[index] = resolved.as_ref().ok().cloned();
        resolved
    }

    fn resolve_trait_definition(&mut self, index: usize) -> Result<Trait> {
        let entry = &self.entries[index];
        let (name, head, range) = (entry.name, entry.head, entry.range);
        let mut errors = Vec::new();
        let mut base = None;
        let mut extended = false;
        let mut on: Vec<(Construct, SrcRange)> = Vec::new();
        for relationship in &entry.relationships {
            let target = relationship.target.as_str();
            match relationship.relation {
                Relation::Has => errors.extend(error(
                    relationship.range,
                    "a trait cannot have traits, it may extend one trait",
                )),
                Relation::On => match construct_named(target) {
                    Some(construct) => {
                        if !on.iter().any(|(listed, _)| *listed == construct) {
                            on.push((construct, relationship.range));
                        }
                    }
                    None => errors.extend(error(
                        relationship.range,
                        "`on` targets a construct: node, edge, fork or junction",
                    )),
                },
                Relation::Extends if extended => {
                    errors.extend(error(relationship.range, "a trait extends at most one trait"));
                }
                Relation::Extends => {
                    extended = true;
                    match self.names.get(target) {
                        Some(&found) if self.entries[found].defined == Defined::Trait => base = Some(found),
                        None if builtin_construct(target).is_none() => {
                            errors.extend(error(relationship.range, format!("unknown trait `{target}`")));
                        }
                        _ => errors.extend(error(
                            relationship.range,
                            format!("a trait can only extend a trait, `{target}` is a kind"),
                        )),
                    }
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut resolved = match base {
            Some(base) => {
                let mut resolved = self.resolve_trait(base)?;
                for (construct, at) in &on {
                    if !resolved.on.contains(construct) {
                        errors.extend(error(
                            *at,
                            format!(
                                "trait `{}` is not on {}, an extension can only narrow where it applies",
                                resolved.name,
                                construct.name()
                            ),
                        ));
                    }
                }
                resolved.base = Some(TraitId(self.ids[base]));
                resolved
            }
            None => Trait {
                name: String::new(),
                base: None,
                on: Vec::new(),
                props: Vec::new(),
                fields: Vec::new(),
                range,
            },
        };
        resolved.name = name.to_owned();
        resolved.range = range;
        if !on.is_empty() {
            resolved.on = on.into_iter().map(|(construct, _)| construct).collect();
        }
        if resolved.on.is_empty() {
            errors.extend(error(range, "a trait needs at least one `on` relationship"));
        }
        if let Err(diags) = schema_attrs(head.header.props.as_deref().unwrap_or(&[]), &mut resolved.props) {
            errors.extend(diags);
        }
        if let Err(diags) = schema_attrs(head.body.as_deref().unwrap_or(&[]), &mut resolved.fields) {
            errors.extend(diags);
        }
        if !resolved.fields.is_empty() && resolved.on.iter().any(|construct| *construct != Construct::Node) {
            errors.extend(error(
                range,
                "only nodes have content fields, a trait with fields applies to node alone",
            ));
        }
        if resolved.fields.iter().any(|field| field.name == "props") {
            errors.extend(error(range, "props is reserved for node properties"));
        }
        if errors.is_empty() { Ok(resolved) } else { Err(errors) }
    }

    fn resolve_kind(&mut self, index: usize, construct: Construct) -> Result<Kind> {
        if let Some(kind) = &self.kinds[index] {
            return Ok(kind.clone());
        }
        self.enter(index, "circular kind extension")?;
        let resolved = self.resolve_kind_definition(index, construct);
        self.states[index] = if resolved.is_ok() { RESOLVED } else { FAILED };
        self.kinds[index] = resolved.as_ref().ok().cloned();
        resolved
    }

    fn resolve_kind_definition(&mut self, index: usize, construct: Construct) -> Result<Kind> {
        let entry = &self.entries[index];
        let (name, head, range) = (entry.name, entry.head, entry.range);
        let relationships = entry.relationships.clone();
        let mut errors = Vec::new();
        let mut base = None;
        let mut has = Vec::new();
        for relationship in &relationships {
            match relationship.relation {
                Relation::On => errors.extend(error(relationship.range, "`on` belongs to a trait definition")),
                Relation::Has => has.push(relationship),
                Relation::Extends if base.is_some() => {
                    errors.extend(error(relationship.range, "a kind extends one base"));
                }
                Relation::Extends => base = Some(relationship),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let crosses = |at: SrcRange| error(at, "kind extension crosses graph construct categories");
        let root = |base: &str, at: SrcRange| Kind {
            name: String::new(),
            base: base.to_owned(),
            base_kind: None,
            construct,
            primitive: base.to_owned(),
            direction: (construct == Construct::Edge).then(|| primitive_direction(base)),
            props: primitive_properties(construct, base, at),
            fields: Vec::new(),
            // the built-in node kinds take any content field
            open: true,
            traits: Vec::new(),
            range: at,
        };
        let mut origins = HashMap::new();
        let mut kind = match base {
            None => root(construct.name(), range),
            Some(relationship) => {
                let target = relationship.target.as_str();
                match self.names.get(target).copied() {
                    Some(found) => {
                        let Defined::Kind(base_construct) = self.entries[found].defined else {
                            return Err(error(
                                relationship.range,
                                format!("a kind cannot extend a trait, attach `{target}` with has"),
                            ));
                        };
                        if base_construct != construct {
                            return Err(crosses(relationship.range));
                        }
                        let mut kind = self.resolve_kind(found, base_construct)?;
                        kind.base_kind = Some(KindId(self.ids[found]));
                        kind.base = target.to_owned();
                        origins = self.origins[found].clone();
                        kind
                    }
                    None => {
                        let found = builtin_construct(target)
                            .ok_or_else(|| error(relationship.range, format!("unknown base kind `{target}`")))?;
                        if found != construct {
                            return Err(crosses(relationship.range));
                        }
                        root(target, range)
                    }
                }
            }
        };
        kind.name = name.to_owned();
        kind.range = range;
        // a dependency that already reported its own failure fails this kind without another diagnostic
        let mut failed = false;
        for relationship in has {
            let target = relationship.target.as_str();
            let found = match self.names.get(target).copied() {
                Some(found) if self.entries[found].defined == Defined::Trait => found,
                None if builtin_construct(target).is_none() => {
                    errors.extend(error(relationship.range, format!("unknown trait `{target}`")));
                    continue;
                }
                _ => {
                    errors.extend(error(relationship.range, format!("`{target}` is a kind, not a trait")));
                    continue;
                }
            };
            let attached = match self.resolve_trait(found) {
                Ok(attached) => attached,
                Err(diags) => {
                    errors.extend(diags);
                    failed = true;
                    continue;
                }
            };
            if !attached.on.contains(&construct) {
                errors.extend(error(
                    relationship.range,
                    format!("trait `{target}` is not on {}", construct.name()),
                ));
                continue;
            }
            if kind.traits.contains(&TraitId(self.ids[found])) {
                continue;
            }
            // the trait and every trait it extends
            let mut lineage = vec![found];
            let mut next = attached.base;
            while let Some(base) = next {
                let Some(position) = (0..self.entries.len())
                    .find(|&entry| self.entries[entry].defined == Defined::Trait && self.ids[entry] == base.0)
                else {
                    break;
                };
                lineage.push(position);
                next = self.traits[position].as_ref().and_then(|extended| extended.base);
            }
            for (property, supplied) in [(true, &attached.props), (false, &attached.fields)] {
                let entries = if property { &mut kind.props } else { &mut kind.fields };
                let what = if property { "property" } else { "field" };
                for field in supplied {
                    let key = (property, field.name.clone());
                    let Some(position) = entries.iter().position(|entry| entry.name == field.name) else {
                        entries.push(field.clone());
                        origins.insert(key, found);
                        continue;
                    };
                    match origins.get(&key).copied() {
                        // an extended trait already supplied this entry, the extension restricts it
                        Some(origin) if lineage.contains(&origin) => {
                            match narrows(&entries[position], field, relationship.range) {
                                Ok(()) => {
                                    entries[position] = field.clone();
                                    origins.insert(key, found);
                                }
                                Err(diags) => errors.extend(diags),
                            }
                        }
                        Some(origin) => errors.extend(error(
                            relationship.range,
                            format!(
                                "{what} `{}` is declared by both trait `{}` and trait `{target}`",
                                field.name, self.entries[origin].name
                            ),
                        )),
                        None => errors.extend(error(
                            relationship.range,
                            format!(
                                "trait `{target}` declares {what} `{}`, which `{name}` already has from its base",
                                field.name
                            ),
                        )),
                    }
                }
            }
            for member in lineage {
                let id = TraitId(self.ids[member]);
                if !kind.traits.contains(&id) {
                    kind.traits.push(id);
                }
            }
        }
        if failed && errors.is_empty() {
            return Err(errors);
        }
        let mut props = head.header.props.as_deref().unwrap_or(&[]).to_vec();
        // a declared kind is closed unless it says otherwise, and a closed base stays closed
        let base_open = std::mem::replace(&mut kind.open, false);
        if construct == Construct::Node {
            // open is a reserved header key on node kinds, not a property descriptor
            let mut seen = false;
            props.retain(|field| {
                if field.key != "open" {
                    return true;
                }
                match field.value.kind {
                    _ if seen => errors.extend(error(field.range, "duplicate schema key")),
                    ast::ExprKind::Bool(true) if !base_open => {
                        errors.extend(error(field.range, "a kind cannot reopen the closed root of its base"));
                    }
                    ast::ExprKind::Bool(open) => kind.open = open,
                    _ => errors.extend(error(field.range, "open must be a literal boolean")),
                }
                seen = true;
                false
            });
        }
        if construct == Construct::Edge {
            // direction is a reserved header key on edge kinds, not a property descriptor
            let mut seen = false;
            props.retain(|field| {
                if field.key != "direction" {
                    return true;
                }
                if seen {
                    errors.extend(error(field.range, "duplicate schema key"));
                } else if let Err(diags) = kind_direction(field, &mut kind) {
                    errors.extend(diags);
                }
                seen = true;
                false
            });
        }
        if let Err(diags) = schema_attrs(&props, &mut kind.props) {
            errors.extend(diags);
        }
        let body = head.body.as_deref().unwrap_or(&[]);
        if construct != Construct::Node && !body.is_empty() {
            errors.extend(error(body[0].range, "only node kinds have content fields"));
        } else if let Err(diags) = schema_attrs(body, &mut kind.fields) {
            errors.extend(diags);
        }
        if kind.fields.iter().any(|field| field.name == "props") {
            errors.extend(error(kind.range, "props is reserved for node properties"));
        }
        if errors.is_empty() {
            self.origins[index] = origins;
            Ok(kind)
        } else {
            Err(errors)
        }
    }
}

fn step_range(step: &ast::GraphStep) -> SrcRange {
    match step {
        ast::GraphStep::Edge(edge) => edge.range,
        ast::GraphStep::Fork(fork) => fork.range,
        ast::GraphStep::Arms(arms) => step_range(&arms[0][0]),
    }
}

// every relationship is one edge from the definition head
fn relationships(steps: &[ast::GraphStep]) -> (Vec<Relationship>, Diags) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let arms: Vec<&[ast::GraphStep]> = match steps {
        [ast::GraphStep::Arms(arms)] => arms.iter().map(Vec::as_slice).collect(),
        _ => vec![steps],
    };
    for arm in arms {
        let Some((first, rest)) = arm.split_first() else { continue };
        match first {
            ast::GraphStep::Edge(step) => match relationship(step) {
                Ok(relationship) => found.push(relationship),
                Err(diags) => errors.extend(diags),
            },
            other => errors.extend(error(step_range(other), "a definition takes relationship edges only")),
        }
        if let Some(next) = rest.first() {
            errors.extend(error(
                step_range(next),
                "a relationship starts at the definition head, write each one on its own aligned line",
            ));
        }
    }
    (found, errors)
}

fn relationship(step: &ast::GraphEdgeStep) -> Result<Relationship> {
    let [header] = step.edges.as_slice() else {
        return Err(error(step.range, "a relationship edge holds one relationship"));
    };
    let relation = match header.kind.as_str() {
        "extends" => Relation::Extends,
        "has" => Relation::Has,
        "on" => Relation::On,
        other => {
            return Err(error(
                header.range,
                format!("unknown relationship `{other}`, expected extends, has or on"),
            ));
        }
    };
    if header.name.is_some() || header.props.is_some() {
        return Err(error(header.range, "a relationship edge has no name and no properties"));
    }
    if step.cycle || step.direction != Direction::Directed {
        return Err(error(step.range, "a relationship edge is written -[:kind]->"));
    }
    let target = match &step.target {
        ast::GraphNode::Reference(ast::Expr {
            kind: ast::ExprKind::GraphRef { target, .. },
            ..
        }) => match &target.kind {
            ast::ExprKind::Ref { path } if path.len() == 1 => Some(path[0].clone()),
            _ => None,
        },
        _ => None,
    };
    let Some(target) = target else {
        return Err(error(step.range, "a relationship target references a definition by name"));
    };
    Ok(Relationship {
        relation,
        target,
        range: step.range,
    })
}

fn kind_direction(field: &ast::Attr, kind: &mut Kind) -> Result<()> {
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
    Ok(())
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
    // no phase stops the next one: a failed element is marked, and later checks that
    // depend on it are skipped, so each independent mistake is reported once
    modeler.definitions(&ast.definitions);
    let paths: Vec<_> = ast.paths.iter().map(|path| modeler.path(path, None)).collect();
    if paths.is_empty() && ast.definitions.is_empty() {
        return Err(error(SrcRange::new(0, 0), "expected at least one graph statement"));
    }
    let mut resolved = Vec::new();
    for path in paths {
        resolved.extend(modeler.lower_path(path));
    }
    modeler.model.paths = resolved;
    modeler.lower_fields();
    if let Err(diags) = modeler.validate_primitives() {
        modeler.errors.extend(diags);
    }
    let states = [&modeler.edge_state, &modeler.fork_state, &modeler.junction_state];
    let unresolved: HashSet<_> = (states[0].iter().enumerate())
        .filter_map(|(i, state)| (*state != RESOLVED).then_some(ElementId::Edge(EdgeId(i))))
        .chain(
            (states[1].iter().enumerate()).filter_map(|(i, state)| (*state != RESOLVED).then_some(ElementId::Fork(ForkId(i)))),
        )
        .chain(
            (states[2].iter().enumerate())
                .filter_map(|(i, state)| (*state != RESOLVED).then_some(ElementId::Junction(JunctionId(i)))),
        )
        .collect();
    debug_assert!(unresolved.is_empty() || !modeler.errors.is_empty());
    if let Err(diags) = validate_graph(&mut modeler.model, &modeler.unchecked, &unresolved) {
        modeler.errors.extend(diags);
    }
    if modeler.errors.is_empty() {
        Ok(modeler.model)
    } else {
        Err(dedup(modeler.errors))
    }
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
        let construct = match self.owner {
            ElementId::Node(_) => Construct::Node,
            ElementId::Edge(_) => Construct::Edge,
            ElementId::Fork(_) => Construct::Fork,
            ElementId::Junction(_) => Construct::Junction,
        };
        let base = primitive(self.model, self.owner);
        let descriptors = primitive_properties(construct, base, self.range);
        let mut errors = Vec::new();
        for descriptor in &descriptors {
            let checked = match self.prop(&descriptor.name) {
                Some(value) => self.typed(value, descriptor.ty),
                None if descriptor.required => self.error(format!("missing required property `{}`", descriptor.name)),
                None => Ok(()),
            };
            if let Err(diags) = checked {
                errors.extend(diags);
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    // `None` when a selected edge has no resolved endpoints to validate against
    fn network(&self, node: NodeId, unresolved: &HashSet<ElementId>) -> Result<Option<Network>> {
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
        if edges.iter().any(|id| unresolved.contains(&ElementId::Edge(*id))) {
            return Ok(None);
        }
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
        Ok(Some(network))
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

// each check reports its own failure and the rest still run, checks that read an
// unresolved or unchecked element are skipped because its mistake is already reported
fn validate_graph(model: &mut Model, unchecked: &HashSet<ElementId>, unresolved: &HashSet<ElementId>) -> Result<()> {
    let mut unchecked = unchecked.clone();
    let mut errors = Vec::new();
    let mut keep = |result: Result<()>| {
        if let Err(diags) = result {
            errors.extend(diags);
        }
    };
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
        if !unchecked.contains(&owner)
            && let Err(diags) = (GraphCheck { model, owner, range }).properties()
        {
            // the kind-specific checks below would describe the same property mistakes again
            unchecked.insert(owner);
            keep(Err(diags));
        }
    }
    let mut networks = Vec::new();
    let mut incidences: HashMap<NodeId, Vec<(NodeId, String)>> = HashMap::new();
    // false once an incidence could not be attributed, relation counts would then be wrong
    let mut incidences_complete = true;
    for (i, edge) in model.edges.iter().enumerate() {
        let owner = ElementId::Edge(EdgeId(i));
        let check = GraphCheck {
            model,
            owner,
            range: edge.range,
        };
        let known = !unchecked.contains(&owner);
        if known && let Some(limit) = check.prop("max_traversals") {
            keep((|| {
                if !edge.cycle {
                    return check.error("max_traversals requires a cycle edge");
                }
                check.typed(limit, FieldType::Number)?;
                if expr::constant(limit) {
                    check.count(limit, "max_traversals")?;
                }
                Ok(())
            })());
        }
        if primitive(model, owner) == "incidence" {
            if !known || unresolved.contains(&owner) {
                incidences_complete = false;
                continue;
            }
            let left = primitive(model, ElementId::Node(edge.source)) == "relation";
            let right = primitive(model, ElementId::Node(edge.target)) == "relation";
            let role = if left == right {
                check.error("incidence requires exactly one relation endpoint")
            } else {
                check.choice("role", "member", &["member", "input", "output"])
            };
            match role {
                Ok(role) => {
                    let (relation, participant) = if left {
                        (edge.source, edge.target)
                    } else {
                        (edge.target, edge.source)
                    };
                    incidences.entry(relation).or_default().push((participant, role.to_owned()));
                }
                Err(diags) => {
                    incidences_complete = false;
                    keep(Err(diags));
                }
            }
        }
    }
    for (i, node) in model.nodes.iter().enumerate() {
        let id = NodeId(i);
        let owner = ElementId::Node(id);
        if unchecked.contains(&owner) {
            continue;
        }
        let check = GraphCheck {
            model,
            owner,
            range: node.range,
        };
        match primitive(model, owner) {
            "network" => match check.network(id, unresolved) {
                Ok(network) => networks.extend(network),
                Err(diags) => keep(Err(diags)),
            },
            "relation" if incidences_complete => {
                let entries = incidences.get(&id).map(Vec::as_slice).unwrap_or_default();
                let inputs = entries.iter().filter(|(_, role)| role == "input").count();
                let outputs = entries.iter().filter(|(_, role)| role == "output").count();
                if inputs + outputs > 0 && (inputs == 0 || outputs == 0 || inputs + outputs != entries.len()) {
                    keep(check.error("directed relations require input and output incidences without member roles"));
                }
                keep(check.boolean("unique", true).and_then(|unique| {
                    let mut seen = HashSet::new();
                    if unique && entries.iter().any(|entry| !seen.insert(entry)) {
                        return check.error("duplicate relation participant in the same role");
                    }
                    Ok(())
                }));
                for (name, count) in [("participants", entries.len()), ("inputs", inputs), ("outputs", outputs)] {
                    keep(check.bounds(name).and_then(|bounds| {
                        if !within(count, bounds) {
                            return check.error(format!("relation violates {name} bounds"));
                        }
                        Ok(())
                    }));
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
        let known = !unchecked.contains(&owner);
        let branch_checks = || {
            fork.branches
                .iter()
                .flatten()
                .filter(|edge| !unchecked.contains(&ElementId::Edge(**edge)))
                .map(|edge| GraphCheck {
                    model,
                    owner: ElementId::Edge(*edge),
                    range: model.edges[edge.0].range,
                })
        };
        match primitive(model, owner) {
            "each" | "repeat" => {
                if known {
                    keep((|| {
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
                        Ok(())
                    })());
                }
                for edge_check in branch_checks() {
                    if edge_check.prop("when").is_some() {
                        keep(edge_check.error("each and repeat run every listed branch and do not allow branch guards"));
                    }
                }
            }
            "weighted" => {
                if known && check.prop("count").is_some() {
                    keep(check.error("weighted selects exactly one branch and does not accept count"));
                }
                for branch in &fork.branches {
                    let mut branch_weight = None;
                    for edge in branch {
                        if unchecked.contains(&ElementId::Edge(*edge)) {
                            continue;
                        }
                        let edge_check = GraphCheck {
                            model,
                            owner: ElementId::Edge(*edge),
                            range: model.edges[edge.0].range,
                        };
                        let weight = match edge_check.prop("weight") {
                            None => {
                                keep(edge_check.error("weighted branches require an explicit weight"));
                                continue;
                            }
                            Some(Value::Num(Number::Int(value))) => Some(*value as f64),
                            Some(Value::Num(Number::Float(value))) => Some(*value),
                            Some(value) if !expr::constant(value) => None,
                            _ => {
                                keep(edge_check.error("weighted branch weight must be a nonnegative number"));
                                continue;
                            }
                        };
                        if let Some(weight) = weight {
                            if !weight.is_finite() || weight < 0.0 {
                                keep(edge_check.error("weighted branch weight must be finite and nonnegative"));
                                continue;
                            }
                            if branch_weight.is_some_and(|previous| previous != weight) {
                                keep(edge_check.error("all edges in a weighted branch must have the same weight"));
                                continue;
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
        if !unchecked.contains(&owner)
            && matches!(
                primitive(model, owner),
                "accumulate" | "merge" | "first" | "quorum" | "reduce"
            )
        {
            let check = GraphCheck {
                model,
                owner,
                range: junction.range,
            };
            keep(check.choice("observe", "arrival", &["arrival", "result"]).map(drop));
            keep(check.choice("on_failure", "fail", &["fail", "skip"]).map(drop));
            keep(check.choice("on_cancel", "fail", &["fail", "skip"]).map(drop));
            if primitive(model, owner) == "reduce" {
                keep(check.required("initial").map(drop));
                keep(check.required("combine").map(drop));
                keep(check.choice("order", "source", &["source", "arrival"]).map(drop));
            }
        }
    }
    model.networks = networks;
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
