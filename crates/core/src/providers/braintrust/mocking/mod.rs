//! Braintrust implementations of the core mocking capabilities.
//!
//! [`code`] supplies scorer registrations and codebase conventions for
//! [`crate::mocking::code`]. [`data`] supplies formatting and delivery for
//! [`crate::mocking::data`].

pub mod code;
pub mod data;

use crate::mocking as execution;
use bts_lang as graph;
use std::collections::{HashMap, HashSet};

/// Compiles source with the Braintrust definitions for the mocking consumers.
pub fn compile(source: &str) -> Result<execution::Model, String> {
    super::Bundle::compile(source)
}

/// Lowers provider behavior graphs into shared mocking execution plans.
///
/// Membership, progression and span parenting remain separate relationships.
/// Acyclic nested behavior uses a static plan. Cycles and junctions retain
/// occurrence scopes, queues and group closure in a graph plan.
/// Both consumers enforce traversal limits and a 10000 occurrence budget.
/// One trace has one entry span, so loop-back targets must be its descendants.
/// Authored error payloads are trace data, not failed runtime operations.
/// Expression errors abort generation. Mocking does not dispatch cancellable operations.
pub fn lower(model: &graph::Model) -> Result<execution::Model, String> {
    let mut result = execution::Model {
        ordered: true,
        ..Default::default()
    };
    for (index, node) in model.nodes.iter().enumerate() {
        if !inherits(model, &node.kind, "agent") {
            continue;
        }
        let agent = graph::NodeId(index);
        let members: HashSet<_> = model
            .edges
            .iter()
            .filter(|edge| edge.source == agent && inherits(model, &edge.kind, "contains"))
            .map(|edge| edge.target)
            .collect();
        let entry = model
            .edges
            .iter()
            .find(|edge| edge.source == agent && inherits(model, &edge.kind, "next"))
            .map(|edge| edge.target);
        // child-owned computed fields need an occurrence-local binding cache
        let computed_children = members.iter().any(|id| {
            Some(*id) != entry
                && inherits(model, &model.nodes[id.0].kind, "span")
                && model.nodes[id.0].fields.iter().any(|field| {
                    let binding = &model.bindings[field.0];
                    binding.path.len() == 1
                        && !matches!(
                            binding.value,
                            graph::Value::Input { .. }
                                | graph::Value::Str(_)
                                | graph::Value::Num(_)
                                | graph::Value::Bool(_)
                                | graph::Value::Null
                                | graph::Value::Func {
                                    func: graph::Func::Range(_)
                                        | graph::Func::Normal { .. }
                                        | graph::Func::Lognormal { .. }
                                        | graph::Func::Exponential { .. }
                                        | graph::Func::Pareto { .. }
                                        | graph::Func::Beta { .. }
                                        | graph::Func::Poisson { .. }
                                        | graph::Func::Chance { .. }
                                        | graph::Func::Uuid
                                        | graph::Func::Hex { .. }
                                        | graph::Func::Alphanum { .. },
                                    ..
                                }
                        )
                        && !matches!(
                            binding.path[0].as_str(),
                            "input"
                                | "output"
                                | "expected"
                                | "error"
                                | "metadata"
                                | "metrics"
                                | "tags"
                                | "scores"
                                | "duration"
                                | "score"
                                | "reason"
                        )
                })
        });
        let nested_dependencies = model.bindings.iter().any(|binding| {
            let graph::Value::BindingRef(target, _) = binding.value else {
                return false;
            };
            let target = &model.bindings[target.0];
            matches!(binding.owner, graph::ElementId::Node(node) if members.contains(&node))
                && target.owner != binding.owner
                && target.path.len() > 1
                && matches!(target.path[0].as_str(), "metadata" | "metrics")
        });
        if computed_children
            || nested_dependencies
            || model.edges.iter().any(|edge| edge.cycle && members.contains(&edge.source))
            || model.junctions.iter().any(|junction| members.contains(&junction.target))
        {
            lower_flow(model, agent, &members, &mut result)?;
            continue;
        }
        let mut lowerer = AgentLowering::new(model, agent, &mut result)?;
        lowerer.lower()?;
    }
    for (index, node) in model.nodes.iter().enumerate() {
        if !inherits(model, &node.kind, "scorer") {
            continue;
        }
        let id = graph::NodeId(index);
        let mut lowerer = AgentLowering {
            graph: model,
            agent: id,
            root: id,
            order: vec![id],
            guards: HashMap::new(),
            parents: HashMap::new(),
            inputs: HashMap::new(),
            building: vec![],
            callable: true,
            flow: false,
            result: &mut result,
        };
        let kind = if let Some(judge) = lowerer.binding(id, "judge") {
            if lowerer.binding(id, "score").is_some() || lowerer.binding(id, "steps").is_some() {
                return Err("a scorer must select expressions or a judge".into());
            }
            let execution::Value::Object(judge) = lowerer.binding_value(judge)? else {
                return Err("judge must be an object".into());
            };
            let field = |key: &str| {
                judge
                    .elem
                    .iter()
                    .find(|field| field.key == key)
                    .map(|field| field.value.clone())
                    .ok_or_else(|| format!("judge needs {key}"))
            };
            let execution::Value::Str(model) = field("model")? else {
                return Err("judge model must be a string".into());
            };
            let prompt = field("prompt")?;
            let execution::Value::Object(options) = field("options")? else {
                return Err("judge options must be an object".into());
            };
            let options = options
                .elem
                .into_iter()
                .map(|field| {
                    let score = match field.value {
                        execution::Value::Num(execution::Number::Int(n)) => n as f64,
                        execution::Value::Num(execution::Number::Float(n)) => n,
                        _ => return Err("judge scores must be constants".to_owned()),
                    };
                    if !(0.0..=1.0).contains(&score) {
                        return Err("judge scores must be between zero and one".into());
                    }
                    Ok(execution::JudgeOption { label: field.key, score })
                })
                .collect::<Result<Vec<_>, String>>()?;
            if options.is_empty() {
                return Err("judge needs at least one option".into());
            }
            execution::ScorerKind::Judge { model, prompt, options }
        } else {
            let score = lowerer.binding(id, "score").ok_or("scorer needs a score or judge field")?;
            let score = lowerer.binding_value(score)?;
            let steps = lowerer.binding(id, "steps").map(|id| lowerer.binding_value(id)).transpose()?;
            execution::ScorerKind::Code {
                score,
                steps: scorer_steps(steps)?,
            }
        };
        let mut lang = None;
        let mut file = None;
        for prop in &node.props {
            let binding = &model.bindings[prop.0];
            match binding.path[0].as_str() {
                "lang" => {
                    lang = Some(match &binding.value {
                        graph::Value::Str(value) if value == "python" => execution::ScorerLang::Python,
                        graph::Value::Str(value) if value == "typescript" => execution::ScorerLang::Typescript,
                        _ => return Err("scorer lang must be python or typescript".into()),
                    })
                }
                "file" => match &binding.value {
                    graph::Value::Str(value) => file = Some(value.clone()),
                    _ => return Err("scorer file must be a constant string".into()),
                },
                _ => {}
            }
        }
        let name = lowerer.name(id)?;
        result.scorers.push(execution::Scorer { name, lang, file, kind });
    }
    lower_resources(model, &mut result)?;
    if model.nodes.is_empty() {
        return Ok(result);
    }
    if result.agents.is_empty()
        && result.scorers.is_empty()
        && result.datasets.is_empty()
        && result.experiments.is_empty()
        && result.facets.is_empty()
        && result.automations.is_empty()
    {
        return Err("the graph declares no agent or scorer nodes".into());
    }
    Ok(result)
}

fn field<'a>(graph: &'a graph::Model, node: graph::NodeId, name: &str) -> Option<&'a graph::Value> {
    graph.nodes[node.0].fields.iter().find_map(|id| {
        let binding = &graph.bindings[id.0];
        (binding.path == [name]).then_some(&binding.value)
    })
}

fn resolved<'a>(graph: &'a graph::Model, mut value: &'a graph::Value) -> &'a graph::Value {
    for _ in 0..=graph.bindings.len() {
        let graph::Value::BindingRef(id, _) = value else {
            return value;
        };
        value = &graph.bindings[id.0].value;
    }
    value
}

fn node_ref(graph: &graph::Model, value: &graph::Value, kind: Option<&str>) -> Result<graph::NodeId, String> {
    let graph::Value::GraphRef { ref_id, .. } = resolved(graph, value) else {
        return Err("expected a node reference".into());
    };
    let graph::ElementId::Node(node) = graph.refs[ref_id.0].target else {
        return Err("expected a node reference".into());
    };
    if kind.is_some_and(|kind| !inherits(graph, &graph.nodes[node.0].kind, kind)) {
        return Err(format!("expected a {} node", kind.unwrap()));
    }
    Ok(node)
}

fn node_refs(graph: &graph::Model, value: &graph::Value, kind: Option<&str>) -> Result<Vec<graph::NodeId>, String> {
    let graph::Value::Array(array) = resolved(graph, value) else {
        return Err("expected an array of node references".into());
    };
    array
        .elem
        .iter()
        .map(|item| match item {
            graph::ArrayElem::Item(value) => node_ref(graph, value, kind),
            _ => Err("resource references must be explicit array entries".into()),
        })
        .collect()
}

fn node_name(graph: &graph::Model, node: graph::NodeId) -> Result<String, String> {
    for id in &graph.nodes[node.0].props {
        if graph.bindings[id.0].path == ["name"] {
            if let graph::Value::Str(name) = &graph.bindings[id.0].value {
                return Ok(name.clone());
            }
            return Err("resource names must be constant strings".into());
        }
    }
    graph.nodes[node.0]
        .name
        .clone()
        .ok_or("provider resources must have names".into())
}

