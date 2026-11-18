//! Walking a world graph.
//!
//! A node whose kind derives from `walk` is built by the steps it leads to. The walk
//! visits what leaves each node in the order written, depth first, along `next`
//! edges. A `weighted` fork takes one branch by a draw and an `exclusive` fork takes
//! the branch whose guard holds. Each visited step gives one value, its `gives`
//! field, and the walk reads them in order as `context.steps`.
//!
//! Nothing interprets a graph when a trace is generated. [`supply`] turns each walk
//! into a value in the language's own model: an array where a step is what it gives
//! and a fork is a call that picks one branch. The provider that lowers the model
//! lowers that value like any other.
//!
//! A draw is addressed, not sequenced. It is a function of the seed, the trace index
//! and the fork, so it is the same wherever and however often the walk is read, and
//! the same in data generation and in a generated agent given the seed.
//!
//! A walk may say what it is drawn for with its `per` property. That value then
//! stands in for the trace index, so every trace that gives the same value sees the
//! same walk: one conversation, one customer, or the whole run.

use crate::mocking::data::Value;
use crate::mocking::{EvaluateError, Evaluation};
use crate::providers::{Supplied, Supply};
use bts_lang::{
    Array, ArrayElem, BindingId, Diag, DiagPhase, Diags, Direction, EdgeId, ElementId, FieldType, ForkId, Func, Model, NodeId,
    Object, Primitive, SrcRange, Value as Graph,
};
use std::collections::{BTreeMap, BTreeSet};

/// The kind every walk derives from.
pub const WALK: &str = "walk";
/// The context key a walk reads its steps through.
pub const STEPS: &str = "steps";
/// The field a step hands to its walk.
pub const GIVES: &str = "gives";
/// The property that says what a walk is drawn for, in place of the trace.
pub const PER: &str = "per";
/// The context key a walk reads its draws through.
pub const DRAWS_KEY: &str = "draws";
/// The internal function that gives a walk its draws.
pub const DRAWS: &str = "world.draws";
/// The context key a network reads what its walks give through.
pub const MEMBERS: &str = "members";
/// The context key a walk reads what it is related to through.
pub const RELATED: &str = "related";
/// The internal function a weighted fork lowers to. No author can write the name.
pub const PICK: &str = "world.pick";
/// The kind of several traces in a row that belong together.
pub const SERIES: &str = "series";
/// The kinds that take a verb, the property they take it in, and the field a sentence starts from.
const STATEMENT: &str = "statement";
const PART: &str = "part";
const SENTENCE: &str = "sentence";
const VERB: &str = "verb";
const DEFAULTS: &str = "defaults";
/// The context key a series reads its identifier through.
pub const IDENTITY: &str = "identity";
/// The internal function a series' identifier lowers to.
pub const ID: &str = "world.id";
/// The context key a series reads its place in the run through.
pub const PLACE: &str = "place";
/// The internal function a series' place lowers to.
pub const TURN: &str = "world.turn";
/// How many traces a series of varying length counts back through to find its place.
///
/// The last series of each block ends where the block does.
pub const BLOCK: u64 = 256;

// what a series' place holds, so reads into it are typed
const PLACE_SHAPE: bts_lang::ResultShape = bts_lang::ResultShape::Object(&[
    ("number", bts_lang::ResultShape::Scalar(FieldType::Number)),
    ("turn", bts_lang::ResultShape::Scalar(FieldType::Number)),
    ("length", bts_lang::ResultShape::Scalar(FieldType::Number)),
]);

// what leaves a node, in the order written
#[derive(Clone, Copy)]
enum Leaving {
    Edge(EdgeId),
    Fork(ForkId),
}

fn roots(model: &Model) -> impl Iterator<Item = NodeId> + '_ {
    (model.nodes.iter().enumerate())
        .filter(|(_, node)| model.derives(&node.kind, WALK))
        .map(|(index, _)| NodeId(index))
}

fn each_series(model: &Model) -> impl Iterator<Item = NodeId> + '_ {
    (model.nodes.iter().enumerate())
        .filter(|(_, node)| model.derives(&node.kind, SERIES))
        .map(|(index, _)| NodeId(index))
}

fn leaving(model: &Model, node: NodeId) -> Vec<Leaving> {
    let mut found: Vec<(SrcRange, Leaving)> = Vec::new();
    for (index, edge) in model.edges.iter().enumerate() {
        if edge.source == node && edge.fork.is_none() && edge.primitive == Primitive::Next {
            found.push((edge.range, Leaving::Edge(EdgeId(index))));
        }
    }
    for (index, fork) in model.forks.iter().enumerate() {
        if fork.source == node {
            found.push((fork.range, Leaving::Fork(ForkId(index))));
        }
    }
    found.sort_by_key(|(range, _)| (range.source, range.start));
    found.into_iter().map(|(_, leaving)| leaving).collect()
}

/// The elements the world provider checks against its schema: what it walks, without the walks and their steps.
///
/// A step is any node that gives something, whatever its kind, and [`supply`] reports one that gives nothing.
/// A walk is a walk by its kind, and a key its kind requires that nothing leads to it with is null.
pub fn checked(model: &Model) -> Vec<ElementId> {
    (elements(model).into_iter())
        .filter(|element| match element {
            ElementId::Node(node) => {
                let kind = &model.nodes[node.0].kind;
                model.derives(kind, SERIES) || matches!(model.nodes[node.0].primitive, Primitive::Relation | Primitive::Network)
            }
            _ => true,
        })
        .collect()
}

