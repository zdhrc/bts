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

use crate::mocking::data::Value;
use crate::mocking::{EvaluateError, Evaluation};
use crate::providers::{Supplied, Supply};
use bts_lang::{
    Array, ArrayElem, BindingId, Diag, DiagPhase, Direction, EdgeId, ElementId, FieldType, ForkId, Func, Model, NodeId, Object,
    Primitive, SrcRange, Value as Graph,
};
use std::collections::{BTreeMap, BTreeSet};

/// The kind every walk derives from.
pub const WALK: &str = "walk";
/// The context key a walk reads its steps through.
pub const STEPS: &str = "steps";
/// The field a step hands to its walk.
pub const GIVES: &str = "gives";
/// The internal function a weighted fork lowers to. No author can write the name.
pub const PICK: &str = "world.pick";

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

/// The elements the world provider walks: every walk, and the steps, edges and forks it leads to.
///
/// A junction that joins any of them is included, so it is reported as unsupported.
pub fn elements(model: &Model) -> Vec<ElementId> {
    let mut nodes = BTreeSet::new();
    let mut pending: Vec<NodeId> = roots(model).collect();
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
    for (index, junction) in model.junctions.iter().enumerate() {
        let joins = nodes.contains(&junction.target)
            || (junction.inputs.iter()).any(|input| nodes.contains(&model.edges[input.0].source));
        if joins {
            elements.insert(ElementId::Junction(bts_lang::JunctionId(index)));
        }
    }
    elements.into_iter().collect()
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
    name: String,
    // the steps being visited, to find a path that returns to itself
    path: Vec<NodeId>,
    // each fork's place in the walk, so a fork reached along two paths draws once
    forks: Vec<ForkId>,
    // how many forks enclose the step being visited
    depth: usize,
    visited: BTreeSet<NodeId>,
    always: BTreeSet<NodeId>,
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
        if self.model.derives(&node.kind, WALK) {
            return Err(fail(
                node.range,
                "a walk cannot be a step of another walk yet, give it steps of its own beside this one",
            ));
        }
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
        if edge.cycle || self.path.contains(&edge.target) {
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
        let mut steps = vec![ArrayElem::Item(self.gives(target)?)];
        self.visited.insert(target);
        if self.depth == 0 {
            self.always.insert(target);
        }
        steps.extend(self.leads(target)?);
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

    // the steps of whichever branch the fork takes
    fn fork(&mut self, id: ForkId) -> Result<Graph, Diag> {
        let fork = &self.model.forks[id.0];
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
        self.depth += 1;
        let mut branches = Vec::new();
        for branch in &fork.branches {
            let mut steps = Vec::new();
            for edge in branch {
                steps.extend(self.step(*edge)?);
            }
            let first = *branch.first().ok_or_else(|| fail(fork.range, "a branch needs a step"))?;
            branches.push((first, Graph::Array(Array { elem: steps })));
        }
        self.depth -= 1;
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
                Ok(Graph::Func {
                    func: Func::Call {
                        function: PICK.to_owned(),
                        args: vec![
                            Graph::Str(format!("{}.{}", self.name, place + 1)),
                            Graph::Array(Array { elem: weights }),
                            Graph::Array(Array { elem: options }),
                        ],
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

/// Turns every walk in a model into the value of its `steps`.
///
/// A step that some walk always visits can be read by name from outside. A step
/// that sits behind a fork in every walk that reaches it is hidden, with the reason.
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
    let mut always = BTreeSet::new();
    let mut behind: BTreeMap<NodeId, String> = BTreeMap::new();
    for root in roots(model) {
        let node = &model.nodes[root.0];
        let name = node.name.clone().unwrap_or_else(|| format!("walk{}", root.0));
        let mut walker = Walker {
            model,
            name: name.clone(),
            path: Vec::new(),
            forks: Vec::new(),
            depth: 0,
            visited: BTreeSet::new(),
            always: BTreeSet::new(),
        };
        let steps = walker.leads(root)?;
        supplied.context.push(Supply {
            node: root,
            key: STEPS,
            value: Graph::Array(Array { elem: steps }),
        });
        for step in &walker.visited {
            if !walker.always.contains(step) {
                behind.entry(*step).or_insert_with(|| name.clone());
            }
        }
        always.extend(walker.always);
    }
    for (step, walk) in behind {
        if always.contains(&step) {
            continue;
        }
        let named = match &model.nodes[step.0].name {
            Some(name) => format!("`{name}`"),
            None => "this step".to_owned(),
        };
        supplied.hidden.push((
            step,
            format!("{named} is behind a fork, so a walk may not visit it. Read `{walk}`, the walk it belongs to"),
        ));
    }
    Ok(supplied)
}

/// A number from zero up to one for an address, the same in Rust, Python and TypeScript.
///
/// FNV-1a over the bytes of the address, then the 32-bit finalizer of MurmurHash3.
pub fn draw(address: &str) -> f64 {
    let mut value: u32 = 2_166_136_261;
    for byte in address.bytes() {
        value = (value ^ u32::from(byte)).wrapping_mul(16_777_619);
    }
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    f64::from(value) / 4_294_967_296.0
}

/// The address of a fork's draw in one trace.
pub fn address(seed: u64, trace_index: usize, label: &str) -> String {
    format!("{seed}/{trace_index}/{label}")
}

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
    let [Value::String(label), Value::Array(weights), Value::Array(options)] = args else {
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
    let taken = take(&weights, draw(&address(seed, trace_index, label)));
    Ok(match taken.and_then(|index| options.get(index)) {
        Some(steps) => steps.clone(),
        None => Value::Array(vec![]),
    })
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

/// The Python text of the helpers a walk needs. `seeded` gives a run its seed.
pub(super) const PY_PICK: &str = r#"
def seeded(random_source, seed=None):
    if seed is None:
        seed = int(_draw(random_source) * 4294967296)
    def source():
        return random_source()
    source.seed = seed
    return source


def _world_draw(address):
    value = 2166136261
    for byte in address.encode("utf-8"):
        value = ((value ^ byte) * 16777619) & 0xFFFFFFFF
    value ^= value >> 16
    value = (value * 0x85EBCA6B) & 0xFFFFFFFF
    value ^= value >> 13
    value = (value * 0xC2B2AE35) & 0xFFFFFFFF
    value ^= value >> 16
    return value / 4294967296


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


def _world_pick(label, weights, options, random_source):
    weights = [float(_num(weight)) for weight in _value(weights)]
    if any(weight < 0 for weight in weights): raise ValueError("a branch weight is a finite number that is not negative")
    taken = _world_take(weights, _world_draw(f"{random_source.seed}/0/{label}"))
    return [] if taken is None else _value(options)[taken]
"#;

/// The TypeScript text of the helpers a walk needs.
pub(super) const TS_PICK: &str = r#"
function seeded(randomSource: () => number, seed?: number | bigint): () => number {
    return Object.assign(() => randomSource(), { seed: seed ?? Math.floor(_draw(randomSource) * 4294967296) });
}
function _world_draw(address: string): number {
    let value = 2166136261;
    for (const byte of new TextEncoder().encode(address)) value = Math.imul(value ^ byte, 16777619) >>> 0;
    value ^= value >>> 16;
    value = Math.imul(value, 0x85ebca6b) >>> 0;
    value ^= value >>> 13;
    value = Math.imul(value, 0xc2b2ae35) >>> 0;
    value ^= value >>> 16;
    return (value >>> 0) / 4294967296;
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
function _world_pick(label: string, weights: unknown, options: unknown, randomSource: () => number): unknown[] {
    const numbers: number[] = (_serialize(weights) as unknown[]).map(weight => {
        if (typeof weight !== "number" || !Number.isFinite(weight) || weight < 0) throw new RangeError("a branch weight is a finite number that is not negative");
        return weight;
    });
    const seed = (randomSource as { seed?: number | bigint }).seed;
    const taken = _world_take(numbers, _world_draw(`${seed}/0/${label}`));
    return taken === null ? [] : (_value(options) as unknown[][])[taken];
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
        assert_eq!(draw("42/0/ask.1"), draw(&address(42, 0, "ask.1")));
        assert_ne!(draw("42/0/ask.1"), draw("42/1/ask.1"));
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