fn span_ref(graph: &graph::Model, node: graph::NodeId, result: &mut execution::Model) -> Result<execution::RefId, String> {
    if inherits(graph, &graph.nodes[node.0].kind, "agent") {
        let name = node_name(graph, node)?;
        return result
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .map(|agent| agent.entrypoint)
            .ok_or("agent has no entrypoint".into());
    }
    fn locate(children: &[execution::Child], node: execution::NodeId) -> Option<Vec<execution::Step>> {
        for child in children {
            let execution::Child::Span(span) = child else { continue };
            let step = execution::Step::Child {
                candidates: vec![span.node],
                position: None,
            };
            if span.node == node {
                return Some(vec![step]);
            }
            if let Some(mut rest) = locate(&span.children, node) {
                rest.insert(0, step);
                return Some(rest);
            }
        }
        None
    }
    let id = execution::NodeId(node.0 as u32);
    for trace in &result.traces {
        let steps = if trace.node == id {
            Some(vec![])
        } else {
            locate(&trace.children, id)
        };
        if let Some(steps) = steps {
            let reference = execution::ResolvedRef {
                module: Some(trace.node),
                up: 0,
                steps,
                accessor: execution::Accessor::Block {
                    node: id,
                    kind: if trace.node == id { "trace" } else { "task" },
                },
                path: vec![],
                range: graph.nodes[node.0].range,
            };
            let id = execution::RefId(result.refs.len() as u32);
            result.refs.push(reference);
            return Ok(id);
        }
    }
    Err("resource task must reference a statically reachable emitted span".into())
}

fn resource_value(
    graph: &graph::Model,
    node: graph::NodeId,
    value: &graph::Value,
    result: &mut execution::Model,
) -> Result<execution::Value, String> {
    AgentLowering {
        graph,
        agent: node,
        root: node,
        order: vec![node],
        guards: HashMap::new(),
        parents: HashMap::new(),
        inputs: HashMap::new(),
        building: vec![],
        callable: true,
        flow: false,
        result,
    }
    .value(value)
}

fn lower_flow(
    graph: &graph::Model,
    agent: graph::NodeId,
    members: &HashSet<graph::NodeId>,
    result: &mut execution::Model,
) -> Result<(), String> {
    let entries: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.source == agent && inherits(graph, &edge.kind, "next"))
        .collect();
    if entries.len() != 1 || !entries[0].props.is_empty() || entries[0].fork.is_some() || entries[0].cycle {
        return Err("agent needs one unconditional entry edge".into());
    }
    let root = entries[0].target;
    if !members.contains(&root) {
        return Err("agent entry must be a member".into());
    }
    if !inherits(graph, &graph.nodes[root.0].kind, "task") && graph.nodes[root.0].kind != "span" {
        return Err("the entry must be a task or span".into());
    }
    let mut parents = HashMap::new();
    for edge in &graph.edges {
        if members.contains(&edge.source)
            && inherits(graph, &edge.kind, "child_of")
            && (!members.contains(&edge.target) || !edge.props.is_empty() || parents.insert(edge.source, edge.target).is_some())
        {
            return Err("child_of needs one unconfigured member parent".into());
        }
    }
    if parents.contains_key(&root) {
        return Err("entry span cannot have a parent".into());
    }
    for node in members {
        let mut cursor = *node;
        let mut seen = HashSet::new();
        while cursor != root {
            if !seen.insert(cursor) {
                return Err("span parenting cannot contain cycles".into());
            }
            cursor = *parents
                .get(&cursor)
                .ok_or("each member needs a child_of path to the entry span")?;
        }
    }
    let mut lowerer = AgentLowering {
        graph,
        agent,
        root,
        order: (0..graph.nodes.len())
            .map(graph::NodeId)
            .filter(|n| members.contains(n))
            .collect(),
        guards: HashMap::new(),
        parents: parents.clone(),
        inputs: HashMap::new(),
        building: vec![],
        callable: false,
        flow: true,
        result,
    };
    let input = lowerer.binding(agent, "input").ok_or("agent needs input")?;
    let root_input = lowerer.binding(root, "input").ok_or("entry span needs input")?;
    if !matches!(graph.bindings[root_input.0].value, graph::Value::BindingRef(id, _) if id == input) {
        return Err("entry span input must reference the agent input".into());
    }
    let output = lowerer.binding(agent, "output").ok_or("agent needs output")?;
    let output = lowerer.binding_value(output)?;
    let mut nodes = vec![];
    for node in lowerer.order.clone() {
        let kind = ["scorer", "tool", "llm", "function", "task", "span", "vars", "node"]
            .into_iter()
            .find(|kind| inherits(graph, &graph.nodes[node.0].kind, kind))
            .ok_or("unsupported graph execution node kind")?;
        let mut bindings = Vec::new();
        for (id, binding) in graph.bindings.iter().enumerate() {
            if binding.owner == graph::ElementId::Node(node)
                || (node == root
                    && binding.owner == graph::ElementId::Node(agent)
                    && binding.path.first().is_some_and(|key| key == "input"))
            {
                bindings.push((id, lowerer.binding_value(graph::BindingId(id))?));
            }
        }
        let fields = graph.nodes[node.0]
            .fields
            .iter()
            .filter_map(|id| {
                let binding = &graph.bindings[id.0];
                (binding.path.len() == 1).then(|| (binding.path[0].clone(), id.0))
            })
            .collect();
        nodes.push(execution::FlowNode {
            id: AgentLowering::id(node),
            key: graph.nodes[node.0].name.clone().unwrap_or_else(|| format!("node_{}", node.0)),
            name: lowerer.name(node)?,
            kind: kind.into(),
            parent: parents.get(&node).copied().map(AgentLowering::id),
            bindings,
            fields,
        });
    }
    let mut edges = vec![];
    for (id, edge) in graph.edges.iter().enumerate() {
        if !members.contains(&edge.source) || inherits(graph, &edge.kind, "child_of") {
            continue;
        }
        if !members.contains(&edge.target) || edge.direction != graph::Direction::Directed {
            return Err("execution edges require directed member endpoints".into());
        }
        if edge.cycle && edge.target == root {
            return Err("cycle edges cannot target the trace entry span".into());
        }
        let kind = ["next", "feeds", "link", "requires", "contains"]
            .into_iter()
            .find(|kind| inherits(graph, &edge.kind, kind))
            .ok_or("unsupported execution edge kind")?;
        let mut props = vec![];
        let mut into = None;
        for prop in &edge.props {
            let binding = &graph.bindings[prop.0];
            if binding.path == ["into"] {
                if let graph::Value::FieldRef(target) = binding.value {
                    if graph.bindings[target.0].owner != graph::ElementId::Node(edge.target)
                        || !matches!(graph.bindings[target.0].value, graph::Value::Input { .. })
                    {
                        return Err("feeds destination must declare an upstream field on its target".into());
                    }
                    into = Some(target.0)
                }
            } else {
                if !["when", "weight", "value", "max_traversals"].contains(&binding.path[0].as_str()) {
                    return Err(format!("unsupported execution edge property {}", binding.path[0]));
                }
                props.push((binding.path[0].clone(), lowerer.flow_property(*prop)?));
            }
        }
        if kind == "feeds" && (into.is_none() || !props.iter().any(|(key, _)| key == "value")) {
            return Err("feeds requires value and into".into());
        }
        let bundle = graph
            .paths
            .iter()
            .chain(graph.forks.iter().flat_map(|fork| fork.continuations.iter()))
            .flat_map(|path| path.steps.iter())
            .find_map(|step| match step {
                graph::PathStep::Edges(ids) if ids.contains(&graph::EdgeId(id)) => ids.first().map(|id| id.0),
                _ => None,
            })
            .unwrap_or(id);
        edges.push(execution::FlowEdge {
            id,
            bundle,
            name: edge
                .name
                .clone()
                .map(execution::Value::Str)
                .unwrap_or(execution::Value::Num(execution::Number::Int(id as i64))),
            source: AgentLowering::id(edge.source),
            target: AgentLowering::id(edge.target),
            kind: kind.into(),
            cycle: edge.cycle,
            into,
            props,
            fork: edge.fork.map(|f| f.0),
        });
    }
    let mut forks = vec![];
    for (id, fork) in graph
        .forks
        .iter()
        .enumerate()
        .filter(|(_, fork)| members.contains(&fork.source))
    {
        let kind = ["parallel", "conditional", "exclusive", "weighted", "each", "repeat"]
            .into_iter()
            .find(|kind| inherits(graph, &fork.kind, kind))
            .ok_or("unsupported fork execution kind")?;
        let props = fork
            .props
            .iter()
            .map(|id| Ok((graph.bindings[id.0].path[0].clone(), lowerer.flow_property(*id)?)))
            .collect::<Result<_, String>>()?;
        forks.push(execution::FlowFork {
            id,
            source: AgentLowering::id(fork.source),
            kind: kind.into(),
            branches: fork.branches.iter().map(|b| b.iter().map(|e| e.0).collect()).collect(),
            props,
        });
    }
    let mut junctions = vec![];
    for junction in graph.junctions.iter().filter(|j| members.contains(&j.target)) {
        let kind = ["accumulate", "merge", "first", "quorum", "reduce"]
            .into_iter()
            .find(|kind| inherits(graph, &junction.kind, kind))
            .ok_or("unsupported junction execution kind")?;
        if junction.inputs.iter().any(|id| !edges.iter().any(|edge| edge.id == id.0)) {
            return Err("junction inputs must belong to the current agent".into());
        }
        let props = junction
            .props
            .iter()
            .map(|id| Ok((graph.bindings[id.0].path[0].clone(), lowerer.flow_property(*id)?)))
            .collect::<Result<_, String>>()?;
        junctions.push(execution::FlowJunction {
            target: AgentLowering::id(junction.target),
            kind: kind.into(),
            inputs: junction.inputs.iter().map(|e| e.0).collect(),
            props,
            fork: junction.fork.map(|f| f.0),
        });
    }
    let root_id = AgentLowering::id(root);
    let fields = lowerer.fields(root)?;
    let name = lowerer.name(root)?;
    lowerer.result.traces.push(execution::Trace {
        node: root_id,
        name,
        fields,
        bindings: vec![],
        children: vec![],
    });
    let entrypoint = execution::RefId(lowerer.result.refs.len() as u32);
    lowerer.result.refs.push(execution::ResolvedRef {
        module: Some(root_id),
        up: 0,
        steps: vec![],
        accessor: execution::Accessor::Block {
            node: root_id,
            kind: "trace",
        },
        path: vec![],
        range: graph.nodes[agent.0].range,
    });
    lowerer.result.agents.push(execution::Agent {
        name: node_name(graph, agent)?,
        entrypoint,
        output,
    });
    lowerer.result.flows.push(execution::Flow {
        root: root_id,
        input: input.0,
        nodes,
        edges,
        forks,
        junctions,
    });
    Ok(())
}