/// The elements the world provider walks: every walk, and the steps, edges and forks it leads to.
///
/// A junction that joins any of them is included, so it is reported as unsupported.
pub fn elements(model: &Model) -> Vec<ElementId> {
    let mut nodes = BTreeSet::new();
    let mut pending: Vec<NodeId> = roots(model).chain(each_series(model)).collect();
    let mut elements = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !nodes.insert(node) {
            continue;
        }
        elements.insert(ElementId::Node(node));
        for (index, edge) in model.edges.iter().enumerate() {
            if edge.source == node {
                elements.insert(ElementId::Edge(EdgeId(index)));
                if edge.primitive == Primitive::Next {
                    pending.push(edge.target);
                }
            }
        }
        for (index, fork) in model.forks.iter().enumerate() {
            if fork.source == node {
                elements.insert(ElementId::Fork(ForkId(index)));
            }
        }
    }
    for network in &model.networks {
        if (network.nodes.iter()).any(|member| model.derives(&model.nodes[member.0].kind, WALK)) {
            elements.insert(ElementId::Node(network.node));
        }
    }
    for (relation, joined) in relations(model) {
        elements.insert(ElementId::Node(relation));
        elements.extend(joined.into_iter().map(|(edge, _)| ElementId::Edge(edge)));
    }
    for (index, junction) in model.junctions.iter().enumerate() {
        let joins = nodes.contains(&junction.target)
            || (junction.inputs.iter()).any(|input| nodes.contains(&model.edges[input.0].source));
        if joins {
            elements.insert(ElementId::Junction(bts_lang::JunctionId(index)));
        }
    }
    elements.into_iter().collect()
}

/// Reports each verb written as a word the provider has no forms for.
///
/// A sentence is put into words when a trace reads it, which is after a shape has
/// been checked and only on the branch a draw takes. A verb written as a constant
/// on a `statement`, a `part` or a sentence's `defaults` is known when the shape
/// compiles, so it is looked up there and reported where it is written.
pub fn verbs(model: &Model) -> Diags {
    let mut diags = Diags::new();
    for binding in &model.bindings {
        let ElementId::Node(node) = binding.owner else { continue };
        let kind = &model.nodes[node.0].kind;
        let written = if binding.property {
            binding.path == [VERB] && (model.derives(kind, STATEMENT) || model.derives(kind, PART))
        } else {
            binding.path == [DEFAULTS, VERB] && model.derives(kind, SENTENCE)
        };
        if written
            && binding.range.source == 0
            && let Graph::Str(verb) = &binding.value
            && super::english::Verb::lookup(verb).is_none()
        {
            diags.push(fail(
                binding.range,
                format!(
                    "unknown English verb {verb:?}, write its forms as {{base: {verb:?}, third_person_singular: \"...\", past: \"...\", past_participle: \"...\", present_participle: \"...\"}}"
                ),
            ));
        }
    }
    diags
}

fn fail(range: SrcRange, what: impl Into<String>) -> Diag {
    Diag {
        when: DiagPhase::Validation,
        what: what.into(),
        r#where: range,
    }
}

struct Walker<'a> {
    model: &'a Model,
    root: NodeId,
    name: String,
    // what the walk is drawn for, when it says
    per: Option<Graph>,
    // the steps being visited, to find a path that returns to itself
    path: Vec<NodeId>,
    // each fork's place in the walk, so a fork reached along two paths draws once
    forks: Vec<ForkId>,
    // the round of each repeat fork being walked, outermost first
    rounds: Vec<usize>,
    // the walks that are steps of this one
    inner: Vec<NodeId>,
}