fn lower_resources(graph: &graph::Model, result: &mut execution::Model) -> Result<(), String> {
    for (index, node) in graph.nodes.iter().enumerate() {
        let id = graph::NodeId(index);
        if !["dataset", "experiment", "facet", "automation"]
            .iter()
            .any(|kind| inherits(graph, &node.kind, kind))
        {
            continue;
        }
        let name = node_name(graph, id)?;
        let required = |key| field(graph, id, key).ok_or_else(|| format!("{name} needs {key}"));
        let description = field(graph, id, "description")
            .map(|value| match resolved(graph, value) {
                graph::Value::Str(value) => Ok(value.clone()),
                _ => Err("description must be a constant string".to_owned()),
            })
            .transpose()?;
        let string = |key: &str| -> Result<Option<String>, String> {
            field(graph, id, key)
                .map(|value| match resolved(graph, value) {
                    graph::Value::Str(value) => Ok(value.clone()),
                    _ => Err(format!("{name}.{key} must be a constant string")),
                })
                .transpose()
        };
        let boolean = |key: &str, default: bool| -> Result<bool, String> {
            match field(graph, id, key).map(|value| resolved(graph, value)) {
                None => Ok(default),
                Some(graph::Value::Bool(value)) => Ok(*value),
                _ => Err(format!("{name}.{key} must be a constant boolean")),
            }
        };
        let strings = |key: &str| -> Result<Vec<String>, String> {
            let Some(value) = field(graph, id, key) else {
                return Ok(vec![]);
            };
            let graph::Value::Array(array) = resolved(graph, value) else {
                return Err(format!("{name}.{key} must be an array"));
            };
            array
                .elem
                .iter()
                .map(|item| match item {
                    graph::ArrayElem::Item(value) => match resolved(graph, value) {
                        graph::Value::Str(value) => Ok(value.clone()),
                        _ => Err(format!("{name}.{key} must contain constant strings")),
                    },
                    _ => Err(format!("{name}.{key} must contain explicit entries")),
                })
                .collect()
        };
        if inherits(graph, &node.kind, "facet") {
            result.facets.push(execution::Facet {
                prompt: string("prompt")?.ok_or_else(|| format!("{name} needs prompt"))?,
                no_match_pattern: string("no_match_pattern")?,
                name,
                description,
            });
        } else if inherits(graph, &node.kind, "automation") {
            let kind = match string("type")?.as_deref() {
                Some("scorer") => {
                    let scorers = strings("scorers")?;
                    if scorers.is_empty() {
                        return Err(format!("{name} needs scorers"));
                    }
                    if field(graph, id, "facets").is_some() {
                        return Err(format!("{name} cannot mix scorers and facets"));
                    }
                    if string("scope")?.as_deref().is_some_and(|scope| scope != "span") {
                        return Err(format!("{name} scorer scope must be span"));
                    }
                    execution::AutomationKind::Scorer {
                        scorers,
                        root: boolean("root", false)?,
                        span_names: strings("span_names")?,
                    }
                }
                Some("topics") => {
                    let facets = strings("facets")?;
                    if facets.is_empty() {
                        return Err(format!("{name} needs facets"));
                    }
                    if field(graph, id, "scorers").is_some()
                        || field(graph, id, "root").is_some()
                        || field(graph, id, "span_names").is_some()
                    {
                        return Err(format!("{name} topics cannot configure scorer targeting"));
                    }
                    if string("scope")?.as_deref().is_some_and(|scope| scope != "trace") {
                        return Err(format!("{name} topics scope must be trace"));
                    }
                    execution::AutomationKind::Topics { facets }
                }
                _ => return Err(format!("{name} type must be scorer or topics")),
            };
            let sampling_rate = match field(graph, id, "sampling_rate").map(|v| resolved(graph, v)) {
                None => 1.0,
                Some(graph::Value::Num(graph::Number::Int(value))) => *value as f64,
                Some(graph::Value::Num(graph::Number::Float(value))) => *value,
                _ => return Err(format!("{name} sampling_rate must be a constant number")),
            };
            if !sampling_rate.is_finite() || !(0.0..=1.0).contains(&sampling_rate) {
                return Err(format!("{name} sampling_rate must be between zero and one"));
            }
            let enabled = boolean("enabled", true)?;
            result.automations.push(execution::Automation {
                name,
                kind,
                sampling_rate,
                enabled,
            });
        } else if inherits(graph, &node.kind, "dataset") {
            let mut cases = Vec::new();
            for case in node_refs(graph, required("cases")?, Some("case"))? {
                let source = if let Some(value) = field(graph, case, "input") {
                    if field(graph, case, "source").is_some() || field(graph, case, "traces").is_some() {
                        return Err("case needs exactly one input source".into());
                    }
                    execution::DatasetSource::Inline(resource_value(graph, case, value, result)?)
                } else if let Some(value) = field(graph, case, "source") {
                    if field(graph, case, "traces").is_some() {
                        return Err("case needs exactly one input source".into());
                    }
                    let target = node_ref(graph, value, None)?;
                    let reference = span_ref(graph, target, result)?;
                    if result.refs[reference.0 as usize].steps.is_empty() {
                        execution::DatasetSource::Trace(reference)
                    } else {
                        execution::DatasetSource::Span(reference)
                    }
                } else if let Some(value) = field(graph, case, "traces") {
                    let refs = node_refs(graph, value, None)?
                        .into_iter()
                        .map(|node| span_ref(graph, node, result))
                        .collect::<Result<_, _>>()?;
                    execution::DatasetSource::Group(refs)
                } else {
                    return Err("case needs input, source or traces".into());
                };
                cases.push(execution::DatasetCase {
                    name: node_name(graph, case)?,
                    source,
                    expected: field(graph, case, "expected")
                        .map(|v| resource_value(graph, case, v, result))
                        .transpose()?,
                    metadata: field(graph, case, "metadata")
                        .map(|v| resource_value(graph, case, v, result))
                        .transpose()?,
                    tags: field(graph, case, "tags")
                        .map(|v| resource_value(graph, case, v, result))
                        .transpose()?,
                });
            }
            result.datasets.push(execution::Dataset {
                name,
                description,
                cases,
            });
        } else {
            let dataset = node_name(graph, node_ref(graph, required("dataset")?, Some("dataset"))?)?;
            let task = span_ref(graph, node_ref(graph, required("task")?, None)?, result)?;
            let scorers = node_refs(graph, required("scorers")?, Some("scorer"))?
                .into_iter()
                .map(|id| node_name(graph, id))
                .collect::<Result<_, _>>()?;
            let baseline = field(graph, id, "baseline")
                .map(|value| node_ref(graph, value, Some("experiment")).and_then(|id| node_name(graph, id)))
                .transpose()?;
            result.experiments.push(execution::Experiment {
                name,
                description,
                dataset,
                task,
                scorers,
                baseline,
            });
        }
    }
    Ok(())
}

fn scorer_steps(value: Option<execution::Value>) -> Result<Vec<execution::ScorerStep>, String> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let execution::Value::Array(array) = value else {
        return Err("scorer steps must be an array".into());
    };
    array
        .elem
        .into_iter()
        .map(|item| {
            let execution::ArrayElem::Item(execution::Value::Object(object)) = item else {
                return Err("scorer steps must be objects".into());
            };
            let mut fields: HashMap<_, _> = object.elem.into_iter().map(|field| (field.key, field.value)).collect();
            let steps = scorer_steps(fields.remove("steps"))?;
            if let Some(count) = fields.remove("repeat") {
                if !fields.is_empty() {
                    return Err("repeat steps accept only repeat and steps".into());
                }
                Ok(execution::ScorerStep::Repeat { count, steps })
            } else {
                let cond = fields.remove("when").ok_or("scorer step needs when or repeat")?;
                let score = fields.remove("score");
                if !fields.is_empty() {
                    return Err("conditional steps accept only when, score and steps".into());
                }
                Ok(execution::ScorerStep::When(execution::When { cond, score, steps }))
            }
        })
        .collect()
}

fn inherits(model: &graph::Model, kind: &str, base: &str) -> bool {
    let mut name = kind;
    for _ in 0..=model.kinds.len() {
        if name == base {
            return true;
        }
        let Some(id) = model.kind_names.get(name) else { return false };
        name = &model.kinds[id.0].base;
    }
    false
}

struct AgentLowering<'a> {
    graph: &'a graph::Model,
    agent: graph::NodeId,
    root: graph::NodeId,
    order: Vec<graph::NodeId>,
    guards: HashMap<graph::NodeId, Vec<graph::BindingId>>,
    parents: HashMap<graph::NodeId, graph::NodeId>,
    inputs: HashMap<graph::BindingId, graph::BindingId>,
    building: Vec<graph::BindingId>,
    callable: bool,
    flow: bool,
    result: &'a mut execution::Model,
}

impl<'a> AgentLowering<'a> {
    fn new(graph: &'a graph::Model, agent: graph::NodeId, result: &'a mut execution::Model) -> Result<Self, String> {
        let mut members = HashSet::new();
        for edge in &graph.edges {
            if edge.source == agent && inherits(graph, &edge.kind, "contains") && !members.insert(edge.target) {
                return Err("duplicate agent membership".into());
            }
        }
        if members.is_empty() {
            return Err("agent needs explicit contains membership".into());
        }
        for fork in &graph.forks {
            if fork.source != agent && !members.contains(&fork.source) {
                continue;
            }
            if fork.source == agent && inherits(graph, &fork.kind, "weighted") {
                return Err("agent entry must be unconditional, not a weighted selection".into());
            }
            if !["parallel", "conditional", "exclusive", "each", "repeat", "weighted"]
                .iter()
                .any(|kind| inherits(graph, &fork.kind, kind))
                || (!inherits(graph, &fork.kind, "each") && !inherits(graph, &fork.kind, "repeat") && !fork.props.is_empty())
            {
                return Err(format!(
                    "fork kind {:?} or its configuration is not supported by mocking yet",
                    fork.kind
                ));
            }
        }
        let mut next: HashMap<graph::NodeId, Vec<(graph::NodeId, Vec<graph::BindingId>)>> = HashMap::new();
        let mut parents = HashMap::new();
        let mut inputs = HashMap::new();
        for edge in &graph.edges {
            if edge.source != agent && !members.contains(&edge.source) {
                continue;
            }
            if edge.cycle || edge.direction != graph::Direction::Directed {
                return Err("mocking requires directed paths without cycle edges".into());
            }
            if !members.contains(&edge.target) {
                return Err("agent edges must target a member of the same agent".into());
            }
            let kind = &edge.kind;
            if inherits(graph, kind, "contains") && edge.source == agent {
                continue;
            }
            if inherits(graph, kind, "next") {
                let mut guards = Vec::new();
                for id in &edge.props {
                    match graph.bindings[id.0].path[0].as_str() {
                        "when" => guards.push(*id),
                        "weight"
                            if graph.forks.iter().any(|fork| {
                                inherits(graph, &fork.kind, "weighted")
                                    && fork
                                        .branches
                                        .iter()
                                        .flatten()
                                        .any(|id| std::ptr::eq(&graph.edges[id.0], edge))
                            }) => {}
                        _ => return Err("configured next edges are not supported by mocking yet".into()),
                    }
                }
                let targets = next.entry(edge.source).or_default();
                if targets.iter().any(|(target, _)| *target == edge.target) {
                    return Err("multiple next edges to one target require occurrence policies".into());
                }
                targets.push((edge.target, guards));
            } else if inherits(graph, kind, "child_of") {
                if edge.source == agent || !edge.props.is_empty() {
                    return Err("child_of requires span endpoints without configuration".into());
                }
                if parents.insert(edge.source, edge.target).is_some() {
                    return Err("a span cannot have multiple child_of parents".into());
                }
            } else if inherits(graph, kind, "feeds") {
                let mut value = None;
                let mut into = None;
                for id in &edge.props {
                    let binding = &graph.bindings[id.0];
                    match binding.path.first().map(String::as_str) {
                        Some("value") => value = Some(*id),
                        Some("into") => {
                            if let graph::Value::FieldRef(id) = binding.value {
                                into = Some(id);
                            }
                        }
                        Some("weight" | "when")
                            if graph.forks.iter().any(|fork| {
                                inherits(graph, &fork.kind, "weighted")
                                    && fork
                                        .branches
                                        .iter()
                                        .flatten()
                                        .any(|id| std::ptr::eq(&graph.edges[id.0], edge))
                            }) => {}
                        _ => return Err("configured feeds edges are not supported by mocking yet".into()),
                    }
                }
                let value = value.ok_or("feeds needs a value")?;
                let Some(into) = into else { continue };
                if graph.bindings[into.0].owner != graph::ElementId::Node(edge.target) {
                    return Err("feeds destination must belong to its target".into());
                }
                if !matches!(graph.bindings[into.0].value, graph::Value::Input { .. }) {
                    return Err("feeds destination must declare an upstream value with <-".into());
                }
                if inputs.insert(into, value).is_some() {
                    return Err("multiple feeds to one field require an accumulation policy".into());
                }
            } else {
                return Err(format!("edge kind {kind:?} is not supported by mocking yet"));
            }
        }
        let mut joined = HashSet::new();
        for junction in &graph.junctions {
            if !members.contains(&junction.target) {
                continue;
            }
            if !inherits(graph, &junction.kind, "accumulate") {
                return Err("mocking currently requires accumulate junctions".into());
            }
            if !joined.insert(junction.target) {
                return Err("multiple junction groups to one target require occurrence policies".into());
            }
            let fork = junction
                .fork
                .ok_or("mocking junctions need an attached fork to define their group")?;
            let source = graph.forks[fork.0].source;
            if !members.contains(&source) {
                return Err("junction group must belong to the same agent".into());
            }
            for id in &junction.props {
                let binding = &graph.bindings[id.0];
                if binding.path != ["observe"]
                    || !matches!(&binding.value, graph::Value::Str(mode) if mode == "arrival" || mode == "result")
                {
                    return Err("mocking junctions support observe with one group per fork activation".into());
                }
            }
            next.entry(source).or_default().push((junction.target, vec![]));
        }
        let entries = next.get(&agent).ok_or("agent needs one next entry edge")?;
        if entries.len() != 1 || !entries[0].1.is_empty() {
            return Err("mocking needs one unconditional entry span".into());
        }
        let root = entries[0].0;
        let mut order = vec![];
        let mut guards = HashMap::new();
        fn walk(
            node: graph::NodeId,
            inherited: Vec<graph::BindingId>,
            next: &HashMap<graph::NodeId, Vec<(graph::NodeId, Vec<graph::BindingId>)>>,
            order: &mut Vec<graph::NodeId>,
            guards: &mut HashMap<graph::NodeId, Vec<graph::BindingId>>,
        ) -> Result<(), String> {
            if guards.insert(node, inherited.clone()).is_some() {
                return Err("repeated arrivals require a junction or an occurrence policy".into());
            }
            order.push(node);
            for (target, local) in next.get(&node).into_iter().flatten() {
                let mut combined = inherited.clone();
                combined.extend(local);
                walk(*target, combined, next, order, guards)?;
            }
            Ok(())
        }
        walk(root, vec![], &next, &mut order, &mut guards)?;
        for edge in &graph.edges {
            if members.contains(&edge.source) && inherits(graph, &edge.kind, "feeds") {
                let source = order.iter().position(|node| *node == edge.source);
                let target = order.iter().position(|node| *node == edge.target);
                if source >= target {
                    return Err("feeds must follow progression order for straight-line mocking".into());
                }
            }
        }
        if parents.contains_key(&root) {
            return Err("the entry span cannot have a parent".into());
        }
        for (position, node) in order.iter().enumerate().skip(1) {
            let parent = parents.get(node).ok_or("each non-root span needs a child_of parent")?;
            if !order[..position].contains(parent) {
                return Err("a span parent must be entered before its child".into());
            }
        }
        // nested span execution must preserve the explicit progression order
        fn visit(
            node: graph::NodeId,
            order: &[graph::NodeId],
            parents: &HashMap<graph::NodeId, graph::NodeId>,
            out: &mut Vec<graph::NodeId>,
        ) {
            out.push(node);
            for child in order.iter().filter(|child| parents.get(child) == Some(&node)) {
                visit(*child, order, parents, out);
            }
        }
        let mut nested = vec![];
        visit(root, &order, &parents, &mut nested);
        if nested != order {
            return Err("progression reenters a completed parent span".into());
        }
        Ok(Self {
            graph,
            agent,
            root,
            order,
            guards,
            parents,
            inputs,
            building: vec![],
            callable: false,
            flow: false,
            result,
        })
    }

    fn id(node: graph::NodeId) -> execution::NodeId {
        execution::NodeId(node.0 as u32)
    }

    fn binding(&self, node: graph::NodeId, name: &str) -> Option<graph::BindingId> {
        self.graph.nodes[node.0]
            .fields
            .iter()
            .copied()
            .find(|id| self.graph.bindings[id.0].path == [name])
    }