impl Walker<'_> {
    fn prop(&self, edge: EdgeId, name: &str) -> Option<BindingId> {
        (self.model.edges[edge.0].props.iter().copied()).find(|id| self.model.bindings[id.0].path == [name])
    }

    // the value a binding holds, read through references to other bindings
    fn resolved<'v>(&'v self, mut value: &'v Graph) -> &'v Graph {
        while let Graph::BindingRef(id, _) = value {
            value = &self.model.bindings[id.0].value;
        }
        value
    }

    // what a step gives, without the keys it was not written with
    fn gives(&self, step: NodeId) -> Result<Graph, Diag> {
        let node = &self.model.nodes[step.0];
        let binding = (node.fields.iter())
            .map(|id| &self.model.bindings[id.0])
            .find(|binding| binding.path == [GIVES])
            .ok_or_else(|| {
                fail(
                    node.range,
                    format!("this step gives nothing to `{}`, its kind needs a `{GIVES}` field", self.name),
                )
            })?;
        Ok(match &binding.value {
            Graph::Object(object) => Graph::Object(Object {
                elem: (object.elem.iter())
                    .filter(|field| !matches!(self.resolved(&field.value), Graph::Null))
                    .cloned()
                    .collect(),
            }),
            value => value.clone(),
        })
    }

    // one step and everything it leads to
    fn step(&mut self, edge: EdgeId) -> Result<Vec<ArrayElem>, Diag> {
        let edge = &self.model.edges[edge.0];
        if edge.kind != "next" || edge.primitive != Primitive::Next {
            return Err(fail(edge.range, format!("a walk follows `next` edges, not `{}`", edge.kind)));
        }
        if edge.direction != Direction::Directed {
            return Err(fail(edge.range, "a walk follows directed edges"));
        }
        if edge.cycle || self.path.contains(&edge.target) || self.root == edge.target {
            return Err(fail(edge.range, "a walk cannot return to a step it has visited yet"));
        }
        if edge.fork.is_none()
            && let Some(id) = edge.props.first()
        {
            return Err(fail(
                self.model.bindings[id.0].range,
                "a step outside a fork is always taken, so its edge takes no properties",
            ));
        }
        let target = edge.target;
        // a step that gives null adds nothing, which is how a step holds a fork and no value
        let mut steps = match self.gives(target)? {
            Graph::Null => vec![],
            gives => vec![ArrayElem::Item(gives)],
        };
        // a walk that is a step gives what it built, and what it leads to builds it, not this walk
        if self.model.derives(&self.model.nodes[target.0].kind, WALK) {
            // one walk is built once, so inside a repeat every round would give the same thing
            if !self.rounds.is_empty() {
                return Err(fail(
                    self.model.nodes[target.0].range,
                    "a walk inside a repeat is built once, so every round would give the same. Repeat an `either` with a fork under it",
                ));
            }
            self.inner.push(target);
        } else {
            steps.extend(self.leads(target)?);
        }
        Ok(steps)
    }

    // what a node leads to, in the order written
    fn leads(&mut self, node: NodeId) -> Result<Vec<ArrayElem>, Diag> {
        self.path.push(node);
        let mut steps = Vec::new();
        for leaving in leaving(self.model, node) {
            match leaving {
                Leaving::Edge(edge) => steps.extend(self.step(edge)?),
                Leaving::Fork(fork) => steps.push(ArrayElem::Spread(self.fork(fork)?)),
            }
        }
        self.path.pop();
        // an edge of another primitive leaves the walk without saying where it goes
        if let Some(edge) = (self.model.edges.iter())
            .find(|edge| edge.source == node && edge.fork.is_none() && edge.primitive != Primitive::Next)
        {
            return Err(fail(edge.range, format!("a walk follows `next` edges, not `{}`", edge.kind)));
        }
        Ok(steps)
    }

    // the rounds a fork sits in, so each round draws for itself
    fn round(&self) -> String {
        self.rounds.iter().map(|round| format!("#{round}")).collect()
    }

    // every branch of a repeat fork, walked once for each round
    fn repeat(&mut self, id: ForkId) -> Result<Graph, Diag> {
        let fork = &self.model.forks[id.0];
        let mut count = None;
        for prop in &fork.props {
            let binding = &self.model.bindings[prop.0];
            match (binding.path[0].as_str(), self.resolved(&binding.value)) {
                ("count", Graph::Num(bts_lang::Number::Int(rounds))) if (0..=64).contains(rounds) => {
                    count = Some(*rounds as usize);
                }
                ("count", _) => {
                    return Err(fail(
                        binding.range,
                        "a repeat fork in a walk is written with a whole count from 0 to 64",
                    ));
                }
                _ => return Err(fail(binding.range, "a repeat fork in a walk takes a count and nothing else")),
            }
        }
        let count = count.ok_or_else(|| fail(fork.range, "a repeat fork needs a count"))?;
        let mut steps = Vec::new();
        for round in 0..count {
            self.rounds.push(round);
            for edge in fork.branches.iter().flatten() {
                steps.extend(self.step(*edge)?);
            }
            self.rounds.pop();
        }
        Ok(Graph::Array(Array { elem: steps }))
    }

    // the steps of whichever branch the fork takes
    fn fork(&mut self, id: ForkId) -> Result<Graph, Diag> {
        let fork = &self.model.forks[id.0];
        if fork.primitive == Primitive::Repeat {
            return self.repeat(id);
        }
        if let Some(prop) = fork.props.first() {
            return Err(fail(
                self.model.bindings[prop.0].range,
                "a fork in a walk takes no properties",
            ));
        }
        let place = match self.forks.iter().position(|known| *known == id) {
            Some(place) => place,
            None => {
                self.forks.push(id);
                self.forks.len() - 1
            }
        };
        let mut branches = Vec::new();
        for branch in &fork.branches {
            let mut steps = Vec::new();
            for edge in branch {
                steps.extend(self.step(*edge)?);
            }
            let first = *branch.first().ok_or_else(|| fail(fork.range, "a branch needs a step"))?;
            branches.push((first, Graph::Array(Array { elem: steps })));
        }
        let value = |binding: BindingId| self.model.bindings[binding.0].value.clone();
        match fork.primitive {
            Primitive::Weighted => {
                let mut weights = Vec::new();
                for (edge, _) in &branches {
                    if let Some(guard) = self.prop(*edge, "when") {
                        return Err(fail(
                            self.model.bindings[guard.0].range,
                            "a weighted fork in a walk chooses by weight alone, guards are not supported yet",
                        ));
                    }
                    let weight = self
                        .prop(*edge, "weight")
                        .ok_or_else(|| fail(self.model.edges[edge.0].range, "a weighted branch needs a weight"))?;
                    weights.push(ArrayElem::Item(value(weight)));
                }
                let options = branches.into_iter().map(|(_, steps)| ArrayElem::Item(steps)).collect();
                let mut args = vec![
                    Graph::Str(format!("{}.{}{}", self.name, place + 1, self.round())),
                    Graph::Array(Array { elem: weights }),
                    Graph::Array(Array { elem: options }),
                ];
                args.extend(self.per.clone());
                Ok(Graph::Func {
                    func: Func::Call {
                        function: PICK.to_owned(),
                        args,
                        returns: FieldType::Array,
                        shape: None,
                    },
                    range: fork.range,
                })
            }
            Primitive::Exclusive => {
                // the language proves the guards disjoint, so their order decides nothing
                let mut taken = Graph::Array(Array { elem: vec![] });
                for (edge, steps) in branches.into_iter().rev() {
                    let guard = self.prop(edge, "when").ok_or_else(|| {
                        fail(
                            self.model.edges[edge.0].range,
                            "a branch of an exclusive fork in a walk needs a `when` guard",
                        )
                    })?;
                    taken = Graph::Cond {
                        cond: Box::new(value(guard)),
                        then: Box::new(steps),
                        otherwise: Box::new(taken),
                    };
                }
                Ok(taken)
            }
            _ => Err(fail(
                fork.range,
                format!("a walk takes one branch at a fork, so `{}` is not supported in it", fork.kind),
            )),
        }
    }
}

// the relation nodes that join walks, each with the walks it joins
// the role an incidence edge gives its participant: member, input or output
fn role(model: &Model, edge: EdgeId) -> &str {
    (model.edges[edge.0].props.iter())
        .map(|id| &model.bindings[id.0])
        .find(|binding| binding.path == ["role"])
        .and_then(|binding| match &binding.value {
            Graph::Str(role) => Some(role.as_str()),
            _ => None,
        })
        .unwrap_or("member")
}

fn relations(model: &Model) -> Vec<(NodeId, Vec<(EdgeId, NodeId)>)> {
    let mut found = Vec::new();
    for (index, node) in model.nodes.iter().enumerate() {
        if node.primitive != Primitive::Relation {
            continue;
        }
        let relation = NodeId(index);
        let joined: Vec<_> = (model.edges.iter().enumerate())
            .filter(|(_, edge)| edge.primitive == Primitive::Incidence)
            .filter_map(|(edge, joins)| match (joins.source == relation, joins.target == relation) {
                (true, false) => Some((EdgeId(edge), joins.target)),
                (false, true) => Some((EdgeId(edge), joins.source)),
                _ => None,
            })
            .collect();
        if joined
            .iter()
            .any(|(_, other)| model.derives(&model.nodes[other.0].kind, WALK))
        {
            found.push((relation, joined));
        }
    }
    found
}