    fn lower(&mut self) -> Result<(), String> {
        let agent = &self.graph.nodes[self.agent.0];
        let name = node_name(self.graph, self.agent)?;
        let root_kind = &self.graph.nodes[self.root.0].kind;
        if self.result.traces.iter().any(|trace| trace.node == Self::id(self.root)) {
            return Err("shared agent entry spans are not supported by mocking yet".into());
        }
        if !inherits(self.graph, root_kind, "task") && root_kind != "span" {
            return Err("the first mocking span must be a task or span".into());
        }
        let agent_input = self.binding(self.agent, "input").ok_or("agent needs an input field")?;
        let root_input = self.binding(self.root, "input").ok_or("entry span needs an input field")?;
        if !matches!(self.graph.bindings[root_input.0].value, graph::Value::BindingRef(id, _) if id == agent_input) {
            return Err("entry span input must reference the agent input for mocking".into());
        }
        let agent_output = self.binding(self.agent, "output").ok_or("agent needs an output field")?;
        let output = self.binding_value(agent_output)?;
        let mut fields = self.fields(self.root)?;
        fields.input = Some(self.binding_value(agent_input)?);
        let children = self.children(self.root)?;
        let mut bindings = vec![];
        // custom fields retain one cached value per graph binding and invocation
        for (index, binding) in self.graph.bindings.iter().enumerate() {
            if let graph::ElementId::Node(node) = binding.owner
                && self.order.contains(&node)
                && self.each_ancestor(node).is_none()
                && !binding.property
                && !matches!(resolved(self.graph, &binding.value), graph::Value::GraphRef { .. })
                && (!inherits(self.graph, &self.graph.nodes[node.0].kind, "span")
                    || Self::field(binding.path.first().map(String::as_str).unwrap_or("")).is_none())
            {
                bindings.push(execution::Binding {
                    name: format!("binding_{index}"),
                    value: self.binding_value(graph::BindingId(index))?,
                });
            }
        }
        for edge in &self.graph.edges {
            if !self.order.contains(&edge.source) || self.each_ancestor(edge.source).is_some() {
                continue;
            }
            for id in &edge.props {
                if self.graph.bindings[id.0].path == ["value"] {
                    bindings.push(execution::Binding {
                        name: format!("edge_value_{}", id.0),
                        value: self.binding_value(*id)?,
                    });
                }
            }
        }
        bindings.extend(self.fork_bindings(self.root)?);
        let guard_ids: std::collections::BTreeSet<_> = self
            .guards
            .values()
            .flatten()
            .copied()
            .chain(
                self.graph
                    .forks
                    .iter()
                    .filter(|fork| self.order.contains(&fork.source) && inherits(self.graph, &fork.kind, "weighted"))
                    .flat_map(|fork| fork.branches.iter().flatten())
                    .flat_map(|edge| self.graph.edges[edge.0].props.iter().copied())
                    .filter(|id| self.graph.bindings[id.0].path == ["when"]),
            )
            .collect();
        for id in guard_ids {
            if self.weighted_guard(id) {
                continue;
            }
            if let graph::ElementId::Edge(edge) = self.graph.bindings[id.0].owner
                && self.each_ancestor(self.graph.edges[edge.0].source).is_some()
            {
                continue;
            }
            bindings.push(execution::Binding {
                name: format!("guard_{}", id.0),
                value: self.binding_value(id)?,
            });
        }
        self.result.traces.push(execution::Trace {
            node: Self::id(self.root),
            name: self.name(self.root)?,
            fields,
            bindings,
            children,
        });
        let entrypoint = execution::RefId(self.result.refs.len() as u32);
        self.result.refs.push(execution::ResolvedRef {
            module: Some(Self::id(self.root)),
            up: 0,
            steps: vec![],
            accessor: execution::Accessor::Block {
                node: Self::id(self.root),
                kind: "trace",
            },
            path: vec![],
            range: agent.range,
        });
        self.result.agents.push(execution::Agent {
            name,
            entrypoint,
            output,
        });
        Ok(())
    }

    fn name(&self, node: graph::NodeId) -> Result<String, String> {
        let n = &self.graph.nodes[node.0];
        for id in &n.props {
            let binding = &self.graph.bindings[id.0];
            if binding.path == ["name"] {
                if let graph::Value::Str(name) = &binding.value {
                    return Ok(name.clone());
                }
                return Err("mocking span names must be constant strings".into());
            }
        }
        Ok(n.name.clone().unwrap_or_else(|| format!("{}_{}", n.kind, node.0)))
    }

    fn each(&self, node: graph::NodeId) -> Option<(usize, &graph::Fork)> {
        self.graph.forks.iter().enumerate().find(|(_, fork)| {
            (inherits(self.graph, &fork.kind, "each") || inherits(self.graph, &fork.kind, "repeat"))
                && fork.branches.iter().flatten().any(|id| self.graph.edges[id.0].target == node)
        })
    }

    fn each_id(&self, index: usize) -> execution::NodeId {
        execution::NodeId((self.graph.nodes.len() * 2 + index) as u32)
    }

    fn each_ancestor(&self, mut node: graph::NodeId) -> Option<(usize, &graph::Fork)> {
        loop {
            if let Some(fork) = self.each(node) {
                return Some(fork);
            }
            node = *self.parents.get(&node)?;
        }
    }