// what a walk is related to: for each relation it is part of, what the other walks in it give
fn related(model: &Model, walk: NodeId) -> Result<Graph, Diag> {
    let mut elem = Vec::new();
    for (relation, joined) in relations(model) {
        if !joined.iter().any(|(_, other)| *other == walk) {
            continue;
        }
        let node = &model.nodes[relation.0];
        let mut others = Vec::new();
        // in a relation with a direction an input reads the outputs and an output reads the inputs
        let own: Vec<_> = (joined.iter())
            .filter(|(_, other)| *other == walk)
            .map(|(edge, _)| role(model, *edge))
            .collect();
        let read = |edge: EdgeId| match role(model, edge) {
            "input" => own.contains(&"output"),
            "output" => own.contains(&"input"),
            _ => own.contains(&"member"),
        };
        for (edge, other) in joined.iter().filter(|(edge, other)| *other != walk && read(*edge)) {
            let gives = (model.bindings.iter())
                .position(|binding| binding.owner == ElementId::Node(*other) && !binding.property && binding.path == [GIVES])
                .filter(|_| model.derives(&model.nodes[other.0].kind, WALK))
                .ok_or_else(|| {
                    fail(
                        model.edges[edge.0].range,
                        "a relation in a world joins walks, and this is not one",
                    )
                })?;
            others.push(ArrayElem::Item(Graph::BindingRef(BindingId(gives), node.range)));
        }
        let key = node.name.clone().unwrap_or_else(|| node.kind.clone());
        if elem.iter().any(|field: &bts_lang::ObjectField| field.key == key) {
            return Err(fail(
                node.range,
                format!("two relations of one walk are both known as `{key}`, name them apart"),
            ));
        }
        elem.push(bts_lang::ObjectField {
            key,
            value: Graph::Array(Array { elem: others }),
        });
    }
    Ok(Graph::Object(Object { elem }))
}

/// Turns every walk in a model into the value of its `steps`.
///
/// A step's own fields do not depend on the walk, so they are read by name whether or
/// not a walk visits the step. What was built is read from the walk.
pub fn supply(model: &Model) -> Result<Supplied, Diag> {
    let mut supplied = Supplied {
        elements: elements(model),
        ..Supplied::default()
    };
    if let Some(junction) = (supplied.elements.iter()).find_map(|element| match element {
        ElementId::Junction(id) => Some(&model.junctions[id.0]),
        _ => None,
    }) {
        return Err(fail(junction.range, "a walk takes one path, so nothing in it is joined"));
    }
    // a series has no steps, and its identifier is drawn for its number
    for series in each_series(model) {
        let node = &model.nodes[series.0];
        if let Some(edge) = model.edges.iter().find(|edge| edge.source == series) {
            return Err(fail(edge.range, "a series has no steps, so nothing leaves it"));
        }
        if let Some(fork) = model.forks.iter().find(|fork| fork.source == series) {
            return Err(fail(fork.range, "a series has no steps, so nothing leaves it"));
        }
        let number = (model.bindings.iter())
            .position(|binding| binding.owner == ElementId::Node(series) && !binding.property && binding.path == ["number"])
            .ok_or_else(|| fail(node.range, "a series needs a `number` field"))?;
        let name = node.name.clone().unwrap_or_else(|| format!("series{}", series.0));
        // where a trace sits is worked out from its index, the fewest turns and the most
        let prop = |key: &str| {
            (node.props.iter())
                .map(|id| &model.bindings[id.0])
                .find(|binding| binding.path == [key])
                .map(|binding| binding.value.clone())
        };
        supplied.context.push(Supply {
            node: series,
            key: PLACE.to_owned(),
            value: Graph::Func {
                func: Func::Call {
                    function: TURN.to_owned(),
                    args: vec![
                        Graph::Str(name.clone()),
                        Graph::Context {
                            owner: ElementId::Node(series),
                            path: vec!["trace_index".to_owned()],
                        },
                        prop("turns").unwrap_or(Graph::Num(bts_lang::Number::Int(1))),
                        prop("up_to").unwrap_or(Graph::Null),
                        prop("weights").unwrap_or(Graph::Null),
                    ],
                    returns: FieldType::Object,
                    shape: Some(PLACE_SHAPE),
                },
                range: node.range,
            },
        });
        supplied.context.push(Supply {
            node: series,
            key: IDENTITY.to_owned(),
            value: Graph::Func {
                func: Func::Call {
                    function: ID.to_owned(),
                    args: vec![Graph::Str(name), Graph::BindingRef(BindingId(number), node.range)],
                    returns: FieldType::String,
                    shape: None,
                },
                range: node.range,
            },
        });
    }
    let mut within: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for root in roots(model) {
        let node = &model.nodes[root.0];
        let name = node.name.clone().unwrap_or_else(|| format!("walk{}", root.0));
        let per = (node.props.iter())
            .map(|id| &model.bindings[id.0])
            .find(|binding| binding.path == [PER])
            .map(|binding| binding.value.clone())
            .filter(|value| !matches!(value, Graph::Null));
        let mut walker = Walker {
            model,
            root,
            name: name.clone(),
            per,
            path: Vec::new(),
            forks: Vec::new(),
            rounds: Vec::new(),
            inner: Vec::new(),
        };
        let steps = walker.leads(root)?;
        supplied.context.push(Supply {
            node: root,
            key: STEPS.to_owned(),
            value: Graph::Array(Array { elem: steps }),
        });
        supplied.context.push(Supply {
            node: root,
            key: RELATED.to_owned(),
            value: related(model, root)?,
        });
        // the numbers a walk's kind may sample by, addressed like a fork's draws
        let mut address = vec![Graph::Str(name.clone())];
        address.extend(walker.per.clone());
        supplied.context.push(Supply {
            node: root,
            key: DRAWS_KEY.to_owned(),
            value: Graph::Func {
                func: Func::Call {
                    function: DRAWS.to_owned(),
                    args: address,
                    returns: FieldType::Array,
                    shape: None,
                },
                range: node.range,
            },
        });
        // a key the walk's kind requires and nothing leads to it with is null, so a kind can ask whether it is there
        let required = (model.kind_names.get(&node.kind).into_iter())
            .flat_map(|kind| &model.kinds[kind.0].requires)
            .map(|key| key.name.as_str());
        for key in required {
            let known = [STEPS, RELATED, DRAWS_KEY, "trace_index", "history"].contains(&key)
                || supplied.context.iter().any(|supply| supply.node == root && supply.key == key);
            let led = (model.context.get(root.0).into_iter().flatten())
                .filter(|supply| supply.key == key)
                .flat_map(|supply| &supply.suppliers)
                .any(|supplier| (model.context_values.iter()).any(|value| value.key == key && value.owner == *supplier));
            if !known && !led {
                supplied.context.push(Supply {
                    node: root,
                    key: key.to_owned(),
                    value: Graph::Null,
                });
            }
        }
        within.insert(root, walker.inner);
    }
    // a walk is built before the walk it is a step of, so none is a step of itself
    for root in roots(model) {
        let mut pending = within[&root].clone();
        let mut seen = BTreeSet::new();
        while let Some(inner) = pending.pop() {
            if inner == root {
                return Err(fail(
                    model.nodes[root.0].range,
                    "this walk is a step of a walk that is a step of it, and a walk cannot build itself",
                ));
            }
            if seen.insert(inner) {
                pending.extend(within.get(&inner).into_iter().flatten().copied());
            }
        }
    }
    // a network gathers walks under one name, and reads what each of them gives
    for network in &model.networks {
        let mut elem = Vec::new();
        for member in &network.nodes {
            let node = &model.nodes[member.0];
            let gives = (model.bindings.iter())
                .position(|binding| binding.owner == ElementId::Node(*member) && !binding.property && binding.path == [GIVES])
                .filter(|_| model.derives(&node.kind, WALK));
            let (Some(gives), Some(name)) = (gives, &node.name) else {
                return Err(fail(
                    node.range,
                    "a network in a world gathers named walks, and this is not one",
                ));
            };
            elem.push(bts_lang::ObjectField {
                key: name.clone(),
                value: Graph::BindingRef(BindingId(gives), model.nodes[network.node.0].range),
            });
        }
        supplied.context.push(Supply {
            node: network.node,
            key: MEMBERS.to_owned(),
            value: Graph::Object(Object { elem }),
        });
    }
    Ok(supplied)
}

/// A number from zero up to one for an address, the same in Rust, Python and TypeScript.
///
/// FNV-1a over the bytes of the address, then the 32-bit finalizer of MurmurHash3.
pub fn draw(address: &str) -> f64 {
    f64::from(hash(address)) / 4_294_967_296.0
}

/// The 32 bits an address hashes to, the same in Rust, Python and TypeScript.
pub fn hash(address: &str) -> u32 {
    let mut value: u32 = 2_166_136_261;
    for byte in address.bytes() {
        value = (value ^ u32::from(byte)).wrapping_mul(16_777_619);
    }
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value
}

/// The address of a fork's draw: the seed, what the walk is drawn for, and the fork.
///
/// A walk that does not say is drawn for the trace, by its index.
pub fn address(seed: u64, per: &str, label: &str) -> String {
    format!("{seed}/{per}/{label}")
}

// what a walk is drawn for, as the text every consumer writes the same way
fn drawn_for(per: &Value) -> Result<String, EvaluateError> {
    match per {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => match (number.as_i64(), number.as_u64(), number.as_f64()) {
            (Some(whole), _, _) => Ok(whole.to_string()),
            (_, Some(whole), _) => Ok(whole.to_string()),
            (_, _, Some(float)) if float.fract() == 0.0 && float.abs() <= 9_007_199_254_740_991.0 => {
                Ok((float as i64).to_string())
            }
            _ => Err(invalid(PER_SHAPE)),
        },
        _ => Err(invalid(PER_SHAPE)),
    }
}

const PER_SHAPE: &str = "a walk is drawn per a string or a whole number";

/// The branch a draw takes among weights, or none when no weight is positive.
pub fn take(weights: &[f64], draw: f64) -> Option<usize> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let target = draw * total;
    let mut reached = 0.0;
    let mut last = None;
    for (index, weight) in weights.iter().enumerate() {
        if *weight <= 0.0 {
            continue;
        }
        reached += weight;
        last = Some(index);
        if target < reached {
            return last;
        }
    }
    last
}

fn invalid(what: &'static str) -> EvaluateError {
    EvaluateError::Invalid(what.into())
}

/// Evaluates the pick a weighted fork lowers to: the steps of the branch its draw takes.
pub(super) fn pick(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let (Value::String(label), Value::Array(weights), Value::Array(options), per) = (match args {
        [label, weights, options] => (label, weights, options, None),
        [label, weights, options, per] => (label, weights, options, Some(per)),
        _ => return Err(EvaluateError::Shape),
    }) else {
        return Err(EvaluateError::Shape);
    };
    let weights = (weights.iter())
        .map(|weight| match weight {
            Value::Number(number) => (number.as_f64())
                .filter(|weight| weight.is_finite() && *weight >= 0.0)
                .ok_or_else(|| invalid("a branch weight is a finite number that is not negative")),
            _ => Err(invalid("a branch weight is a number")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (seed, trace_index) = evaluation.address();
    let per = match per {
        Some(per) => drawn_for(per)?,
        None => trace_index.to_string(),
    };
    let taken = take(&weights, draw(&address(seed, &per, label)));
    Ok(match taken.and_then(|index| options.get(index)) {
        Some(steps) => steps.clone(),
        None => Value::Array(vec![]),
    })
}

/// Evaluates a series' identifier: sixteen hex digits drawn for its number.
pub(super) fn id(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::String(label), per] = args else {
        return Err(EvaluateError::Shape);
    };
    let (seed, _) = evaluation.address();
    let per = drawn_for(per)?;
    let half = |part: &str| hash(&address(seed, &per, &format!("{label}#{part}")));
    Ok(Value::String(format!("{:08x}{:08x}", half("a"), half("b"))))
}

/// Where a trace sits among series: the series' number, the turn within it and its length.
///
/// With one length every series has `turns` traces. With `up_to`, each draws a
/// length from `turns` to `up_to`, and a trace counts from the start of its block of
/// [`BLOCK`] traces. The last series of a block ends with the block, and its
/// length is the turns it has. The lengths are equally likely unless each is given a
/// weight, the first for `turns` and the last for `up_to`, at least one of them positive.
pub fn place(seed: u64, label: &str, index: u64, turns: u64, up_to: Option<u64>, weights: Option<&[f64]>) -> (u64, u64, u64) {
    let Some(up_to) = up_to else {
        return (index / turns, index % turns, turns);
    };
    let (block, offset) = (index / BLOCK, index % BLOCK);
    let (mut start, mut count) = (0, 0);
    loop {
        let drawn = draw(&address(seed, &block.to_string(), &format!("{label}#{count}")));
        let length = turns
            + match weights {
                Some(weights) => take(weights, drawn).unwrap_or(0) as u64,
                None => (drawn * (up_to - turns + 1) as f64) as u64,
            };
        if start + length > offset {
            return (block * BLOCK + count, offset - start, length.min(BLOCK - start));
        }
        start += length;
        count += 1;
    }
}

// a count of turns as written: a whole number of at least one
fn turns_of(value: &Value) -> Result<u64, EvaluateError> {
    match value {
        Value::Number(number) => (number.as_u64())
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|float| float.fract() == 0.0 && *float >= 1.0)
                    .map(|float| float as u64)
            })
            .filter(|turns| (1..=BLOCK).contains(turns))
            .ok_or_else(|| invalid(TURNS_SHAPE)),
        _ => Err(invalid(TURNS_SHAPE)),
    }
}