    fn fork_bindings(&mut self, node: graph::NodeId) -> Result<Vec<execution::Binding>, String> {
        let ids: Vec<_> = self
            .graph
            .forks
            .iter()
            .filter(|fork| fork.source == node)
            .flat_map(|fork| fork.props.iter().copied())
            .filter(|id| matches!(self.graph.bindings[id.0].path[0].as_str(), "count" | "items"))
            .collect();
        let mut bindings = self.weighted_bindings(node)?;
        bindings.extend(
            ids.into_iter()
                .map(|id| {
                    Ok(execution::Binding {
                        name: format!("fork_input_{}", id.0),
                        value: self.binding_value(id)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        );
        Ok(bindings)
    }

    fn weighted_branches(&self, node: graph::NodeId) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        for (index, fork) in self.graph.forks.iter().enumerate() {
            if !inherits(self.graph, &fork.kind, "weighted") {
                continue;
            }
            for (branch, edges) in fork.branches.iter().enumerate() {
                let mut pending: Vec<_> = edges.iter().map(|id| self.graph.edges[id.0].target).collect();
                let mut seen = HashSet::new();
                while let Some(current) = pending.pop() {
                    if !seen.insert(current) {
                        continue;
                    }
                    if current == node {
                        result.push((index, branch));
                        break;
                    }
                    pending.extend(
                        self.graph
                            .edges
                            .iter()
                            .filter(|edge| edge.source == current && !edge.cycle && inherits(self.graph, &edge.kind, "next"))
                            .map(|edge| edge.target),
                    );
                }
            }
        }
        result
    }

    fn node_condition(&self, node: graph::NodeId) -> execution::Value {
        let mut condition = self.condition(&self.guards[&node]);
        for (fork, branch) in self.weighted_branches(node) {
            let selected = execution::Value::Binary {
                op: execution::BinOp::Eq,
                lhs: Box::new(execution::Value::VarRef(format!("fork_pick_{fork}"))),
                rhs: Box::new(execution::Value::Num(execution::Number::Int(branch as i64))),
                range: self.graph.forks[fork].range,
            };
            condition = execution::Value::Binary {
                op: execution::BinOp::And,
                lhs: Box::new(condition),
                rhs: Box::new(selected),
                range: self.graph.nodes[node.0].range,
            };
        }
        condition
    }

    fn weighted_guard(&self, id: graph::BindingId) -> bool {
        let graph::ElementId::Edge(edge) = self.graph.bindings[id.0].owner else {
            return false;
        };
        self.graph.edges[edge.0]
            .fork
            .is_some_and(|fork| inherits(self.graph, &self.graph.forks[fork.0].kind, "weighted"))
    }

    fn weighted_owner(&self, fork: &graph::Fork) -> graph::NodeId {
        let mut owner = fork.source;
        loop {
            let encloses = fork.branches.iter().flatten().all(|id| {
                let mut node = self.graph.edges[id.0].target;
                loop {
                    if node == owner {
                        return true;
                    }
                    let Some(parent) = self.parents.get(&node) else { return false };
                    node = *parent;
                }
            });
            if encloses {
                return owner;
            }
            owner = self.parents[&owner];
        }
    }

    fn weighted_bindings(&mut self, owner: graph::NodeId) -> Result<Vec<execution::Binding>, String> {
        let forks: Vec<_> = self
            .graph
            .forks
            .iter()
            .enumerate()
            .filter(|(_, fork)| {
                self.order.contains(&fork.source)
                    && inherits(self.graph, &fork.kind, "weighted")
                    && self.weighted_owner(fork) == owner
            })
            .map(|(i, fork)| (i, fork.clone()))
            .collect();
        let mut bindings = Vec::new();
        for (index, fork) in forks {
            for id in fork
                .branches
                .iter()
                .flatten()
                .flat_map(|edge| self.graph.edges[edge.0].props.clone())
                .filter(|id| self.graph.bindings[id.0].path == ["when"])
                .collect::<Vec<_>>()
            {
                bindings.push(execution::Binding {
                    name: format!("guard_{}", id.0),
                    value: self.binding_value(id)?,
                });
            }
            let mut branches = Vec::new();
            for branch in &fork.branches {
                let mut guard = execution::Value::Bool(true);
                for edge in branch {
                    for id in &self.graph.edges[edge.0].props {
                        if self.graph.bindings[id.0].path == ["when"] {
                            guard = execution::Value::Binary {
                                op: execution::BinOp::And,
                                lhs: Box::new(guard),
                                rhs: Box::new(execution::Value::VarRef(format!("guard_{}", id.0))),
                                range: fork.range,
                            };
                        }
                    }
                }
                let mut weights = Vec::new();
                for edge in branch {
                    let id = *self.graph.edges[edge.0]
                        .props
                        .iter()
                        .find(|id| self.graph.bindings[id.0].path == ["weight"])
                        .ok_or("weighted branch needs weight")?;
                    weights.push(execution::Value::Cond {
                        cond: Box::new(guard.clone()),
                        then: Box::new(self.binding_value(id)?),
                        otherwise: Box::new(execution::Value::Num(execution::Number::Int(0))),
                    });
                }
                branches.push(weights);
            }
            bindings.push(execution::Binding {
                name: format!("fork_pick_{index}"),
                value: execution::Value::Func {
                    func: execution::Func::ForkPick(branches),
                    range: fork.range,
                },
            });
        }
        Ok(bindings)
    }

    fn iteration_bindings(&mut self, index: usize) -> Result<Vec<execution::Binding>, String> {
        let mut bindings = Vec::new();
        for (id, binding) in self.graph.bindings.iter().enumerate() {
            let name = match binding.owner {
                graph::ElementId::Node(node)
                    if self.each_ancestor(node).is_some_and(|(i, _)| i == index)
                        && !binding.property
                        && !matches!(resolved(self.graph, &binding.value), graph::Value::GraphRef { .. })
                        && (!inherits(self.graph, &self.graph.nodes[node.0].kind, "span")
                            || Self::field(&binding.path[0]).is_none()) =>
                {
                    Some(format!("binding_{id}"))
                }
                graph::ElementId::Edge(edge)
                    if self
                        .each_ancestor(self.graph.edges[edge.0].source)
                        .is_some_and(|(i, _)| i == index) =>
                {
                    match binding.path[0].as_str() {
                        "when" if !self.weighted_guard(graph::BindingId(id)) => Some(format!("guard_{id}")),
                        "value" => Some(format!("edge_value_{id}")),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(name) = name {
                bindings.push(execution::Binding {
                    name,
                    value: self.binding_value(graph::BindingId(id))?,
                });
            }
        }
        Ok(bindings)
    }

    fn junction(&self, node: graph::NodeId) -> Option<&graph::Junction> {
        self.graph.junctions.iter().find(|junction| junction.target == node)
    }

    fn condition(&self, guards: &[graph::BindingId]) -> execution::Value {
        guards
            .iter()
            .fold(execution::Value::Bool(true), |lhs, id| execution::Value::Binary {
                op: execution::BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(execution::Value::VarRef(format!("guard_{}", id.0))),
                range: self.graph.bindings[id.0].range,
            })
    }

    fn arrivals(&mut self, node: graph::NodeId) -> Result<execution::Value, String> {
        let junction = self
            .junction(node)
            .ok_or("context.arrivals requires a junction target")?
            .clone();
        let result = junction
            .props
            .iter()
            .any(|id| matches!(&self.graph.bindings[id.0].value, graph::Value::Str(value) if value == "result"));
        let mut elem = Vec::new();
        for id in junction.inputs {
            let edge = &self.graph.edges[id.0];
            let name = edge
                .name
                .clone()
                .map(execution::Value::Str)
                .unwrap_or(execution::Value::Num(execution::Number::Int(id.0 as i64)));
            let value = if result {
                match self.binding(edge.target, "output") {
                    Some(binding) => self.reference(binding)?,
                    None => execution::Value::Null,
                }
            } else {
                match edge.props.iter().find(|id| self.graph.bindings[id.0].path == ["value"]) {
                    Some(id) => execution::Value::VarRef(format!("edge_value_{}", id.0)),
                    None => execution::Value::Null,
                }
            };
            elem.push(execution::ArrayElem::Item(execution::Value::Object(execution::Object {
                elem: vec![
                    execution::ObjectField {
                        key: "edge".into(),
                        value: name,
                    },
                    execution::ObjectField {
                        key: "value".into(),
                        value,
                    },
                ],
            })));
        }
        Ok(execution::Value::Array(execution::Array { elem }))
    }

    fn guard_id(&self, node: graph::NodeId) -> execution::NodeId {
        execution::NodeId((self.graph.nodes.len() + node.0) as u32)
    }

    fn children(&mut self, parent: graph::NodeId) -> Result<Vec<execution::Child>, String> {
        let children: Vec<_> = self
            .order
            .iter()
            .copied()
            .filter(|node| self.parents.get(node) == Some(&parent))
            .collect();
        let children: Vec<_> = children
            .into_iter()
            .map(|node| {
                let kind = &self.graph.nodes[node.0].kind;
                let span = if kind == "node" || kind == "vars" {
                    execution::Child::Maybe(execution::Maybe {
                        node: Self::id(node),
                        name: self.graph.nodes[node.0].name.clone(),
                        chance: execution::Value::Num(execution::Number::Int(1)),
                        chance_range: self.graph.nodes[node.0].range,
                        bindings: self.fork_bindings(node)?,
                        children: self.children(node)?,
                    })
                } else if inherits(self.graph, kind, "scorer") {
                    if self.parents.values().any(|parent| *parent == node) {
                        return Err("scorer spans cannot contain child spans yet".into());
                    }
                    let score = self.binding(node, "score").ok_or("scorer needs a score field")?;
                    let reason = self
                        .binding(node, "reason")
                        .map(|id| self.binding_value(id).map(|value| (value, self.graph.bindings[id.0].range)))
                        .transpose()?;
                    execution::Child::Scorer(execution::ScorerSpan {
                        node: Self::id(node),
                        name: self.name(node)?,
                        score: self.binding_value(score)?,
                        score_range: self.graph.bindings[score.0].range,
                        reason,
                        bindings: self.fork_bindings(node)?,
                    })
                } else {
                    let kind = if inherits(self.graph, kind, "tool") {
                        execution::SpanKind::Tool
                    } else if inherits(self.graph, kind, "llm") {
                        execution::SpanKind::Llm
                    } else if inherits(self.graph, kind, "function") {
                        execution::SpanKind::Function
                    } else if inherits(self.graph, kind, "task") || kind == "span" {
                        execution::SpanKind::Task
                    } else {
                        return Err(format!("node kind {kind:?} is not supported by agent mocking yet"));
                    };
                    execution::Child::Span(execution::Span {
                        node: Self::id(node),
                        name: self.name(node)?,
                        kind,
                        fields: self.fields(node)?,
                        bindings: self.fork_bindings(node)?,
                        children: self.children(node)?,
                    })
                };
                let guards = &self.guards[&node];
                if guards.is_empty() && self.junction(node).is_none() && self.weighted_branches(node).is_empty() {
                    return Ok(span);
                }
                let condition = self.node_condition(node);
                let active = if let Some(junction) = self.junction(node) {
                    let ready =
                        junction
                            .inputs
                            .iter()
                            .fold(execution::Value::Bool(true), |ready, id| execution::Value::Binary {
                                op: execution::BinOp::And,
                                lhs: Box::new(ready),
                                rhs: Box::new(self.node_condition(self.graph.edges[id.0].target)),
                                range: junction.range,
                            });
                    execution::Value::Func {
                        func: execution::Func::Arrivals { ready: Box::new(ready) },
                        range: junction.range,
                    }
                } else {
                    execution::Value::Num(execution::Number::Int(1))
                };
                Ok(execution::Child::Maybe(execution::Maybe {
                    node: self.guard_id(node),
                    name: None,
                    chance: execution::Value::Cond {
                        cond: Box::new(condition),
                        then: Box::new(active),
                        otherwise: Box::new(execution::Value::Num(execution::Number::Int(0))),
                    },
                    chance_range: self.graph.nodes[node.0].range,
                    bindings: vec![],
                    children: vec![span],
                }))
            })
            .collect::<Result<_, String>>()?;
        let mut result = Vec::new();
        let mut groups = HashMap::new();
        for child in children {
            let node = match &child {
                execution::Child::Span(v) => graph::NodeId(v.node.0 as usize),
                execution::Child::Scorer(v) => graph::NodeId(v.node.0 as usize),
                execution::Child::Maybe(v) => graph::NodeId(v.node.0 as usize % self.graph.nodes.len()),
                _ => unreachable!(),
            };
            if let Some((index, fork)) = self.each(node) {
                if fork.source != parent {
                    return Err("each branch spans must be children of the fork source".into());
                }
                if let Some(position) = groups.get(&index) {
                    let execution::Child::Repeat(repeat) = &mut result[*position] else {
                        unreachable!()
                    };
                    repeat.children.push(child);
                    continue;
                }
                groups.insert(index, result.len());
                let repeated = inherits(self.graph, &fork.kind, "repeat");
                let property = if repeated { "count" } else { "items" };
                let input = *fork
                    .props
                    .iter()
                    .find(|id| self.graph.bindings[id.0].path == [property])
                    .ok_or("iteration fork needs its input")?;
                let range = self.graph.bindings[input.0].range;
                let name = fork.name.clone();
                let value = execution::Value::VarRef(format!("fork_input_{}", input.0));
                let count = if repeated {
                    value
                } else {
                    execution::Value::Func {
                        func: execution::Func::Len { target: Box::new(value) },
                        range,
                    }
                };
                result.push(execution::Child::Repeat(execution::Repeat {
                    node: self.each_id(index),
                    name,
                    count,
                    count_range: range,
                    bindings: self.iteration_bindings(index)?,
                    children: vec![child],
                }));
            } else {
                result.push(child);
            }
        }
        Ok(result)
    }

    fn field(name: &str) -> Option<execution::Field> {
        use execution::Field;
        Some(match name {
            "input" => Field::Input,
            "output" => Field::Output,
            "expected" => Field::Expected,
            "error" => Field::Error,
            "metadata" => Field::Metadata,
            "metrics" => Field::Metrics,
            "tags" => Field::Tags,
            _ => return None,
        })
    }

    fn fields(&mut self, node: graph::NodeId) -> Result<execution::SpanFields, String> {
        let mut fields = execution::SpanFields::default();
        for id in self.graph.nodes[node.0].fields.clone() {
            let binding = &self.graph.bindings[id.0];
            let name = binding.path[0].as_str();
            if Self::field(name).is_none() && name != "duration" && name != "scores" {
                continue;
            }
            let value = self.binding_value(id)?;
            match name {
                "input" => fields.input = Some(value),
                "output" => fields.output = Some(value),
                "expected" => fields.expected = Some(value),
                "error" => fields.error = Some(value),
                "metadata" | "metrics" => {
                    let execution::Value::Object(object) = value else {
                        return Err(format!("{name} must be an object expression for mocking"));
                    };
                    if name == "metadata" {
                        fields.metadata = Some(object);
                    } else {
                        fields.metrics = Some(object);
                    }
                }
                "tags" => {
                    let execution::Value::Array(array) = value else {
                        return Err("tags must be an array expression for mocking".into());
                    };
                    for item in array.elem {
                        fields.tags.push(match item {
                            execution::ArrayElem::Item(execution::Value::Str(text)) => execution::Template {
                                parts: vec![execution::Part::Lit(text)],
                            },
                            execution::ArrayElem::Item(execution::Value::Template(template)) => template,
                            _ => return Err("mocking tags must be strings".into()),
                        });
                    }
                }
                "duration" => fields.duration = Some((execution::Value::VarRef(format!("binding_{}", id.0)), binding.range)),
                "scores" => return Err("span scores are not supported by agent mocking yet".into()),
                _ => {}
            }
        }
        Ok(fields)
    }

    fn reference(&mut self, id: graph::BindingId) -> Result<execution::Value, String> {
        let binding = &self.graph.bindings[id.0];
        if self.flow {
            if let graph::ElementId::Node(node) = binding.owner {
                if node != self.agent && !self.order.contains(&node) {
                    return Err("graph occurrence references must belong to the current agent".into());
                }
                return Ok(execution::Value::Func {
                    func: execution::Func::FlowRead {
                        node: Self::id(if node == self.agent { self.root } else { node }),
                        binding: id.0,
                    },
                    range: binding.range,
                });
            }
            return self.flow_property(id);
        }
        if self.callable && binding.owner == graph::ElementId::Node(self.root) {
            return self.binding_value(id);
        }
        if self.callable && inherits(self.graph, &self.graph.nodes[self.root.0].kind, "scorer") {
            return Err("callable scorers read external values through context".into());
        }
        if let graph::ElementId::Node(node) = binding.owner
            && !self.order.contains(&node)
            && node != self.agent
        {
            let owners = self
                .graph
                .edges
                .iter()
                .filter(|edge| {
                    edge.target == node
                        && inherits(self.graph, &edge.kind, "contains")
                        && inherits(self.graph, &self.graph.nodes[edge.source.0].kind, "agent")
                })
                .map(|edge| edge.source)
                .collect::<Vec<_>>();
            if owners.len() != 1 {
                return Err("external value references need one owning agent".into());
            }
            return AgentLowering::new(self.graph, owners[0], self.result)?.reference(id);
        }
        let graph::ElementId::Node(mut node) = binding.owner else {
            return match binding.owner {
                graph::ElementId::Fork(_) if matches!(binding.path[0].as_str(), "items" | "count") => {
                    Ok(execution::Value::VarRef(format!("fork_input_{}", id.0)))
                }
                graph::ElementId::Edge(_) if binding.path == ["value"] => {
                    Ok(execution::Value::VarRef(format!("edge_value_{}", id.0)))
                }
                graph::ElementId::Edge(_) if binding.path == ["when"] => {
                    Ok(execution::Value::VarRef(format!("guard_{}", id.0)))
                }
                _ => self.binding_value(id),
            };
        };
        if node == self.agent {
            if binding.path != ["input"] {
                return Err("only agent input can be read by its steps".into());
            }
            node = self.root;
        }
        if !self.order.contains(&node) {
            return Err("value references must stay within the selected agent".into());
        }
        if binding.property {
            return self.value(&binding.value.clone());
        }
        if !inherits(self.graph, &self.graph.nodes[node.0].kind, "span") {
            return Ok(execution::Value::VarRef(format!("binding_{}", id.0)));
        }
        let Some(field) = Self::field(&binding.path[0]) else {
            return Ok(execution::Value::VarRef(format!("binding_{}", id.0)));
        };
        let mut nodes = vec![];
        let mut cursor = node;
        while cursor != self.root {
            nodes.push(cursor);
            cursor = self.parents[&cursor];
        }
        nodes.reverse();
        let reference = execution::ResolvedRef {
            module: Some(Self::id(self.root)),
            up: 0,
            steps: nodes
                .into_iter()
                .flat_map(|node| {
                    let mut steps = Vec::new();
                    if let Some((index, _)) = self.each(node) {
                        steps.push(execution::Step::Child {
                            candidates: vec![self.each_id(index)],
                            position: None,
                        });
                        steps.push(execution::Step::Iteration(execution::Value::CtxRef(
                            execution::CtxRef::RepeatIndexAt(self.each_id(index)),
                        )));
                    }
                    if !self.guards[&node].is_empty()
                        || self.junction(node).is_some()
                        || !self.weighted_branches(node).is_empty()
                    {
                        steps.push(execution::Step::Child {
                            candidates: vec![self.guard_id(node)],
                            position: None,
                        });
                    }
                    steps.push(execution::Step::Child {
                        candidates: vec![Self::id(node)],
                        position: None,
                    });
                    steps
                })
                .collect(),
            accessor: execution::Accessor::Field(field),
            path: binding
                .path
                .iter()
                .skip(1)
                .map(|key| execution::Selection::Index(execution::Value::Str(key.clone())))
                .collect(),
            range: binding.range,
        };
        let ref_id = execution::RefId(self.result.refs.len() as u32);
        self.result.refs.push(reference);
        Ok(execution::Value::BlockRef {
            ref_id,
            range: binding.range,
        })
    }

    fn flow_property(&mut self, id: graph::BindingId) -> Result<execution::Value, String> {
        let value = self.binding_value(id)?;
        if matches!(self.graph.bindings[id.0].owner, graph::ElementId::Junction(_)) {
            return Ok(value);
        }
        Ok(execution::Value::Func {
            func: execution::Func::FlowProperty {
                binding: id.0,
                value: Box::new(value),
            },
            range: self.graph.bindings[id.0].range,
        })
    }

    fn binding_value(&mut self, id: graph::BindingId) -> Result<execution::Value, String> {
        if self.building.contains(&id) {
            return Err("circular value propagation".into());
        }
        self.building.push(id);
        let value = self.value(&self.graph.bindings[id.0].value.clone());
        self.building.pop();
        value
    }

    fn value(&mut self, value: &graph::Value) -> Result<execution::Value, String> {
        use execution::Value as V;
        use graph::Value as G;
        Ok(match value {
            G::Input { binding, ty } if self.flow => V::Func {
                func: execution::Func::FlowInput {
                    binding: binding.0,
                    ty: *ty,
                },
                range: self.graph.bindings[binding.0].range,
            },
            G::Context { path, .. } if self.flow => V::Func {
                func: execution::Func::FlowContext(path.clone()),
                range: self.graph.nodes[self.root.0].range,
            },
            G::Input { binding, ty } => {
                let source = *self.inputs.get(binding).ok_or("upstream field has no feeds edge")?;
                let value = execution::Value::VarRef(format!("edge_value_{}", source.0));
                if *ty == graph::FieldType::Any {
                    value
                } else {
                    V::Func {
                        func: execution::Func::Check {
                            value: Box::new(value),
                            ty: *ty,
                        },
                        range: self.graph.bindings[binding.0].range,
                    }
                }
            }
            G::BindingRef(id, range) => {
                let child = &self.graph.bindings[id.0];
                let internal = self.building.last().is_some_and(|parent| {
                    let parent = &self.graph.bindings[parent.0];
                    parent.owner == child.owner
                        && parent.property == child.property
                        && child.path.len() > parent.path.len()
                        && child.path.starts_with(&parent.path)
                });
                if internal && !self.flow {
                    self.binding_value(*id)?
                } else {
                    let mut value = self.reference(*id)?;
                    if let V::BlockRef { ref_id, range: source } = &mut value {
                        *source = *range;
                        self.result.refs[ref_id.0 as usize].range = *range;
                    }
                    value
                }
            }
            G::Str(value) => V::Str(value.clone()),
            G::Num(value) => V::Num(value.clone()),
            G::Bool(value) => V::Bool(*value),
            G::Null => V::Null,
            G::Array(array) => V::Array(execution::Array {
                elem: array
                    .elem
                    .iter()
                    .map(|item| {
                        Ok(match item {
                            graph::ArrayElem::Item(value) => execution::ArrayElem::Item(self.value(value)?),
                            graph::ArrayElem::Spread(value) => execution::ArrayElem::Spread(self.value(value)?),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            }),
            G::Object(object) => V::Object(execution::Object {
                elem: object
                    .elem
                    .iter()
                    .map(|field| {
                        Ok(execution::ObjectField {
                            key: field.key.clone(),
                            value: self.value(&field.value)?,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            }),
            G::Template(template) => V::Template(execution::Template {
                parts: template
                    .parts
                    .iter()
                    .map(|part| {
                        Ok(match part {
                            graph::Part::Lit(text) => execution::Part::Lit(text.clone()),
                            graph::Part::BindingRef(id) => execution::Part::Dynamic(self.reference(*id)?),
                            graph::Part::Dynamic(value) => execution::Part::Dynamic(self.value(value)?),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            }),
            G::Attachment {
                path,
                filename,
                content_type,
                key,
            } => V::Attachment {
                path: path.clone(),
                filename: filename.clone(),
                content_type: content_type.clone(),
                key: key.clone(),
            },
            G::Unary { op, operand, range } => V::Unary {
                op: *op,
                operand: Box::new(self.value(operand)?),
                range: *range,
            },
            G::Binary { op, lhs, rhs, range } => V::Binary {
                op: *op,
                lhs: Box::new(self.value(lhs)?),
                rhs: Box::new(self.value(rhs)?),
                range: *range,
            },
            G::Cond { cond, then, otherwise } => V::Cond {
                cond: Box::new(self.value(cond)?),
                then: Box::new(self.value(then)?),
                otherwise: Box::new(self.value(otherwise)?),
            },
            G::Index { target, index, range } => V::Index {
                target: Box::new(self.value(target)?),
                index: Box::new(self.value(index)?),
                range: *range,
            },
            G::Slice {
                target,
                start,
                end,
                range,
            } => V::Slice {
                target: Box::new(self.value(target)?),
                start: start.as_ref().map(|v| self.value(v).map(Box::new)).transpose()?,
                end: end.as_ref().map(|v| self.value(v).map(Box::new)).transpose()?,
                range: *range,
            },
            G::Func { func, range } => V::Func {
                func: self.function(func)?,
                range: *range,
            },
            G::Context {
                owner: graph::ElementId::Node(node),
                path,
            } if path.first().is_some_and(|key| key == "history") => {
                let name = path.get(1).ok_or("history needs a node name")?;
                let target = self
                    .graph
                    .nodes
                    .iter()
                    .position(|node| node.name.as_ref() == Some(name))
                    .map(graph::NodeId)
                    .ok_or("history node does not exist")?;
                let (group, _) = self.each_ancestor(*node).ok_or("history requires a repetition group")?;
                if self.each_ancestor(target).map(|(i, _)| i) != Some(group) {
                    return Err("history must stay within the current repetition group".into());
                }
                let binding = self.graph.nodes[target.0]
                    .fields
                    .first()
                    .copied()
                    .ok_or("history target needs a payload field")?;
                let V::BlockRef { ref_id, range } = self.reference(binding)? else {
                    return Err("history target needs a payload field".into());
                };
                let reference = &mut self.result.refs[ref_id.0 as usize];
                reference.accessor = execution::Accessor::Record;
                reference.path.clear();
                let Some(step) = reference
                    .steps
                    .iter_mut()
                    .rev()
                    .find(|step| matches!(step, execution::Step::Iteration(_)))
                else {
                    return Err("history needs an occurrence reference".into());
                };
                *step = execution::Step::Iterations {
                    start: None,
                    end: Some(V::CtxRef(execution::CtxRef::RepeatIndex)),
                };
                let mut value = V::BlockRef { ref_id, range };
                for key in path.iter().skip(2) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range,
                    };
                }
                value
            }
            G::Context {
                owner: graph::ElementId::Node(node),
                path,
            } if path.first().is_some_and(|key| key == "arrivals") => {
                let mut value = self.arrivals(*node)?;
                for key in path.iter().skip(1) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range: self.graph.nodes[node.0].range,
                    };
                }
                value
            }
            G::Context {
                owner: graph::ElementId::Node(node),
                path,
            } if path
                .first()
                .is_some_and(|key| key == "item" || key == "index" || key == "count") =>
            {
                if self.callable {
                    return Ok(V::CtxRef(if path[0] == "index" {
                        execution::CtxRef::RepeatIndex
                    } else {
                        execution::CtxRef::RepeatCount
                    }));
                }
                let (_, fork) = self.each_ancestor(*node).ok_or("item and index require an each occurrence")?;
                let mut value = V::CtxRef(if path[0] == "count" {
                    execution::CtxRef::RepeatCount
                } else {
                    execution::CtxRef::RepeatIndex
                });
                if path[0] == "item" {
                    let items = *fork
                        .props
                        .iter()
                        .find(|id| self.graph.bindings[id.0].path == ["items"])
                        .ok_or("item requires an each fork")?;
                    value = V::Index {
                        target: Box::new(V::VarRef(format!("fork_input_{}", items.0))),
                        index: Box::new(value),
                        range: self.graph.nodes[node.0].range,
                    };
                }
                for key in path.iter().skip(1) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range: self.graph.nodes[node.0].range,
                    };
                }
                value
            }
            G::Context { owner, path } => {
                let Some(first) = path.first() else {
                    return Err("scorer context needs a field".into());
                };
                if self.callable && path == &["count"] {
                    return Ok(V::CtxRef(execution::CtxRef::RepeatCount));
                }
                if path == &["trace_index"] {
                    return Ok(V::CtxRef(execution::CtxRef::TraceIndex));
                }
                let arg = match first.as_str() {
                    "input" => execution::ScorerArg::Input,
                    "output" => execution::ScorerArg::Output,
                    "expected" => execution::ScorerArg::Expected,
                    "metadata" => execution::ScorerArg::Metadata,
                    _ => return Err(format!("unsupported mocking context field {first:?}")),
                };
                let mut value = if self.callable {
                    V::ArgRef(arg)
                } else {
                    let graph::ElementId::Node(node) = owner else {
                        return Err("scorer context requires a node".into());
                    };
                    if !inherits(self.graph, &self.graph.nodes[node.0].kind, "scorer") {
                        return Err("this context requires a scorer node".into());
                    }
                    let parent = *self.parents.get(node).ok_or("scorer span needs a parent")?;
                    match self.binding(parent, first) {
                        Some(id) => self.reference(id)?,
                        None => V::Null,
                    }
                };
                for key in path.iter().skip(1) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range: self.graph.nodes[self.root.0].range,
                    };
                }
                value
            }
            G::GraphRef { .. } | G::FieldRef(_) => {
                return Err("graph identities and context values are not supported by agent mocking yet".into());
            }
        })
    }

    fn function(&mut self, func: &graph::Func) -> Result<execution::Func, String> {
        Ok(match func {
            graph::Func::English { operation, args } => execution::Func::English {
                operation: *operation,
                args: args.iter().map(|v| self.value(v)).collect::<Result<_, String>>()?,
            },
            graph::Func::Attachment { path, content_type } => execution::Func::Attachment {
                path: path.clone(),
                content_type: content_type.clone(),
            },
            graph::Func::Choice(values) => {
                execution::Func::Choice(values.iter().map(|v| self.value(v)).collect::<Result<_, String>>()?)
            }
            graph::Func::Range(values) => execution::Func::Range(match values {
                graph::Range::Int { min, max } => execution::Range::Int { min: *min, max: *max },
                graph::Range::Float { min, max } => execution::Range::Float { min: *min, max: *max },
            }),
            graph::Func::Weighted(values) => execution::Func::Weighted(
                values
                    .iter()
                    .map(|v| {
                        Ok(execution::WeightedOption {
                            value: self.value(&v.value)?,
                            weight: v.weight,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            ),
            graph::Func::Normal { mean, stddev } => execution::Func::Normal {
                mean: *mean,
                stddev: *stddev,
            },
            graph::Func::Lognormal { median, sigma } => execution::Func::Lognormal {
                median: *median,
                sigma: *sigma,
            },
            graph::Func::Exponential { mean } => execution::Func::Exponential { mean: *mean },
            graph::Func::Pareto { min, shape } => execution::Func::Pareto {
                min: *min,
                shape: *shape,
            },
            graph::Func::Beta { alpha, beta } => execution::Func::Beta {
                alpha: *alpha,
                beta: *beta,
            },
            graph::Func::Poisson { mean } => execution::Func::Poisson { mean: *mean },
            graph::Func::Chance { probability } => execution::Func::Chance {
                probability: *probability,
            },
            graph::Func::Upper { text } => execution::Func::Upper {
                text: Box::new(self.value(text)?),
            },
            graph::Func::Lower { text } => execution::Func::Lower {
                text: Box::new(self.value(text)?),
            },
            graph::Func::Trim { text } => execution::Func::Trim {
                text: Box::new(self.value(text)?),
            },
            graph::Func::Replace { text, from, to } => execution::Func::Replace {
                text: Box::new(self.value(text)?),
                from: Box::new(self.value(from)?),
                to: Box::new(self.value(to)?),
            },
            graph::Func::Split { text, separator } => execution::Func::Split {
                text: Box::new(self.value(text)?),
                separator: Box::new(self.value(separator)?),
            },
            graph::Func::Join { array, separator } => execution::Func::Join {
                array: Box::new(self.value(array)?),
                separator: Box::new(self.value(separator)?),
            },
            graph::Func::Render { template, values } => execution::Func::Render {
                template: Box::new(self.value(template)?),
                values: Box::new(self.value(values)?),
            },
            graph::Func::Compose { array, separator } => execution::Func::Compose {
                array: Box::new(self.value(array)?),
                separator: Box::new(self.value(separator)?),
            },
            graph::Func::Sample { array, count } => execution::Func::Sample {
                array: Box::new(self.value(array)?),
                count: Box::new(self.value(count)?),
            },
            graph::Func::Contains { target, needle } => execution::Func::Contains {
                target: Box::new(self.value(target)?),
                needle: Box::new(self.value(needle)?),
            },
            graph::Func::StartsWith { text, prefix } => execution::Func::StartsWith {
                text: Box::new(self.value(text)?),
                prefix: Box::new(self.value(prefix)?),
            },
            graph::Func::EndsWith { text, suffix } => execution::Func::EndsWith {
                text: Box::new(self.value(text)?),
                suffix: Box::new(self.value(suffix)?),
            },
            graph::Func::Len { target } => execution::Func::Len {
                target: Box::new(self.value(target)?),
            },
            graph::Func::Tokens { value } => execution::Func::Tokens {
                value: Box::new(self.value(value)?),
            },
            graph::Func::Format { pieces, args } => execution::Func::Format {
                pieces: pieces.clone(),
                args: args.iter().map(|v| self.value(v)).collect::<Result<_, String>>()?,
            },
            graph::Func::Clamp { value, min, max } => execution::Func::Clamp {
                value: Box::new(self.value(value)?),
                min: Box::new(self.value(min)?),
                max: Box::new(self.value(max)?),
            },
            graph::Func::Round { value } => execution::Func::Round {
                value: Box::new(self.value(value)?),
            },
            graph::Func::Floor { value } => execution::Func::Floor {
                value: Box::new(self.value(value)?),
            },
            graph::Func::Ceil { value } => execution::Func::Ceil {
                value: Box::new(self.value(value)?),
            },
            graph::Func::Abs { value } => execution::Func::Abs {
                value: Box::new(self.value(value)?),
            },
            graph::Func::Min(values) => {
                execution::Func::Min(values.iter().map(|v| self.value(v)).collect::<Result<_, String>>()?)
            }
            graph::Func::Max(values) => {
                execution::Func::Max(values.iter().map(|v| self.value(v)).collect::<Result<_, String>>()?)
            }
            graph::Func::Uuid => execution::Func::Uuid,
            graph::Func::Hex { length } => execution::Func::Hex { length: *length },
            graph::Func::Alphanum { length } => execution::Func::Alphanum { length: *length },
            graph::Func::Noise { size } => execution::Func::Noise {
                size: Box::new(self.value(size)?),
            },
        })
    }
}