const TURNS_SHAPE: &str = "a series has a whole number of turns from 1 to 256";
const WEIGHTS_SHAPE: &str = "a series takes one weight for each length from turns to up_to, none negative and one positive";

/// Evaluates a series' place for one trace.
pub(super) fn turn(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::String(label), Value::Number(index), turns, up_to, weights] = args else {
        return Err(EvaluateError::Shape);
    };
    let index = index.as_u64().ok_or(EvaluateError::Shape)?;
    let turns = turns_of(turns)?;
    let up_to = match up_to {
        Value::Null => None,
        up_to => Some(turns_of(up_to)?),
    };
    if up_to.is_some_and(|up_to| up_to < turns) {
        return Err(invalid("a series runs up to at least as many turns as it starts with"));
    }
    let weights = match (weights, up_to) {
        (Value::Null, _) => None,
        (Value::Array(weights), Some(up_to)) => {
            let weights = (weights.iter())
                .map(|weight| match weight {
                    Value::Number(number) => number.as_f64().filter(|weight| weight.is_finite() && *weight >= 0.0),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .filter(|weights| weights.len() as u64 == up_to - turns + 1 && weights.iter().any(|weight| *weight > 0.0))
                .ok_or_else(|| invalid(WEIGHTS_SHAPE))?;
            Some(weights)
        }
        _ => return Err(invalid(WEIGHTS_SHAPE)),
    };
    let (seed, _) = evaluation.address();
    let (number, turn, length) = place(seed, label, index, turns, up_to, weights.as_deref());
    let whole = |value: u64| Value::Number(value.into());
    Ok(Value::Object(BTreeMap::from([
        ("number".to_owned(), whole(number)),
        ("turn".to_owned(), whole(turn)),
        ("length".to_owned(), whole(length)),
    ])))
}

/// How many draws a walk is given. Styling a sentence takes far fewer.
pub const DRAWN: usize = 32;

/// The numbers a walk's kind may sample by: [`DRAWN`] draws of 53 bits each, addressed like a fork's.
pub fn draws(seed: u64, per: &str, label: &str) -> Vec<f64> {
    (0..DRAWN)
        .map(|at| {
            let half = |part: &str| u64::from(hash(&address(seed, per, &format!("{label}~{at}{part}"))));
            ((half("a") << 21) | (half("b") >> 11)) as f64 / 9_007_199_254_740_992.0
        })
        .collect()
}

/// Evaluates the draws a walk is given.
pub(super) fn drawn(args: &[Value], evaluation: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let (seed, trace_index) = evaluation.address();
    let (label, per) = match args {
        [Value::String(label)] => (label, trace_index.to_string()),
        [Value::String(label), per] => (label, drawn_for(per)?),
        _ => return Err(EvaluateError::Shape),
    };
    let numbers = draws(seed, &per, label).into_iter().map(|draw| {
        serde_json::Number::from_f64(draw)
            .map(Value::Number)
            .ok_or(EvaluateError::Shape)
    });
    Ok(Value::Array(numbers.collect::<Result<_, _>>()?))
}

/// Evaluates `merge`: one object from many, where later keys win and null changes nothing.
pub(super) fn merge(args: &[Value], _: &mut Evaluation<'_>) -> Result<Value, EvaluateError> {
    let [Value::Object(base), Value::Array(objects)] = args else {
        return Err(EvaluateError::Shape);
    };
    let mut merged = base.clone();
    for object in objects {
        match object {
            Value::Null => {}
            Value::Object(object) => {
                for (key, value) in object {
                    if !matches!(value, Value::Null) {
                        merged.insert(key.clone(), value.clone());
                    }
                }
            }
            _ => return Err(invalid("merge: every item is an object or null")),
        }
    }
    Ok(Value::Object(merged))
}

/// The text the helpers that need the seed share, for one language.
pub(super) fn runtime(lang: crate::mocking::ScorerLang, _agent: bool) -> String {
    match lang {
        crate::mocking::ScorerLang::Python => PY_RUNTIME,
        crate::mocking::ScorerLang::Typescript => TS_RUNTIME,
    }
    .to_owned()
}

// `seeded` gives a run its seed
const PY_RUNTIME: &str = r#"
def seeded(random_source, seed=None, trace=0):
    if seed is None:
        seed = int(_draw(random_source) * 4294967296)
    def source():
        return random_source()
    source.seed, source.trace = seed, trace
    return source


def trace_of(random_source):
    return random_source.trace


def _world_hash(address):
    value = 2166136261
    for byte in address.encode("utf-8"):
        value = ((value ^ byte) * 16777619) & 0xFFFFFFFF
    value ^= value >> 16
    value = (value * 0x85EBCA6B) & 0xFFFFFFFF
    value ^= value >> 13
    value = (value * 0xC2B2AE35) & 0xFFFFFFFF
    value ^= value >> 16
    return value


def _world_draw(address):
    return _world_hash(address) / 4294967296


def _world_take(weights, draw):
    total = sum(weights)
    if total <= 0: return None
    target, reached, last = draw * total, 0.0, None
    for index, weight in enumerate(weights):
        if weight <= 0: continue
        reached += weight
        last = index
        if target < reached: return last
    return last


def _world_per(per):
    per = _value(per)
    if isinstance(per, str): return per
    if type(per) is int or (type(per) is float and per.is_integer() and abs(per) <= 9007199254740991): return str(int(per))
    raise TypeError("a walk is drawn per a string or a whole number")


def _world_pick(label, weights, options, *rest):
    *per, random_source = rest
    weights = [float(_num(weight)) for weight in _value(weights)]
    if any(weight < 0 for weight in weights): raise ValueError("a branch weight is a finite number that is not negative")
    drawn_for = _world_per(per[0]) if per else str(random_source.trace)
    taken = _world_take(weights, _world_draw(f"{random_source.seed}/{drawn_for}/{label}"))
    return [] if taken is None else _value(options)[taken]


def _world_turns(value):
    value = _value(value)
    if type(value) is float and value.is_integer(): value = int(value)
    if type(value) is not int or not 1 <= value <= 256: raise ValueError("a series has a whole number of turns from 1 to 256")
    return value


def _world_turn(label, index, turns, up_to, weights, random_source):
    index, turns, weights = int(_value(index)), _world_turns(turns), _value(weights)
    shape = "a series takes one weight for each length from turns to up_to, none negative and one positive"
    if _value(up_to) is None:
        if weights is not None: raise ValueError(shape)
        return {"number": index // turns, "turn": index % turns, "length": turns}
    up_to = _world_turns(up_to)
    if up_to < turns: raise ValueError("a series runs up to at least as many turns as it starts with")
    if weights is not None:
        if not isinstance(weights, list) or len(weights) != up_to - turns + 1: raise ValueError(shape)
        if any(type(weight) not in (int, float) or not _math.isfinite(weight) or weight < 0 for weight in weights) or not any(weights): raise ValueError(shape)
        weights = [float(weight) for weight in weights]
    block, offset = divmod(index, 256)
    start, count = 0, 0
    while True:
        drawn = _world_draw(f"{random_source.seed}/{block}/{label}#{count}")
        length = turns + (int(drawn * (up_to - turns + 1)) if weights is None else _world_take(weights, drawn))
        if start + length > offset: return {"number": block * 256 + count, "turn": offset - start, "length": min(length, 256 - start)}
        start += length
        count += 1


def _world_draws(label, *rest):
    *per, random_source = rest
    address = f"{random_source.seed}/{_world_per(per[0]) if per else random_source.trace}/{label}~"
    return [(_world_hash(f"{address}{at}a") * 2097152 + (_world_hash(f"{address}{at}b") >> 11)) / 9007199254740992 for at in range(32)]


def _world_id(label, per, random_source):
    address = f"{random_source.seed}/{_world_per(per)}/{label}#"
    return f"{_world_hash(address + 'a'):08x}{_world_hash(address + 'b'):08x}"
"#;

const TS_RUNTIME: &str = r#"
function seeded(randomSource: () => number, seed?: number | bigint, trace: number = 0): () => number {
    return Object.assign(() => randomSource(), { seed: seed ?? Math.floor(_draw(randomSource) * 4294967296), trace });
}
function traceOf(randomSource: () => number): number {
    return (randomSource as { trace?: number }).trace ?? 0;
}
function _world_hash(address: string): number {
    let value = 2166136261;
    for (const byte of new TextEncoder().encode(address)) value = Math.imul(value ^ byte, 16777619) >>> 0;
    value ^= value >>> 16;
    value = Math.imul(value, 0x85ebca6b) >>> 0;
    value ^= value >>> 13;
    value = Math.imul(value, 0xc2b2ae35) >>> 0;
    value ^= value >>> 16;
    return value >>> 0;
}
function _world_draw(address: string): number {
    return _world_hash(address) / 4294967296;
}
function _world_take(weights: number[], draw: number): number | null {
    const total = weights.reduce((sum, weight) => sum + weight, 0);
    if (total <= 0) return null;
    const target = draw * total;
    let reached = 0;
    let last: number | null = null;
    for (let index = 0; index < weights.length; index += 1) {
        if (weights[index] <= 0) continue;
        reached += weights[index];
        last = index;
        if (target < reached) return last;
    }
    return last;
}
function _world_per(per: unknown): string {
    per = _serialize(per);
    if (typeof per === "string") return per;
    if (typeof per === "number" && Number.isSafeInteger(per)) return String(per);
    throw new TypeError("a walk is drawn per a string or a whole number");
}
function _world_pick(label: string, weights: unknown, options: unknown, ...rest: unknown[]): unknown[] {
    const randomSource = rest[rest.length - 1] as () => number;
    const drawnFor = rest.length > 1 ? _world_per(rest[0]) : String(traceOf(randomSource));
    const numbers: number[] = (_serialize(weights) as unknown[]).map(weight => {
        if (typeof weight !== "number" || !Number.isFinite(weight) || weight < 0) throw new RangeError("a branch weight is a finite number that is not negative");
        return weight;
    });
    const seed = (randomSource as { seed?: number | bigint }).seed;
    const taken = _world_take(numbers, _world_draw(`${seed}/${drawnFor}/${label}`));
    return taken === null ? [] : (_value(options) as unknown[][])[taken];
}
function _world_turns(value: unknown): number {
    value = _serialize(value);
    if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > 256) throw new RangeError("a series has a whole number of turns from 1 to 256");
    return value;
}
function _world_turn(label: string, index: unknown, turnsValue: unknown, upToValue: unknown, weightsValue: unknown, randomSource: () => number): { number: number; turn: number; length: number } {
    const at = Number(_serialize(index));
    const turns = _world_turns(turnsValue);
    const weights = _serialize(weightsValue) ?? null;
    const shape = "a series takes one weight for each length from turns to up_to, none negative and one positive";
    if (_serialize(upToValue) === null || _serialize(upToValue) === undefined) {
        if (weights !== null) throw new RangeError(shape);
        return { number: Math.floor(at / turns), turn: at % turns, length: turns };
    }
    const upTo = _world_turns(upToValue);
    if (upTo < turns) throw new RangeError("a series runs up to at least as many turns as it starts with");
    if (weights !== null) {
        if (!Array.isArray(weights) || weights.length !== upTo - turns + 1) throw new RangeError(shape);
        if (weights.some(weight => typeof weight !== "number" || !Number.isFinite(weight) || weight < 0) || !weights.some(weight => weight > 0)) throw new RangeError(shape);
    }
    const seed = (randomSource as { seed?: number | bigint }).seed;
    const block = Math.floor(at / 256);
    const offset = at % 256;
    let start = 0;
    for (let count = 0; ; count += 1) {
        const drawn = _world_draw(`${seed}/${block}/${label}#${count}`);
        const length = turns + (weights === null ? Math.floor(drawn * (upTo - turns + 1)) : (_world_take(weights, drawn) as number));
        if (start + length > offset) return { number: block * 256 + count, turn: offset - start, length: Math.min(length, 256 - start) };
        start += length;
    }
}
function _world_draws(label: string, ...rest: unknown[]): number[] {
    const randomSource = rest[rest.length - 1] as () => number;
    const seed = (randomSource as { seed?: number | bigint }).seed;
    const address = `${seed}/${rest.length > 1 ? _world_per(rest[0]) : traceOf(randomSource)}/${label}~`;
    return Array.from({ length: 32 }, (_, at) => (_world_hash(`${address}${at}a`) * 2097152 + (_world_hash(`${address}${at}b`) >>> 11)) / 9007199254740992);
}
function _world_id(label: string, per: unknown, randomSource: () => number): string {
    const seed = (randomSource as { seed?: number | bigint }).seed;
    const address = `${seed}/${_world_per(per)}/${label}#`;
    const half = (part: string) => _world_hash(address + part).toString(16).padStart(8, "0");
    return half("a") + half("b");
}
"#;

/// The Python text of `merge`.
pub(super) const PY_MERGE: &str = r#"
def _world_merge(base, objects):
    merged = dict(_value(base))
    for item in _value(objects):
        if item is None: continue
        if not isinstance(item, dict): raise TypeError("merge: every item is an object or null")
        merged.update({key: value for key, value in item.items() if value is not None})
    return merged
"#;

/// The TypeScript text of `merge`.
pub(super) const TS_MERGE: &str = r#"
function _world_merge(base: unknown, objects: unknown): Record<string, any> {
    const merged: Record<string, any> = { ..._value(base) };
    for (const item of _value(objects) as unknown[]) {
        if (item === null || item === undefined) continue;
        if (typeof item !== "object" || Array.isArray(item)) throw new TypeError("merge: every item is an object or null");
        for (const [key, value] of Object.entries(item)) if (value !== null && value !== undefined) merged[key] = value;
    }
    return merged;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // the agent tests check that Python and TypeScript take the same branches from the same seeds
    #[test]
    fn a_draw_is_a_fixed_function_of_its_address() {
        assert_eq!(draw(""), 0.6689221884589642);
        assert_eq!(draw("42/0/ask.1"), 0.10680331196635962);
        assert_eq!(draw("42/0/ask.2"), 0.8749931799247861);
        // an address is hashed as its bytes
        assert_eq!(draw("7/3/名前.1"), 0.9960092506371439);
        assert_eq!(draw("42/0/ask.1"), draw(&address(42, "0", "ask.1")));
        assert_ne!(draw("42/0/ask.1"), draw("42/1/ask.1"));
        assert_eq!(hash("42/0/chat#a"), 0x7afdce9);
    }

    #[test]
    fn a_trace_finds_its_place_among_series() {
        // with one length the place is arithmetic
        assert_eq!(place(7, "chat", 7, 3, None, None), (2, 1, 3));
        // with a range every trace of a block belongs to one series, in order, with no gaps
        let mut lengths = BTreeSet::new();
        for block in 0..4 {
            let (mut expected, mut left) = (None, 0);
            for offset in 0..BLOCK {
                let (number, turn, length) = place(7, "chat", block * BLOCK + offset, 2, Some(6), None);
                if left == 0 {
                    // a series opens where the one before it closed
                    assert_eq!(turn, 0);
                    assert_eq!(number, expected.map_or(block * BLOCK, |last: u64| last + 1));
                    assert!((1..=6).contains(&length));
                    // only the series the block cuts short has fewer turns than the fewest
                    assert!(length >= 2 || offset + length == BLOCK);
                    lengths.insert(length);
                    (expected, left) = (Some(number), length);
                }
                assert_eq!(Some(number), expected);
                assert_eq!(turn, length - left);
                left -= 1;
            }
            assert_eq!(left, 0);
        }
        // every length in the range turns up
        assert!((2..=6).all(|length| lengths.contains(&length)), "{lengths:?}");
        assert_eq!(
            place(7, "chat", 300, 2, Some(6), None),
            place(7, "chat", 300, 2, Some(6), None)
        );
        // equal weights are the same draw as no weights, and a length with no weight never turns up
        for index in 0..BLOCK {
            assert_eq!(
                place(7, "chat", index, 2, Some(6), None),
                place(7, "chat", index, 2, Some(6), Some(&[1.0; 5]))
            );
            let (_, turn, length) = place(7, "chat", index, 2, Some(4), Some(&[5.0, 0.0, 1.0]));
            assert!(
                length == 2 || length == 4 || (length == 1 && index == BLOCK - 1),
                "{index} {turn} {length}"
            );
        }
        // most series are short when the short lengths weigh the most
        let opens = |weights: Option<&[f64]>, wanted: u64| {
            (0..4 * BLOCK)
                .map(|index| place(7, "chat", index, 1, Some(4), weights))
                .filter(|(_, turn, length)| *turn == 0 && *length == wanted)
                .count()
        };
        assert!(opens(Some(&[8.0, 4.0, 2.0, 1.0]), 1) > 3 * opens(Some(&[8.0, 4.0, 2.0, 1.0]), 4));
        assert_ne!(
            (0..64)
                .map(|index| place(7, "chat", index, 2, Some(6), None))
                .collect::<Vec<_>>(),
            (0..64)
                .map(|index| place(8, "chat", index, 2, Some(6), None))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_draw_takes_a_branch_by_weight() {
        assert_eq!(take(&[2.0, 1.0, 1.0], 0.0), Some(0));
        assert_eq!(take(&[2.0, 1.0, 1.0], 0.49), Some(0));
        assert_eq!(take(&[2.0, 1.0, 1.0], 0.5), Some(1));
        assert_eq!(take(&[2.0, 1.0, 1.0], 0.99), Some(2));
        // a branch with no weight is never taken, and no weight at all takes nothing
        assert_eq!(take(&[0.0, 1.0], 0.0), Some(1));
        assert_eq!(take(&[0.0, 0.0], 0.3), None);
        assert_eq!(take(&[], 0.3), None);
    }
}
