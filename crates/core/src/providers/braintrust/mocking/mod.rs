//! Braintrust implementations of the core mocking capabilities.
//!
//! [`code`] supplies scorer registrations and codebase conventions for
//! [`crate::mocking::code`]. [`data`] supplies formatting and delivery for
//! [`crate::mocking::data`].

pub mod code;
pub mod data;

use crate::mocking as execution;
use bts_lang as graph;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Compiles source with the Braintrust definitions for the mocking consumers.
pub fn compile(source: &str) -> Result<execution::Model, String> {
    crate::providers::Provider::compile(&super::Bundle, source)
}

/// What Braintrust declares: its kinds, and the kinds and context keys mocking gives meaning to.
pub const SCHEMA: graph::ProviderSchema<'static> = graph::ProviderSchema {
    id: "braintrust",
    name: "Braintrust mocking",
    definitions: &[super::defs::SOURCE],
    // a span and the kinds derived from it emit spans, node and vars carry values without one
    nodes: &["span", "node", "vars"],
    // calls derives from next
    edges: &["next", "feeds"],
    forks: &["parallel", "conditional", "exclusive", "weighted", "each", "repeat"],
    junctions: &["accumulate", "merge", "first", "quorum", "reduce"],
    context: &[
        "trace_index",
        "history",
        "item",
        "index",
        "count",
        "arrivals",
        "accumulator",
        "arrival",
        "parent",
        "input",
        "output",
        "expected",
        "metadata",
    ],
    functions: super::functions::DESCRIPTORS,
};

/// The elements the mocking consumers walk.
///
/// They are the steps of each agent with their edges, forks and junctions, and
/// every scorer. Other elements of the model are left to other consumers.
pub fn elements(model: &graph::Model) -> Vec<graph::ElementId> {
    let mut elements = BTreeSet::new();
    for (index, node) in model.nodes.iter().enumerate() {
        let id = graph::NodeId(index);
        if model.derives(&node.kind, "scorer") {
            elements.insert(graph::ElementId::Node(id));
        }
        if !model.derives(&node.kind, "agent") {
            continue;
        }
        // a broken structure is reported by the lowering itself
        let Ok(Structure { members, .. }) = structure(model, id) else {
            continue;
        };
        let walked = |node: &graph::NodeId| *node == id || members.contains(node);
        elements.extend(members.iter().map(|member| graph::ElementId::Node(*member)));
        elements.extend(
            (model.edges.iter().enumerate())
                .filter(|(_, edge)| walked(&edge.source))
                .map(|(edge, _)| graph::ElementId::Edge(graph::EdgeId(edge))),
        );
        elements.extend(
            (model.forks.iter().enumerate())
                .filter(|(_, fork)| walked(&fork.source))
                .map(|(fork, _)| graph::ElementId::Fork(graph::ForkId(fork))),
        );
        elements.extend(
            (model.junctions.iter().enumerate())
                .filter(|(_, junction)| members.contains(&junction.target))
                .map(|(junction, _)| graph::ElementId::Junction(graph::JunctionId(junction))),
        );
    }
    elements.into_iter().collect()
}

/// Reports every element the mocking consumers would walk and give no meaning to.
pub fn unsupported(model: &graph::Model) -> graph::Diags {
    model.unsupported(&SCHEMA, &elements(model))
}

/// Lowers provider behavior graphs into shared mocking execution plans.
///
/// Membership is every step the entry leads to, and a calls edge makes its target a child span.
/// An agent lowers to one nested plan. A fork that iterates becomes a repeat of its rounds,
/// a loop-back becomes a repeat that runs again while its guard holds, and a junction target
/// runs once for each emission of its junction.
/// Both consumers enforce traversal limits, and a loop-back without one stops at 10000 traversals.
/// One trace has one entry span, so loop-back targets must be its descendants.
/// Authored error payloads are trace data, not failed runtime operations.
/// Expression errors abort generation. Mocking does not dispatch cancellable operations.
///
/// A failure is one validation diagnostic at the graph element it concerns, or at
/// the start of the source when no single element is responsible.
pub fn lower(model: &graph::Model) -> Result<execution::Model, graph::Diag> {
    if let Some(diag) = unsupported(model).into_iter().next() {
        return Err(diag);
    }
    lower_model(model).map_err(|error| graph::Diag {
        when: graph::DiagPhase::Validation,
        what: error.what,
        r#where: error.range.unwrap_or(graph::SrcRange::new(0, 0)),
    })
}

// a lowering failure, with the range of the graph element it concerns once one is known
struct Error {
    what: String,
    range: Option<graph::SrcRange>,
}

impl From<String> for Error {
    fn from(what: String) -> Self {
        Self { what, range: None }
    }
}

impl From<&str> for Error {
    fn from(what: &str) -> Self {
        what.to_owned().into()
    }
}

fn located(range: graph::SrcRange, what: impl Into<String>) -> Error {
    Error {
        what: what.into(),
        range: Some(range),
    }
}

trait At<T> {
    // names the element responsible for a failure unless a more specific one already has
    fn at(self, range: graph::SrcRange) -> Result<T, Error>;
}

impl<T, E: Into<Error>> At<T> for Result<T, E> {
    fn at(self, range: graph::SrcRange) -> Result<T, Error> {
        self.map_err(|error| {
            let mut error = error.into();
            error.range.get_or_insert(range);
            error
        })
    }
}

fn lower_model(model: &graph::Model) -> Result<execution::Model, Error> {
    let mut result = execution::Model {
        ordered: true,
        functions: super::functions::FUNCTIONS,
        ..Default::default()
    };
    for (index, node) in model.nodes.iter().enumerate() {
        if !model.derives(&node.kind, "agent") {
            continue;
        }
        let agent = graph::NodeId(index);
        let mut lowerer = AgentLowering::new(model, agent, &mut result).at(node.range)?;
        lowerer.lower().at(node.range)?;
    }
    for (index, node) in model.nodes.iter().enumerate() {
        if !model.derives(&node.kind, "scorer") {
            continue;
        }
        let id = graph::NodeId(index);
        let mut lowerer = AgentLowering::callable(model, id, &mut result);
        let kind = if let Some(judge) = lowerer.binding(id, "judge") {
            if lowerer.binding(id, "score").is_some() || lowerer.binding(id, "steps").is_some() {
                return Err(located(node.range, "a scorer must select expressions or a judge"));
            }
            let judge_range = model.bindings[judge.0].range;
            let execution::Value::Object(judge) = lowerer.binding_value(judge)? else {
                return Err(located(judge_range, "judge must be an object"));
            };
            let field = |key: &str| {
                judge
                    .elem
                    .iter()
                    .find(|field| field.key == key)
                    .map(|field| field.value.clone())
                    .ok_or_else(|| located(judge_range, format!("judge needs {key}")))
            };
            let execution::Value::Str(model) = field("model")? else {
                return Err(located(judge_range, "judge model must be a string"));
            };
            let prompt = field("prompt")?;
            let execution::Value::Object(options) = field("options")? else {
                return Err(located(judge_range, "judge options must be an object"));
            };
            let options = options
                .elem
                .into_iter()
                .map(|field| {
                    let score = match field.value {
                        execution::Value::Num(execution::Number::Int(n)) => n as f64,
                        execution::Value::Num(execution::Number::Float(n)) => n,
                        _ => return Err(located(judge_range, "judge scores must be constants")),
                    };
                    if !(0.0..=1.0).contains(&score) {
                        return Err(located(judge_range, "judge scores must be between zero and one"));
                    }
                    Ok(execution::JudgeOption { label: field.key, score })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            if options.is_empty() {
                return Err(located(judge_range, "judge needs at least one option"));
            }
            execution::ScorerKind::Judge { model, prompt, options }
        } else {
            let score = lowerer
                .binding(id, "score")
                .ok_or("scorer needs a score or judge field")
                .at(node.range)?;
            let score = lowerer.binding_value(score)?;
            let steps = lowerer.binding(id, "steps").map(|id| lowerer.binding_value(id)).transpose()?;
            execution::ScorerKind::Code {
                score,
                steps: scorer_steps(steps).at(node.range)?,
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
                        _ => return Err(located(binding.range, "scorer lang must be python or typescript")),
                    })
                }
                "file" => match &binding.value {
                    graph::Value::Str(value) => file = Some(value.clone()),
                    _ => return Err(located(binding.range, "scorer file must be a constant string")),
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

fn node_ref(graph: &graph::Model, value: &graph::Value, kind: Option<&str>) -> Result<graph::NodeId, Error> {
    let graph::Value::GraphRef { ref_id, .. } = resolved(graph, value) else {
        return Err("expected a node reference".into());
    };
    let graph::ElementId::Node(node) = graph.refs[ref_id.0].target else {
        return Err("expected a node reference".into());
    };
    if kind.is_some_and(|kind| !graph.derives(&graph.nodes[node.0].kind, kind)) {
        return Err(format!("expected a {} node", kind.unwrap()).into());
    }
    Ok(node)
}

fn node_refs(graph: &graph::Model, value: &graph::Value, kind: Option<&str>) -> Result<Vec<graph::NodeId>, Error> {
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

fn node_name(graph: &graph::Model, node: graph::NodeId) -> Result<String, Error> {
    for id in &graph.nodes[node.0].props {
        if graph.bindings[id.0].path == ["name"] {
            if let graph::Value::Str(name) = &graph.bindings[id.0].value {
                return Ok(name.clone());
            }
            return Err(located(graph.bindings[id.0].range, "resource names must be constant strings"));
        }
    }
    graph.nodes[node.0]
        .name
        .clone()
        .ok_or_else(|| located(graph.nodes[node.0].range, "provider resources must have names"))
}

fn span_ref(graph: &graph::Model, node: graph::NodeId, result: &mut execution::Model) -> Result<execution::RefId, Error> {
    if graph.derives(&graph.nodes[node.0].kind, "agent") {
        let name = node_name(graph, node)?;
        return result
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .map(|agent| agent.entrypoint)
            .ok_or_else(|| located(graph.nodes[node.0].range, "agent has no entrypoint"));
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
    Err(located(
        graph.nodes[node.0].range,
        "resource task must reference a statically reachable emitted span",
    ))
}

fn resource_value(
    graph: &graph::Model,
    node: graph::NodeId,
    value: &graph::Value,
    result: &mut execution::Model,
) -> Result<execution::Value, Error> {
    AgentLowering::callable(graph, node, result)
        .value(value)
        .at(graph.nodes[node.0].range)
}

fn lower_resources(graph: &graph::Model, result: &mut execution::Model) -> Result<(), Error> {
    for (index, node) in graph.nodes.iter().enumerate() {
        lower_resource(graph, graph::NodeId(index), result).at(node.range)?;
    }
    Ok(())
}

// failures that name no narrower element are located at the resource node by the caller
fn lower_resource(graph: &graph::Model, id: graph::NodeId, result: &mut execution::Model) -> Result<(), Error> {
    let node = &graph.nodes[id.0];
    if !["dataset", "experiment", "facet", "automation"]
        .iter()
        .any(|kind| graph.derives(&node.kind, kind))
    {
        return Ok(());
    }
    let name = node_name(graph, id)?;
    let required = |key| field(graph, id, key).ok_or_else(|| format!("{name} needs {key}"));
    let description = field(graph, id, "description")
        .map(|value| match resolved(graph, value) {
            graph::Value::Str(value) => Ok(value.clone()),
            _ => Err("description must be a constant string".to_owned()),
        })
        .transpose()?;
    let string = |key: &str| -> Result<Option<String>, Error> {
        field(graph, id, key)
            .map(|value| match resolved(graph, value) {
                graph::Value::Str(value) => Ok(value.clone()),
                _ => Err(format!("{name}.{key} must be a constant string").into()),
            })
            .transpose()
    };
    let boolean = |key: &str, default: bool| -> Result<bool, Error> {
        match field(graph, id, key).map(|value| resolved(graph, value)) {
            None => Ok(default),
            Some(graph::Value::Bool(value)) => Ok(*value),
            _ => Err(format!("{name}.{key} must be a constant boolean").into()),
        }
    };
    let strings = |key: &str| -> Result<Vec<String>, Error> {
        let Some(value) = field(graph, id, key) else {
            return Ok(vec![]);
        };
        let graph::Value::Array(array) = resolved(graph, value) else {
            return Err(format!("{name}.{key} must be an array").into());
        };
        array
            .elem
            .iter()
            .map(|item| match item {
                graph::ArrayElem::Item(value) => match resolved(graph, value) {
                    graph::Value::Str(value) => Ok(value.clone()),
                    _ => Err(format!("{name}.{key} must contain constant strings").into()),
                },
                _ => Err(format!("{name}.{key} must contain explicit entries").into()),
            })
            .collect()
    };
    if graph.derives(&node.kind, "facet") {
        result.facets.push(execution::Facet {
            prompt: string("prompt")?.ok_or_else(|| format!("{name} needs prompt"))?,
            no_match_pattern: string("no_match_pattern")?,
            name,
            description,
        });
    } else if graph.derives(&node.kind, "automation") {
        let kind = match string("type")?.as_deref() {
            Some("scorer") => {
                let scorers = strings("scorers")?;
                if scorers.is_empty() {
                    return Err(format!("{name} needs scorers").into());
                }
                if field(graph, id, "facets").is_some() {
                    return Err(format!("{name} cannot mix scorers and facets").into());
                }
                if string("scope")?.as_deref().is_some_and(|scope| scope != "span") {
                    return Err(format!("{name} scorer scope must be span").into());
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
                    return Err(format!("{name} needs facets").into());
                }
                if field(graph, id, "scorers").is_some()
                    || field(graph, id, "root").is_some()
                    || field(graph, id, "span_names").is_some()
                {
                    return Err(format!("{name} topics cannot configure scorer targeting").into());
                }
                if string("scope")?.as_deref().is_some_and(|scope| scope != "trace") {
                    return Err(format!("{name} topics scope must be trace").into());
                }
                execution::AutomationKind::Topics { facets }
            }
            _ => return Err(format!("{name} type must be scorer or topics").into()),
        };
        let sampling_rate = match field(graph, id, "sampling_rate").map(|v| resolved(graph, v)) {
            None => 1.0,
            Some(graph::Value::Num(graph::Number::Int(value))) => *value as f64,
            Some(graph::Value::Num(graph::Number::Float(value))) => *value,
            _ => return Err(format!("{name} sampling_rate must be a constant number").into()),
        };
        if !sampling_rate.is_finite() || !(0.0..=1.0).contains(&sampling_rate) {
            return Err(format!("{name} sampling_rate must be between zero and one").into());
        }
        let enabled = boolean("enabled", true)?;
        result.automations.push(execution::Automation {
            name,
            kind,
            sampling_rate,
            enabled,
        });
    } else if graph.derives(&node.kind, "dataset") {
        let mut cases = Vec::new();
        for case in node_refs(graph, required("cases")?, Some("case"))? {
            let source = if let Some(value) = field(graph, case, "input") {
                if field(graph, case, "source").is_some() || field(graph, case, "traces").is_some() {
                    return Err(located(graph.nodes[case.0].range, "case needs exactly one input source"));
                }
                execution::DatasetSource::Inline(resource_value(graph, case, value, result)?)
            } else if let Some(value) = field(graph, case, "source") {
                if field(graph, case, "traces").is_some() {
                    return Err(located(graph.nodes[case.0].range, "case needs exactly one input source"));
                }
                let target = node_ref(graph, value, None).at(graph.nodes[case.0].range)?;
                let reference = span_ref(graph, target, result)?;
                if result.refs[reference.0 as usize].steps.is_empty() {
                    execution::DatasetSource::Trace(reference)
                } else {
                    execution::DatasetSource::Span(reference)
                }
            } else if let Some(value) = field(graph, case, "traces") {
                let refs = node_refs(graph, value, None)
                    .at(graph.nodes[case.0].range)?
                    .into_iter()
                    .map(|node| span_ref(graph, node, result))
                    .collect::<Result<_, _>>()?;
                execution::DatasetSource::Group(refs)
            } else {
                return Err(located(graph.nodes[case.0].range, "case needs input, source or traces"));
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
    Ok(())
}

fn scorer_steps(value: Option<execution::Value>) -> Result<Vec<execution::ScorerStep>, Error> {
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

/// An agent's members and the parent of each member below the entry.
struct Structure {
    members: HashSet<graph::NodeId>,
    parents: HashMap<graph::NodeId, graph::NodeId>,
}

// membership and parenting are derived from the edges the entry leads to
fn structure(graph: &graph::Model, agent: graph::NodeId) -> Result<Structure, Error> {
    let named = |node: graph::NodeId| match &graph.nodes[node.0].name {
        Some(name) => format!("`{name}`"),
        None => "this step".to_owned(),
    };
    for edge in graph.edges.iter().filter(|edge| edge.source == agent) {
        if edge.primitive == graph::Primitive::Contains {
            return Err(located(
                edge.range,
                "agent membership comes from the entry edge, remove this `contains` edge",
            ));
        }
        if graph.derives(&edge.kind, "calls") {
            return Err(located(
                edge.range,
                "an agent emits no span, enter its first step with a `next` edge",
            ));
        }
    }
    let entries: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.source == agent && edge.primitive == graph::Primitive::Next)
        .map(|edge| edge.target)
        .collect();
    // members are the nodes the entry leads to along next edges and through junctions
    let mut members = HashSet::new();
    let mut pending = entries.clone();
    while let Some(node) = pending.pop() {
        if !members.insert(node) {
            continue;
        }
        pending.extend(
            (graph.edges.iter())
                .filter(|edge| edge.source == node && edge.primitive == graph::Primitive::Next)
                .map(|edge| edge.target),
        );
        pending.extend(
            (graph.junctions.iter())
                .filter(|junction| junction.inputs.iter().any(|input| graph.edges[input.0].source == node))
                .map(|junction| junction.target),
        );
    }
    // a member's parent is the source of the calls edge that supplies its parent key
    let mut parents = HashMap::new();
    for index in 0..graph.nodes.len() {
        let node = graph::NodeId(index);
        // an edge back to the entry is a cycle, which each lowering reports itself
        if !members.contains(&node) || entries.contains(&node) {
            continue;
        }
        let sources: BTreeSet<_> = (graph.context.get(index).into_iter().flatten())
            .filter(|supply| supply.key == "parent")
            .flat_map(|supply| &supply.suppliers)
            .filter_map(|supplier| match supplier {
                graph::ElementId::Edge(edge) => Some(graph.edges[edge.0].source),
                _ => None,
            })
            .filter(|source| members.contains(source))
            .collect();
        let sources: Vec<_> = sources.into_iter().collect();
        match sources.as_slice() {
            [] => {}
            [parent] => {
                parents.insert(node, *parent);
            }
            [first, second, ..] => {
                if let Some(junction) = graph.junctions.iter().find(|junction| junction.target == node) {
                    let inputs: Option<Vec<_>> = (junction.inputs.iter())
                        .map(|input| graph.edges[input.0].name.as_ref().map(|name| format!("`{name}`")))
                        .collect();
                    let inputs = match inputs {
                        Some(names) if names.len() > 1 => {
                            format!(" {} and {}", names[..names.len() - 1].join(", "), names[names.len() - 1])
                        }
                        _ => String::new(),
                    };
                    return Err(located(
                        junction.range,
                        format!("junction inputs{inputs} give {} different parent spans", named(node)),
                    ));
                }
                return Err(located(
                    graph.nodes[index].range,
                    format!(
                        "{} is reached under different parent spans, {} and {}",
                        named(node),
                        named(*first),
                        named(*second)
                    ),
                ));
            }
        }
    }
    // without a calls edge out of the entry, its first step would be a second root
    if let [entry] = entries.as_slice() {
        let missing = |node: &graph::NodeId| node != entry && !parents.contains_key(node);
        if let Some(edge) = (graph.edges.iter())
            .find(|edge| edge.source == *entry && edge.primitive == graph::Primitive::Next && missing(&edge.target))
        {
            return Err(located(
                edge.range,
                format!(
                    "{} has no parent span, go down from {} with a `calls` edge",
                    named(edge.target),
                    named(*entry)
                ),
            ));
        }
        if let Some(node) = (0..graph.nodes.len())
            .map(graph::NodeId)
            .find(|node| members.contains(node) && missing(node))
        {
            return Err(located(
                graph.nodes[node.0].range,
                format!("{} has no parent span", named(node)),
            ));
        }
    }
    Ok(Structure { members, parents })
}

// steps that run more than once inside their parent span
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Group {
    // the rounds of an each or repeat fork
    Round(usize),
    // the rounds of a loop-back, named by the step it returns to
    Loop(graph::NodeId),
    // the emissions of a junction, named by its index
    Emit(usize),
}

// where a scoped value is evaluated: once for a span, or once for each iteration of a group
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Span(graph::NodeId),
    Group(Group),
}

// how a reference reads a group other than at the reader's own iteration
#[derive(Clone, Copy)]
enum Across {
    // every iteration
    All,
    // the iterations before the reader's
    Before,
    // the iteration before the reader's
    Previous,
}

// where a route through the plan ends
#[derive(Clone, Copy)]
enum End {
    Span,
    Guard,
    Iteration(Group),
}

// a child of a span with the groups it runs in on that level, widest first
type Member = (Vec<Group>, execution::Child);

// a loop-back edge and the step it leaves
struct LoopBack {
    edge: graph::EdgeId,
    source: graph::NodeId,
}

struct AgentLowering<'a> {
    graph: &'a graph::Model,
    agent: graph::NodeId,
    root: graph::NodeId,
    order: Vec<graph::NodeId>,
    guards: HashMap<graph::NodeId, Vec<graph::BindingId>>,
    parents: HashMap<graph::NodeId, graph::NodeId>,
    inputs: HashMap<graph::BindingId, graph::BindingId>,
    // values fed along a loop-back, by the field they supply
    carried: HashMap<graph::BindingId, graph::BindingId>,
    // loop-backs by the step they return to
    loops: HashMap<graph::NodeId, LoopBack>,
    // the positions in `order` of a step and everything it leads to
    ranges: HashMap<graph::NodeId, (usize, usize)>,
    building: Vec<graph::BindingId>,
    across: Vec<(Group, Across)>,
    // the groups a value is read in, when that is not where the value being built is declared
    readers: Vec<Vec<Group>>,
    // whether a fold hands its combine expression bare values, not arrival records
    raw_fold: bool,
    callable: bool,
    result: &'a mut execution::Model,
}

// the path a supplied context value is lowered under, which no author can write
const CONTEXT_VALUE: &str = "@context";

// how a junction primitive combines what arrives, the capability check admits no other junction
fn emission_kind(primitive: graph::Primitive) -> execution::EmissionKind {
    match primitive {
        graph::Primitive::Merge => execution::EmissionKind::Merge,
        graph::Primitive::First => execution::EmissionKind::First,
        graph::Primitive::Quorum => execution::EmissionKind::Quorum,
        graph::Primitive::Reduce => execution::EmissionKind::Reduce,
        _ => execution::EmissionKind::Accumulate,
    }
}

impl<'a> AgentLowering<'a> {
    // lowers the values of one node that is not walked as an agent
    fn callable(graph: &'a graph::Model, node: graph::NodeId, result: &'a mut execution::Model) -> Self {
        Self {
            graph,
            agent: node,
            root: node,
            order: vec![node],
            guards: HashMap::new(),
            parents: HashMap::new(),
            inputs: HashMap::new(),
            carried: HashMap::new(),
            loops: HashMap::new(),
            ranges: HashMap::new(),
            building: vec![],
            across: vec![],
            readers: vec![],
            raw_fold: false,
            callable: true,
            result,
        }
    }

    fn new(graph: &'a graph::Model, agent: graph::NodeId, result: &'a mut execution::Model) -> Result<Self, Error> {
        let Structure { members, parents } = structure(graph, agent)?;
        if members.is_empty() {
            return Err("agent needs one next entry edge".into());
        }
        let iterates =
            |fork: &graph::Fork| fork.primitive == graph::Primitive::Each || fork.primitive == graph::Primitive::Repeat;
        for fork in &graph.forks {
            if fork.source != agent && !members.contains(&fork.source) {
                continue;
            }
            if fork.source == agent && fork.primitive == graph::Primitive::Weighted {
                return Err(located(
                    fork.range,
                    "agent entry must be unconditional, not a weighted selection",
                ));
            }
            if !iterates(fork) && !fork.props.is_empty() {
                return Err(located(
                    fork.range,
                    format!(
                        "fork kind {:?} or its configuration is not supported by mocking yet",
                        fork.kind
                    ),
                ));
            }
        }
        let weighted = |edge: &graph::Edge| {
            edge.fork
                .is_some_and(|fork| graph.forks[fork.0].primitive == graph::Primitive::Weighted)
        };
        let mut next: HashMap<graph::NodeId, Vec<(graph::NodeId, Vec<graph::BindingId>)>> = HashMap::new();
        let mut inputs = HashMap::new();
        let mut carried = HashMap::new();
        let mut loops: HashMap<graph::NodeId, LoopBack> = HashMap::new();
        for (index, edge) in graph.edges.iter().enumerate() {
            if edge.source != agent && !members.contains(&edge.source) {
                continue;
            }
            if edge.direction != graph::Direction::Directed {
                return Err(located(edge.range, "mocking requires directed paths"));
            }
            if !members.contains(&edge.target) {
                return Err(located(edge.range, "agent edges must target a member of the same agent"));
            }
            if edge.primitive == graph::Primitive::Next {
                let mut guards = Vec::new();
                for id in &edge.props {
                    match graph.bindings[id.0].path[0].as_str() {
                        "when" => guards.push(*id),
                        "max_traversals" if edge.cycle => {}
                        "weight" if weighted(edge) => {}
                        _ => {
                            return Err(located(
                                graph.bindings[id.0].range,
                                "configured next edges are not supported by mocking yet",
                            ));
                        }
                    }
                }
                if edge.cycle {
                    // a loop-back is not walked, it repeats the steps its target leads to
                    if edge.fork.is_some_and(|fork| iterates(&graph.forks[fork.0])) {
                        return Err(located(
                            edge.range,
                            "a loop-back cannot be a branch of an each or repeat fork",
                        ));
                    }
                    let back = LoopBack {
                        edge: graph::EdgeId(index),
                        source: edge.source,
                    };
                    if loops.insert(edge.target, back).is_some() {
                        return Err(located(
                            edge.range,
                            "more than one loop-back to one step is not supported by mocking yet",
                        ));
                    }
                    continue;
                }
                let targets = next.entry(edge.source).or_default();
                if targets.iter().any(|(target, _)| *target == edge.target) {
                    return Err(located(
                        edge.range,
                        "multiple next edges to one target require occurrence policies",
                    ));
                }
                targets.push((edge.target, guards));
            } else if edge.primitive == graph::Primitive::Feeds {
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
                        // a loop-back carries its value whenever it is traversed
                        Some("when") if edge.cycle => {}
                        Some("weight" | "when") if weighted(edge) => {}
                        _ => {
                            return Err(located(
                                binding.range,
                                "configured feeds edges are not supported by mocking yet",
                            ));
                        }
                    }
                }
                let value = value.ok_or("feeds needs a value").at(edge.range)?;
                let Some(into) = into else { continue };
                if graph.bindings[into.0].owner != graph::ElementId::Node(edge.target) {
                    return Err(located(edge.range, "feeds destination must belong to its target"));
                }
                if !matches!(graph.bindings[into.0].value, graph::Value::Input { .. }) {
                    return Err(located(
                        edge.range,
                        "feeds destination must declare an upstream value with <-",
                    ));
                }
                let supplied = if edge.cycle { &mut carried } else { &mut inputs };
                if supplied.insert(into, value).is_some() {
                    return Err(located(
                        edge.range,
                        "multiple feeds to one field require an accumulation policy",
                    ));
                }
            } else {
                unreachable!("the capability check admits next and feeds edges only");
            }
        }
        let mut joined = HashSet::new();
        for junction in &graph.junctions {
            if !members.contains(&junction.target) {
                continue;
            }
            if !joined.insert(junction.target) {
                return Err(located(
                    junction.range,
                    "multiple junction groups to one target require occurrence policies",
                ));
            }
            // a junction without a fork joins the edges of one step
            let source = match junction.fork {
                Some(fork) => graph.forks[fork.0].source,
                None => {
                    let mut sources = junction.inputs.iter().map(|input| graph.edges[input.0].source);
                    let first = sources.next().ok_or("a junction needs an input").at(junction.range)?;
                    if sources.any(|source| source != first) {
                        return Err(located(
                            junction.range,
                            "a junction without a fork must join edges that leave one step",
                        ));
                    }
                    first
                }
            };
            if !members.contains(&source) {
                return Err(located(junction.range, "junction group must belong to the same agent"));
            }
            for id in &junction.props {
                let binding = &graph.bindings[id.0];
                let text =
                    |allowed: &[&str]| matches!(&binding.value, graph::Value::Str(value) if allowed.contains(&value.as_str()));
                let supported = match binding.path[0].as_str() {
                    "observe" => text(&["arrival", "result"]),
                    "correlate" => true,
                    "count" => junction.primitive == graph::Primitive::Quorum,
                    "initial" | "combine" => junction.primitive == graph::Primitive::Reduce,
                    "order" => junction.primitive == graph::Primitive::Reduce && text(&["source", "arrival"]),
                    _ => false,
                };
                if !supported {
                    return Err(located(
                        binding.range,
                        format!("junction property `{}` is not supported by mocking yet", binding.path[0]),
                    ));
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
        let mut ranges = HashMap::new();
        fn walk(
            graph: &graph::Model,
            node: graph::NodeId,
            inherited: Vec<graph::BindingId>,
            next: &HashMap<graph::NodeId, Vec<(graph::NodeId, Vec<graph::BindingId>)>>,
            order: &mut Vec<graph::NodeId>,
            guards: &mut HashMap<graph::NodeId, Vec<graph::BindingId>>,
            ranges: &mut HashMap<graph::NodeId, (usize, usize)>,
        ) -> Result<(), Error> {
            if guards.insert(node, inherited.clone()).is_some() {
                return Err(located(
                    graph.nodes[node.0].range,
                    "repeated arrivals require a junction or an occurrence policy",
                ));
            }
            let start = order.len();
            order.push(node);
            for (target, local) in next.get(&node).into_iter().flatten() {
                let mut combined = inherited.clone();
                combined.extend(local);
                walk(graph, *target, combined, next, order, guards, ranges)?;
            }
            ranges.insert(node, (start, order.len()));
            Ok(())
        }
        walk(graph, root, vec![], &next, &mut order, &mut guards, &mut ranges)?;
        for edge in &graph.edges {
            if members.contains(&edge.source) && edge.primitive == graph::Primitive::Feeds && !edge.cycle {
                let source = order.iter().position(|node| *node == edge.source);
                let target = order.iter().position(|node| *node == edge.target);
                if source >= target {
                    return Err(located(
                        edge.range,
                        "feeds must follow progression order for straight-line mocking",
                    ));
                }
            }
        }
        for node in order.iter().skip(1) {
            if !parents.contains_key(node) {
                return Err(located(
                    graph.nodes[node.0].range,
                    "each step after the entry needs a parent span",
                ));
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
        // a loop-back repeats its target and every step the target leads to
        for (target, back) in &loops {
            let edge = &graph.edges[back.edge.0];
            if *target == root {
                return Err(located(edge.range, "cycle edges cannot target the trace entry span"));
            }
            let (start, end) = ranges[target];
            if !order[start..end].contains(&back.source) {
                return Err(located(
                    edge.range,
                    "a loop-back must return to a step that leads to its source",
                ));
            }
            let level = parents[target];
            for node in &order[start..end] {
                let mut cursor = *node;
                while parents.get(&cursor) != Some(&level) {
                    cursor = *parents.get(&cursor).ok_or_else(|| {
                        located(
                            graph.nodes[node.0].range,
                            "a step repeated by a loop-back must stay under the span that holds the loop",
                        )
                    })?;
                }
            }
        }
        for (into, value) in &carried {
            let graph::ElementId::Node(target) = graph.bindings[into.0].owner else {
                continue;
            };
            if !loops.contains_key(&target) {
                return Err(located(
                    graph.bindings[value.0].range,
                    "a feeds loop-back needs a next loop-back to the same step",
                ));
            }
            if !inputs.contains_key(into) {
                return Err(located(
                    graph.bindings[into.0].range,
                    "the first round of a loop needs a feeds edge for this field",
                ));
            }
        }
        for (into, _) in inputs.iter().filter(|(into, _)| !carried.contains_key(into)) {
            if let graph::ElementId::Node(target) = graph.bindings[into.0].owner
                && let Some(back) = loops.get(&target)
            {
                return Err(located(
                    graph.edges[back.edge.0].range,
                    format!(
                        "this loop-back must feed `{}`, a new round does not keep the earlier value",
                        graph.bindings[into.0].path.join(".")
                    ),
                ));
            }
        }
        Ok(Self {
            graph,
            agent,
            root,
            order,
            guards,
            parents,
            inputs,
            carried,
            loops,
            ranges,
            building: vec![],
            across: vec![],
            readers: vec![],
            raw_fold: false,
            callable: false,
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

    fn lower(&mut self) -> Result<(), Error> {
        let agent = &self.graph.nodes[self.agent.0];
        let name = node_name(self.graph, self.agent)?;
        let root_kind = &self.graph.nodes[self.root.0].kind;
        if self.result.traces.iter().any(|trace| trace.node == Self::id(self.root)) {
            return Err(located(
                self.graph.nodes[self.root.0].range,
                "shared agent entry spans are not supported by mocking yet",
            ));
        }
        if !self.graph.derives(root_kind, "task") && root_kind != "span" {
            return Err(located(
                self.graph.nodes[self.root.0].range,
                "the first mocking span must be a task or span",
            ));
        }
        let agent_input = self.binding(self.agent, "input").ok_or("agent needs an input field")?;
        let root_input = self
            .binding(self.root, "input")
            .ok_or("entry span needs an input field")
            .at(self.graph.nodes[self.root.0].range)?;
        if !matches!(self.graph.bindings[root_input.0].value, graph::Value::BindingRef(id, _) if id == agent_input) {
            return Err(located(
                self.graph.bindings[root_input.0].range,
                "entry span input must reference the agent input for mocking",
            ));
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
                && self.enclosing(node).is_none()
                && !binding.property
                && !matches!(resolved(self.graph, &binding.value), graph::Value::GraphRef { .. })
                && (!self.graph.derives(&self.graph.nodes[node.0].kind, "span")
                    || Self::field(binding.path.first().map(String::as_str).unwrap_or("")).is_none())
            {
                bindings.push(execution::Binding {
                    name: format!("binding_{index}"),
                    label: None,
                    value: self.binding_value(graph::BindingId(index))?,
                });
            }
        }
        for edge in &self.graph.edges {
            if !self.order.contains(&edge.source) || self.enclosing(edge.source).is_some() {
                continue;
            }
            for id in &edge.props {
                if self.graph.bindings[id.0].path == ["value"] {
                    bindings.push(execution::Binding {
                        name: format!("edge_value_{}", id.0),
                        label: None,
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
                    .filter(|fork| self.order.contains(&fork.source) && fork.primitive == graph::Primitive::Weighted)
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
                && self.enclosing(self.graph.edges[edge.0].source).is_some()
            {
                continue;
            }
            bindings.push(execution::Binding {
                name: format!("guard_{}", id.0),
                label: None,
                value: self.binding_value(id)?,
            });
        }
        let mut trace = execution::Trace {
            node: Self::id(self.root),
            name: self.name(self.root)?,
            fields,
            bindings,
            children,
        };
        for binding in &mut trace.bindings {
            binding.label = self.label(&binding.name);
        }
        self.label_children(&mut trace.children);
        self.result.traces.push(trace);
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

    fn label_children(&self, children: &mut [execution::Child]) {
        for child in children {
            let (bindings, children) = match child {
                execution::Child::Span(span) => (&mut span.bindings, Some(&mut span.children)),
                execution::Child::Repeat(repeat) => (&mut repeat.bindings, Some(&mut repeat.children)),
                execution::Child::Choice(choice) => (&mut choice.bindings, Some(&mut choice.children)),
                execution::Child::Maybe(maybe) => (&mut maybe.bindings, Some(&mut maybe.children)),
                execution::Child::Scorer(scorer) => (&mut scorer.bindings, None),
            };
            for binding in bindings {
                binding.label = self.label(&binding.name);
            }
            if let Some(children) = children {
                self.label_children(children);
            }
        }
    }

    // a readable name for a scoped value, taken from the graph element it belongs to
    fn label(&self, name: &str) -> Option<String> {
        let (role, index) = name.rsplit_once('_')?;
        let index: usize = index.parse().ok()?;
        let node = |id: graph::NodeId| {
            let node = &self.graph.nodes[id.0];
            node.name.clone().unwrap_or_else(|| node.kind.clone())
        };
        match role {
            "binding" | "guard" | "edge_value" | "fork_input" => {
                let binding = self.graph.bindings.get(index)?;
                match (role, binding.owner) {
                    // a supplied context value is known by its key
                    ("binding", _) if binding.path.first().is_some_and(|key| key == CONTEXT_VALUE) => {
                        Some(binding.path[1..].join("_"))
                    }
                    // a value a step keeps for itself is known by its own name
                    ("binding", graph::ElementId::Node(owner)) => {
                        let own: Vec<_> = (binding.path.iter()).filter(|key| *key != "local").cloned().collect();
                        Some(if own.is_empty() {
                            format!("{}_local", node(owner))
                        } else {
                            own.join("_")
                        })
                    }
                    ("guard", graph::ElementId::Edge(edge)) => {
                        let edge = &self.graph.edges[edge.0];
                        Some(edge.name.clone().unwrap_or_else(|| format!("runs_{}", node(edge.target))))
                    }
                    ("edge_value", graph::ElementId::Edge(edge)) => {
                        let edge = &self.graph.edges[edge.0];
                        let into = (edge.props.iter()).find_map(|id| match self.graph.bindings[id.0].value {
                            graph::Value::FieldRef(field) if self.graph.bindings[id.0].path == ["into"] => {
                                Some(self.graph.bindings[field.0].path.join("_"))
                            }
                            _ => None,
                        });
                        Some(match (into, &edge.name) {
                            // what a loop-back sends is the value for the round after this one
                            (Some(into), _) if edge.cycle => format!("next_{}_{into}", node(edge.target)),
                            (Some(into), _) => format!("{}_{into}", node(edge.target)),
                            (None, Some(name)) => format!("{name}_value"),
                            (None, None) => format!("{}_value", node(edge.target)),
                        })
                    }
                    ("fork_input", graph::ElementId::Fork(fork)) => {
                        let fork = &self.graph.forks[fork.0];
                        let stem = fork.name.clone().unwrap_or_else(|| node(fork.source));
                        Some(format!("{stem}_{}", binding.path[0]))
                    }
                    _ => None,
                }
            }
            "fork_pick" => {
                let fork = self.graph.forks.get(index)?;
                Some(format!("{}_route", fork.name.clone().unwrap_or_else(|| node(fork.source))))
            }
            "emissions" => Some(format!("{}_arrivals", node(self.graph.junctions.get(index)?.target))),
            "correlate" => Some(format!("{}_key", node(self.graph.junctions.get(index)?.target))),
            _ => None,
        }
    }

    fn name(&self, node: graph::NodeId) -> Result<String, Error> {
        let n = &self.graph.nodes[node.0];
        for id in &n.props {
            let binding = &self.graph.bindings[id.0];
            if binding.path == ["name"] {
                if let graph::Value::Str(name) = &binding.value {
                    return Ok(name.clone());
                }
                return Err(located(binding.range, "mocking span names must be constant strings"));
            }
        }
        Ok(n.name.clone().unwrap_or_else(|| format!("{}_{}", n.kind, node.0)))
    }

    fn iterates(&self, fork: &graph::Fork) -> bool {
        fork.primitive == graph::Primitive::Each || fork.primitive == graph::Primitive::Repeat
    }

    // the each or repeat fork whose round a step runs in
    fn round(&self, node: graph::NodeId, seen: &mut HashSet<graph::NodeId>) -> Option<(usize, &graph::Fork)> {
        if !seen.insert(node) {
            return None;
        }
        let direct = self.graph.forks.iter().enumerate().find(|(_, fork)| {
            self.iterates(fork) && fork.branches.iter().flatten().any(|id| self.graph.edges[id.0].target == node)
        });
        if direct.is_some() {
            return direct;
        }
        // a step that follows a round's step on the same level belongs to that round
        let level = self.parents.get(&node)?;
        for edge in &self.graph.edges {
            if edge.target == node
                && !edge.cycle
                && edge.primitive == graph::Primitive::Next
                && self.parents.get(&edge.source) == Some(level)
                && let Some(found) = self.round(edge.source, seen)
            {
                return Some(found);
            }
        }
        // so does the target of a junction on a fork inside the round
        for junction in self.graph.junctions.iter().filter(|junction| junction.target == node) {
            let Some(fork) = junction.fork else { continue };
            let fork = &self.graph.forks[fork.0];
            if !self.iterates(fork)
                && self.parents.get(&fork.source) == Some(level)
                && let Some(found) = self.round(fork.source, seen)
            {
                return Some(found);
            }
        }
        None
    }

    fn group_id(&self, group: Group) -> execution::NodeId {
        let nodes = self.graph.nodes.len();
        let forks = self.graph.forks.len();
        execution::NodeId(match group {
            Group::Round(index) => nodes * 2 + index,
            Group::Loop(target) => nodes * 2 + forks + target.0,
            Group::Emit(junction) => nodes * 3 + forks + junction,
        } as u32)
    }

    fn junction_index(&self, node: graph::NodeId) -> Option<usize> {
        self.graph.junctions.iter().position(|junction| junction.target == node)
    }

    fn junction_kind(&self, junction: &graph::Junction) -> execution::EmissionKind {
        emission_kind(junction.primitive)
    }

    fn junction_prop(&self, junction: &graph::Junction, name: &str) -> Option<graph::BindingId> {
        (junction.props.iter().copied()).find(|id| self.graph.bindings[id.0].path == [name])
    }

    // the rounds a junction gathers arrivals from, when its fork iterates
    fn junction_round(&self, junction: &graph::Junction) -> Option<Group> {
        let fork = junction.fork?;
        self.iterates(&self.graph.forks[fork.0]).then_some(Group::Round(fork.0))
    }

    // whether a junction can fire more than once, so its target runs once for each emission
    fn emits(&self, index: usize) -> bool {
        use execution::EmissionKind as Kind;
        let junction = &self.graph.junctions[index];
        let kind = self.junction_kind(junction);
        kind == Kind::Merge
            || (self.junction_round(junction).is_some()
                && (kind != Kind::Reduce || self.junction_prop(junction, "correlate").is_some()))
    }

    // the groups a step runs in on its own level, widest first
    fn level_groups(&self, node: graph::NodeId) -> Vec<Group> {
        let mut groups = Vec::new();
        if let Some((index, _)) = self.round(node, &mut HashSet::new()) {
            groups.push(Group::Round(index));
        }
        let Some(position) = self.order.iter().position(|member| *member == node) else {
            return groups;
        };
        let level = self.parents.get(&node);
        let holds = |target: graph::NodeId| {
            self.parents.get(&target) == level
                && (self.ranges.get(&target)).is_some_and(|(start, end)| (*start..*end).contains(&position))
        };
        let mut inner = Vec::new();
        for (index, junction) in self.graph.junctions.iter().enumerate() {
            if self.order.contains(&junction.target) && self.emits(index) && holds(junction.target) {
                inner.push((self.ranges[&junction.target].0, 0, Group::Emit(index)));
            }
        }
        for target in self.loops.keys() {
            if holds(*target) {
                inner.push((self.ranges[target].0, 1, Group::Loop(*target)));
            }
        }
        inner.sort_by_key(|(start, kind, _)| (*start, *kind));
        groups.extend(inner.into_iter().map(|(_, _, group)| group));
        groups
    }

    // the innermost group a step runs in, with the step on the group's level that holds it
    fn enclosing(&self, mut node: graph::NodeId) -> Option<(Group, graph::NodeId)> {
        loop {
            if let Some(group) = self.level_groups(node).pop() {
                return Some((group, node));
            }
            node = *self.parents.get(&node)?;
        }
    }

    fn within(&self, node: graph::NodeId, group: Group) -> bool {
        self.order.contains(&node) && self.enclosing(node).is_some_and(|(found, _)| found == group)
    }

    // every group a step runs in, on its own level and above
    fn all_groups(&self, mut node: graph::NodeId) -> Vec<Group> {
        let mut groups = Vec::new();
        loop {
            groups.extend(self.level_groups(node));
            match self.parents.get(&node) {
                Some(parent) => node = *parent,
                None => return groups,
            }
        }
    }

    // the groups the value being built is read in
    fn reader(&self) -> Vec<Group> {
        if let Some(groups) = self.readers.last() {
            return groups.clone();
        }
        let Some(first) = self.building.first() else { return vec![] };
        let node = match self.graph.bindings[first.0].owner {
            graph::ElementId::Node(node) => node,
            graph::ElementId::Edge(edge) => self.graph.edges[edge.0].source,
            graph::ElementId::Fork(fork) => self.graph.forks[fork.0].source,
            graph::ElementId::Junction(junction) => self.graph.junctions[junction.0].target,
        };
        self.all_groups(node)
    }

    fn each_ancestor(&self, mut node: graph::NodeId) -> Option<(usize, &graph::Fork)> {
        loop {
            if let Some(fork) = self.round(node, &mut HashSet::new()) {
                return Some(fork);
            }
            node = *self.parents.get(&node)?;
        }
    }

    // the number of rounds of an each or repeat fork, read where the fork starts
    fn round_count(&self, index: usize) -> Result<(execution::Value, graph::SrcRange), Error> {
        let fork = &self.graph.forks[index];
        let repeated = fork.primitive == graph::Primitive::Repeat;
        let property = if repeated { "count" } else { "items" };
        let input = *fork
            .props
            .iter()
            .find(|id| self.graph.bindings[id.0].path == [property])
            .ok_or("iteration fork needs its input")
            .at(fork.range)?;
        let range = self.graph.bindings[input.0].range;
        let value = execution::Value::VarRef(format!("fork_input_{}", input.0));
        let count = if repeated {
            value
        } else {
            execution::Value::Func {
                func: execution::Func::Len { target: Box::new(value) },
                range,
            }
        };
        Ok((count, range))
    }

    // the item, index or count of a round, read from a step inside it
    fn round_value(&self, fork: usize, key: &str, innermost: bool, range: graph::SrcRange) -> Result<execution::Value, Error> {
        use execution::Value as V;
        let index = V::CtxRef(execution::CtxRef::RepeatIndexAt(self.group_id(Group::Round(fork))));
        Ok(match key {
            "index" => index,
            "count" if innermost => V::CtxRef(execution::CtxRef::RepeatCount),
            "count" => self.round_count(fork)?.0,
            _ => {
                let items = *(self.graph.forks[fork].props.iter())
                    .find(|id| self.graph.bindings[id.0].path == ["items"])
                    .ok_or("item requires an each fork")?;
                V::Index {
                    target: Box::new(V::VarRef(format!("fork_input_{}", items.0))),
                    index: Box::new(index),
                    range,
                }
            }
        })
    }

    fn fork_bindings(&mut self, node: graph::NodeId) -> Result<Vec<execution::Binding>, Error> {
        let ids: Vec<_> = self
            .graph
            .forks
            .iter()
            .filter(|fork| fork.source == node)
            .flat_map(|fork| fork.props.iter().copied())
            .filter(|id| matches!(self.graph.bindings[id.0].path[0].as_str(), "count" | "items"))
            .collect();
        let mut bindings = self.weighted_bindings(Scope::Span(node))?;
        bindings.extend(
            ids.into_iter()
                .map(|id| {
                    Ok(execution::Binding {
                        name: format!("fork_input_{}", id.0),
                        label: None,
                        value: self.binding_value(id)?,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?,
        );
        bindings.extend(self.emission_bindings(Scope::Span(node))?);
        Ok(bindings)
    }

    fn weighted_branches(&self, node: graph::NodeId) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        for (index, fork) in self.graph.forks.iter().enumerate() {
            if fork.primitive != graph::Primitive::Weighted {
                continue;
            }
            for (branch, edges) in fork.branches.iter().enumerate() {
                // a loop-back branch starts a new round, it does not lead to its target
                let mut pending: Vec<_> = (edges.iter().map(|id| &self.graph.edges[id.0]))
                    .filter(|edge| !edge.cycle)
                    .map(|edge| edge.target)
                    .collect();
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
                            .filter(|edge| edge.source == current && !edge.cycle && edge.primitive == graph::Primitive::Next)
                            .map(|edge| edge.target),
                    );
                }
            }
        }
        result
    }

    // whether a step runs at all: every guard and weighted pick on the way to it
    fn node_condition(&self, node: graph::NodeId) -> execution::Value {
        self.selected(&self.guards[&node], &self.weighted_branches(node), node)
    }

    // the guards and weighted picks that decide a step beyond those that decide its parent span
    fn own_gates(&self, node: graph::NodeId) -> (Vec<graph::BindingId>, Vec<(usize, usize)>) {
        let guards = &self.guards[&node];
        let picks = self.weighted_branches(node);
        let Some(parent) = (self.parents.get(&node)).filter(|parent| self.guards.contains_key(parent)) else {
            return (guards.clone(), picks);
        };
        let inherited = &self.guards[parent];
        let guards = match guards.strip_prefix(inherited.as_slice()) {
            Some(own) => own.to_vec(),
            None => guards.clone(),
        };
        let above = self.weighted_branches(*parent);
        (guards, picks.into_iter().filter(|pick| !above.contains(pick)).collect())
    }

    // whether a step runs, given that its parent span does
    fn gate_condition(&self, node: graph::NodeId) -> execution::Value {
        let (guards, picks) = self.own_gates(node);
        self.selected(&guards, &picks, node)
    }

    fn selected(&self, guards: &[graph::BindingId], picks: &[(usize, usize)], node: graph::NodeId) -> execution::Value {
        let mut condition = self.condition(guards);
        for (fork, branch) in picks {
            let selected = execution::Value::Binary {
                op: execution::BinOp::Eq,
                lhs: Box::new(execution::Value::VarRef(format!("fork_pick_{fork}"))),
                rhs: Box::new(execution::Value::Num(execution::Number::Int(*branch as i64))),
                range: self.graph.forks[*fork].range,
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
            .is_some_and(|fork| self.graph.forks[fork.0].primitive == graph::Primitive::Weighted)
    }

    // the innermost scope that holds a weighted fork and every step it may select
    fn weighted_owner(&self, fork: &graph::Fork) -> Scope {
        let targets: Vec<_> = (fork.branches.iter().flatten())
            .map(|id| self.graph.edges[id.0].target)
            .collect();
        let under = |scope: Scope, node: graph::NodeId| match scope {
            Scope::Group(group) => self.all_groups(node).contains(&group),
            Scope::Span(span) => {
                let mut cursor = node;
                loop {
                    if cursor == span {
                        return true;
                    }
                    let Some(parent) = self.parents.get(&cursor) else {
                        return false;
                    };
                    cursor = *parent;
                }
            }
        };
        let mut node = fork.source;
        loop {
            let mut scopes = vec![Scope::Span(node)];
            scopes.extend(self.level_groups(node).into_iter().rev().map(Scope::Group));
            if let Some(scope) = (scopes.into_iter()).find(|scope| targets.iter().all(|target| under(*scope, *target))) {
                return scope;
            }
            match self.parents.get(&node) {
                Some(parent) => node = *parent,
                None => return Scope::Span(self.root),
            }
        }
    }

    fn weighted_bindings(&mut self, owner: Scope) -> Result<Vec<execution::Binding>, Error> {
        let forks: Vec<_> = self
            .graph
            .forks
            .iter()
            .enumerate()
            .filter(|(_, fork)| {
                self.order.contains(&fork.source)
                    && fork.primitive == graph::Primitive::Weighted
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
                    label: None,
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
                    // a feeds edge beside a branch shares the weight of its next edge
                    let Some(id) = (self.graph.edges[edge.0].props.iter().copied())
                        .find(|id| self.graph.bindings[id.0].path == ["weight"])
                    else {
                        continue;
                    };
                    weights.push(execution::Value::Cond {
                        cond: Box::new(guard.clone()),
                        then: Box::new(self.binding_value(id)?),
                        otherwise: Box::new(execution::Value::Num(execution::Number::Int(0))),
                    });
                }
                if weights.is_empty() {
                    return Err(located(fork.range, "weighted branch needs weight"));
                }
                branches.push(weights);
            }
            bindings.push(execution::Binding {
                name: format!("fork_pick_{index}"),
                label: None,
                value: execution::Value::Func {
                    func: execution::Func::ForkPick(branches),
                    range: fork.range,
                },
            });
        }
        Ok(bindings)
    }

    // the values evaluated once for each iteration of a group
    fn iteration_bindings(&mut self, group: Group) -> Result<Vec<execution::Binding>, Error> {
        let mut bindings = Vec::new();
        for id in 0..self.graph.bindings.len() {
            let binding = &self.graph.bindings[id];
            let (name, reader) = match binding.owner {
                graph::ElementId::Node(node)
                    if self.within(node, group)
                        && !binding.property
                        && !matches!(resolved(self.graph, &binding.value), graph::Value::GraphRef { .. })
                        && (!self.graph.derives(&self.graph.nodes[node.0].kind, "span")
                            || Self::field(&binding.path[0]).is_none()) =>
                {
                    (format!("binding_{id}"), None)
                }
                graph::ElementId::Edge(edge) => {
                    let edge = &self.graph.edges[edge.0];
                    let carried = edge.cycle && edge.primitive == graph::Primitive::Feeds;
                    match binding.path[0].as_str() {
                        "when" if !self.weighted_guard(graph::BindingId(id)) && self.within(edge.source, group) => {
                            (format!("guard_{id}"), None)
                        }
                        // a value fed along a loop-back is kept by the round that sends it
                        "value" if carried && group == Group::Loop(edge.target) && self.order.contains(&edge.source) => {
                            (format!("edge_value_{id}"), Some(self.all_groups(edge.target)))
                        }
                        "value" if !carried && self.within(edge.source, group) => (format!("edge_value_{id}"), None),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            if let Some(reader) = reader {
                self.readers.push(reader);
                let value = self.binding_value(graph::BindingId(id));
                self.readers.pop();
                bindings.push(execution::Binding {
                    name,
                    label: None,
                    value: value?,
                });
            } else {
                bindings.push(execution::Binding {
                    name,
                    label: None,
                    value: self.binding_value(graph::BindingId(id))?,
                });
            }
        }
        bindings.extend(self.weighted_bindings(Scope::Group(group))?);
        if let Group::Round(fork) = group {
            // a correlation key is read in the round whose arrivals it groups
            for index in 0..self.graph.junctions.len() {
                let junction = &self.graph.junctions[index];
                if !self.order.contains(&junction.target) || junction.fork.map(|fork| fork.0) != Some(fork) {
                    continue;
                }
                if let Some(id) = self.junction_prop(junction, "correlate") {
                    let member = self.graph.forks[fork].branches.iter().flatten().next();
                    let member = member.map(|edge| self.graph.edges[edge.0].target);
                    self.readers
                        .push(member.map(|member| self.all_groups(member)).unwrap_or_default());
                    let value = self.binding_value(id);
                    self.readers.pop();
                    bindings.push(execution::Binding {
                        name: format!("correlate_{index}"),
                        label: None,
                        value: value?,
                    });
                }
            }
        }
        bindings.extend(self.emission_bindings(Scope::Group(group))?);
        Ok(bindings)
    }

    // the scope that holds the repeat a junction's emissions run in
    fn emission_scope(&self, index: usize) -> Scope {
        let target = self.graph.junctions[index].target;
        let groups = self.level_groups(target);
        match groups.iter().position(|group| *group == Group::Emit(index)) {
            Some(position) if position > 0 => Scope::Group(groups[position - 1]),
            _ => Scope::Span(self.parents[&target]),
        }
    }

    fn emission_bindings(&mut self, scope: Scope) -> Result<Vec<execution::Binding>, Error> {
        let mut bindings = Vec::new();
        for index in 0..self.graph.junctions.len() {
            let target = self.graph.junctions[index].target;
            if !self.order.contains(&target) || !self.emits(index) || self.emission_scope(index) != scope {
                continue;
            }
            // the emissions are gathered outside the repeat they drive
            let mut reader = self.all_groups(self.parents[&target]);
            let groups = self.level_groups(target);
            let position = groups.iter().position(|group| *group == Group::Emit(index)).unwrap_or(0);
            reader.extend(&groups[..position]);
            self.readers.push(reader);
            let value = self.emissions(index);
            self.readers.pop();
            bindings.push(execution::Binding {
                name: format!("emissions_{index}"),
                label: None,
                value: value?,
            });
        }
        Ok(bindings)
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

    // appends a reference to the plan and returns the value that reads it
    fn block(
        &mut self,
        steps: Vec<execution::Step>,
        accessor: execution::Accessor,
        path: Vec<execution::Selection>,
        range: graph::SrcRange,
    ) -> execution::Value {
        let ref_id = execution::RefId(self.result.refs.len() as u32);
        self.result.refs.push(execution::ResolvedRef {
            module: Some(Self::id(self.root)),
            up: 0,
            steps,
            accessor,
            path,
            range,
        });
        execution::Value::BlockRef { ref_id, range }
    }

    // the steps from the entry span to a member, its gate or the iteration of one of its groups
    fn route(&mut self, node: graph::NodeId, end: End) -> Result<Vec<execution::Step>, Error> {
        use execution::{Step, Value as V};
        let mut nodes = vec![];
        let mut cursor = node;
        while cursor != self.root {
            nodes.push(cursor);
            cursor = *(self.parents.get(&cursor))
                .ok_or("value references must stay within the selected agent")
                .at(self.graph.nodes[node.0].range)?;
        }
        nodes.reverse();
        let reader = self.reader();
        let range = self.graph.nodes[node.0].range;
        let named = match &self.graph.nodes[node.0].name {
            Some(name) => format!("`{name}`"),
            None => "this step".to_owned(),
        };
        let child = |id: execution::NodeId| Step::Child {
            candidates: vec![id],
            position: None,
        };
        let mut steps = Vec::new();
        for current in nodes {
            for group in self.level_groups(current) {
                let id = self.group_id(group);
                steps.push(child(id));
                let index = V::CtxRef(execution::CtxRef::RepeatIndexAt(id));
                let across = (self.across.iter().rev())
                    .find(|(found, _)| *found == group)
                    .map(|(_, across)| *across);
                steps.push(match across {
                    Some(Across::All) => Step::Iterations { start: None, end: None },
                    Some(Across::Before) => Step::Iterations {
                        start: None,
                        end: Some(index),
                    },
                    Some(Across::Previous) => Step::Iteration(V::Binary {
                        op: execution::BinOp::Sub,
                        lhs: Box::new(index),
                        rhs: Box::new(V::Num(execution::Number::Int(1))),
                        range,
                    }),
                    None if reader.contains(&group) => Step::Iteration(index),
                    // a step after a loop reads the round the loop ended on
                    None if matches!(group, Group::Loop(_)) => Step::Last,
                    None if matches!(group, Group::Round(_)) => {
                        return Err(located(
                            range,
                            format!(
                                "{named} runs once for each round of its fork, read it from a step in the round or through a junction"
                            ),
                        ));
                    }
                    None => {
                        return Err(located(
                            range,
                            format!("{named} runs once for each arrival at its junction, read it from a step that runs with it"),
                        ));
                    }
                });
                if current == node && matches!(end, End::Iteration(found) if found == group) {
                    return Ok(steps);
                }
            }
            if self.gated(current) {
                steps.push(child(self.guard_id(current)));
                if current == node && matches!(end, End::Guard) {
                    return Ok(steps);
                }
            }
            steps.push(child(Self::id(current)));
        }
        Ok(steps)
    }

    // reads a value declared in the scope of a member, by name when the reader shares that scope
    fn scoped(&mut self, name: String, member: graph::NodeId, range: graph::SrcRange) -> Result<execution::Value, Error> {
        let Some((group, holder)) = self.enclosing(member) else {
            return Ok(execution::Value::VarRef(name));
        };
        let crossed = self.across.iter().any(|(found, _)| *found == group);
        if !crossed && self.reader().contains(&group) {
            return Ok(execution::Value::VarRef(name));
        }
        let steps = self.route(holder, End::Iteration(group))?;
        Ok(self.block(steps, execution::Accessor::Binding(name), vec![], range))
    }

    // the record a junction target reads for one arrival
    fn arrival(name: execution::Value, value: execution::Value) -> execution::Value {
        execution::Value::Object(execution::Object {
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
        })
    }

    fn edge_name(&self, id: graph::EdgeId) -> execution::Value {
        (self.graph.edges[id.0].name.clone())
            .map(execution::Value::Str)
            .unwrap_or(execution::Value::Num(execution::Number::Int(id.0 as i64)))
    }

    // what a junction observes on one input: the result of the step it reaches or the value it carries
    fn observed(&mut self, junction: &graph::Junction, id: graph::EdgeId) -> Result<Option<execution::Value>, Error> {
        let edge = &self.graph.edges[id.0];
        let result = (junction.props.iter())
            .any(|id| matches!(&self.graph.bindings[id.0].value, graph::Value::Str(value) if value == "result"));
        if result {
            return (self.binding(edge.target, "output"))
                .map(|binding| self.reference(binding))
                .transpose();
        }
        let source = edge.source;
        let range = edge.range;
        match edge.props.iter().find(|id| self.graph.bindings[id.0].path == ["value"]) {
            Some(id) => Ok(Some(self.scoped(format!("edge_value_{}", id.0), source, range)?)),
            None => Ok(None),
        }
    }

    // the inputs of a junction in the order their branches run
    fn arrival_order(&self, junction: &graph::Junction) -> Vec<usize> {
        let branches: Vec<_> = match junction.fork {
            Some(fork) => self.graph.forks[fork.0].branches.iter().flatten().copied().collect(),
            None => (0..self.graph.edges.len()).map(graph::EdgeId).collect(),
        };
        let mut order: Vec<usize> = (0..junction.inputs.len()).collect();
        order.sort_by_key(|position| branches.iter().position(|edge| *edge == junction.inputs[*position]));
        order
    }

    // every emission of a junction, each one the arrivals that fire together
    fn emissions(&mut self, index: usize) -> Result<execution::Value, Error> {
        let junction = self.graph.junctions[index].clone();
        let round = self.junction_round(&junction);
        let range = junction.range;
        if let Some(group) = round {
            self.across.push((group, Across::All));
        }
        let mut inputs = Vec::new();
        let mut failure = None;
        for id in &junction.inputs {
            let target = self.graph.edges[id.0].target;
            let arrived = if !self.gated(target) {
                Ok(None)
            } else if round.is_some() {
                (self.route(target, End::Guard))
                    .map(|steps| Some(self.block(steps, execution::Accessor::Included, vec![], range)))
            } else {
                Ok(Some(self.node_condition(target)))
            };
            let value = self.observed(&junction, *id);
            match (arrived, value) {
                (Ok(arrived), Ok(value)) => inputs.push(execution::EmissionInput {
                    edge: self.edge_name(*id),
                    arrived,
                    shared: !matches!(value, Some(execution::Value::BlockRef { .. })),
                    value,
                }),
                (Err(error), _) | (_, Err(error)) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let keys = match (round, self.junction_prop(&junction, "correlate")) {
            (Some(group), Some(_)) if failure.is_none() => {
                let Group::Round(fork) = group else { unreachable!() };
                let member = (self.graph.forks[fork].branches.iter().flatten().next())
                    .map(|edge| self.graph.edges[edge.0].target)
                    .ok_or("a correlated junction needs a fork branch");
                match member.at(range).and_then(|member| self.route(member, End::Iteration(group))) {
                    Ok(steps) => Some(self.block(
                        steps,
                        execution::Accessor::Binding(format!("correlate_{index}")),
                        vec![],
                        range,
                    )),
                    Err(error) => {
                        failure = Some(error);
                        None
                    }
                }
            }
            _ => None,
        };
        if round.is_some() {
            self.across.pop();
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let rounds = match round {
            Some(Group::Round(fork)) => Some(self.round_count(fork)?.0),
            _ => None,
        };
        let count = (self.junction_prop(&junction, "count"))
            .map(|id| self.binding_value(id))
            .transpose()?;
        Ok(execution::Value::Func {
            func: execution::Func::Emissions(Box::new(execution::Emissions {
                kind: self.junction_kind(&junction),
                order: self.arrival_order(&junction),
                inputs,
                rounds,
                keys,
                count,
            })),
            range,
        })
    }

    fn arrivals(&mut self, node: graph::NodeId) -> Result<execution::Value, Error> {
        use execution::{EmissionKind as Kind, Value as V};
        let index = self
            .junction_index(node)
            .ok_or("context.arrivals requires a junction target")?;
        let junction = self.graph.junctions[index].clone();
        let range = junction.range;
        if self.emits(index) {
            // the target runs once for each emission and reads its own
            return Ok(V::Index {
                target: Box::new(V::VarRef(format!("emissions_{index}"))),
                index: Box::new(V::CtxRef(execution::CtxRef::RepeatIndexAt(self.group_id(Group::Emit(index))))),
                range,
            });
        }
        let kind = self.junction_kind(&junction);
        if kind == Kind::Quorum || (kind == Kind::Reduce && self.junction_round(&junction).is_some()) {
            return Ok(V::Index {
                target: Box::new(self.emissions(index)?),
                index: Box::new(V::Num(execution::Number::Int(0))),
                range,
            });
        }
        // one activation: accumulate lists its inputs as declared, the others in the order they arrive
        let positions = match kind {
            Kind::Accumulate => (0..junction.inputs.len()).collect(),
            _ => self.arrival_order(&junction),
        };
        let mut records = Vec::new();
        for position in positions {
            let id = junction.inputs[position];
            let target = self.graph.edges[id.0].target;
            let condition = self.gated(target).then(|| self.node_condition(target));
            let value = self.observed(&junction, id)?.unwrap_or(V::Null);
            records.push((condition, Self::arrival(self.edge_name(id), value)));
        }
        let single = |record: V| {
            V::Array(execution::Array {
                elem: vec![execution::ArrayElem::Item(record)],
            })
        };
        let empty = || V::Array(execution::Array { elem: vec![] });
        Ok(match kind {
            Kind::First => {
                let mut value = empty();
                for (condition, record) in records.into_iter().rev() {
                    value = match condition {
                        Some(cond) => V::Cond {
                            cond: Box::new(cond),
                            then: Box::new(single(record)),
                            otherwise: Box::new(value),
                        },
                        None => single(record),
                    };
                }
                value
            }
            Kind::Reduce => V::Array(execution::Array {
                elem: (records.into_iter())
                    .map(|(condition, record)| match condition {
                        Some(cond) => execution::ArrayElem::Spread(V::Cond {
                            cond: Box::new(cond),
                            then: Box::new(single(record)),
                            otherwise: Box::new(empty()),
                        }),
                        None => execution::ArrayElem::Item(record),
                    })
                    .collect(),
            }),
            _ => V::Array(execution::Array {
                elem: (records.into_iter())
                    .map(|(_, record)| execution::ArrayElem::Item(record))
                    .collect(),
            }),
        })
    }

    // the values upstream elements supply for a context key that reaches a node
    fn supplied(&self, node: graph::NodeId, path: &[String]) -> Option<Vec<graph::BindingId>> {
        let key = path.first()?;
        let supply = (self.graph.context.get(node.0)?.iter()).find(|supply| &supply.key == key)?;
        let values: Vec<_> = (self.graph.context_values.iter())
            .filter(|value| &value.key == key && supply.suppliers.contains(&value.owner))
            .map(|value| value.binding)
            .collect();
        (!values.is_empty()).then_some(values)
    }

    // a step that may not run when its parent span does sits under a gate
    fn gated(&self, node: graph::NodeId) -> bool {
        let (guards, picks) = self.own_gates(node);
        let guarded = !guards.is_empty() || !picks.is_empty();
        match self.junction_index(node) {
            // the count of emissions decides how often this target runs
            Some(index) if self.emits(index) => false,
            // a reduce target always runs
            Some(index) => guarded || self.junction_kind(&self.graph.junctions[index]) != execution::EmissionKind::Reduce,
            None => guarded,
        }
    }

    // the folded value a reduce junction hands its target
    fn accumulator(&mut self, node: graph::NodeId) -> Result<execution::Value, Error> {
        let index = self
            .junction_index(node)
            .filter(|index| self.graph.junctions[*index].primitive == graph::Primitive::Reduce)
            .ok_or("context.accumulator requires a reduce junction target")?;
        let junction = self.graph.junctions[index].clone();
        let initial = self.junction_prop(&junction, "initial").ok_or("reduce needs initial")?;
        let combine = self.junction_prop(&junction, "combine").ok_or("reduce needs combine")?;
        let result = (junction.props.iter())
            .any(|id| matches!(&self.graph.bindings[id.0].value, graph::Value::Str(value) if value == "result"));
        let output = match junction.inputs.as_slice() {
            [input] if result => self.binding(self.graph.edges[input.0].target, "output"),
            _ => None,
        };
        // one step observed across the rounds of its fork folds its results directly
        let round = self.junction_round(&junction).filter(|_| !self.emits(index));
        let (items, raw) = match (round, output) {
            (Some(group), Some(output)) => {
                self.across.push((group, Across::All));
                let items = self.reference(output);
                self.across.pop();
                (items?, true)
            }
            _ => (self.arrivals(node)?, false),
        };
        let outer = std::mem::replace(&mut self.raw_fold, raw);
        let combine = self.binding_value(combine);
        self.raw_fold = outer;
        Ok(execution::Value::Func {
            func: execution::Func::Fold {
                items: Box::new(items),
                initial: Box::new(self.binding_value(initial)?),
                combine: Box::new(combine?),
            },
            range: junction.range,
        })
    }

    fn guard_id(&self, node: graph::NodeId) -> execution::NodeId {
        execution::NodeId((self.graph.nodes.len() + node.0) as u32)
    }

    // whether a loop runs another round, read in the round that just ran
    fn again(&mut self, target: graph::NodeId) -> Result<execution::Value, Error> {
        use execution::Value as V;
        let (id, source) = (self.loops[&target].edge, self.loops[&target].source);
        let edge = self.graph.edges[id.0].clone();
        let both = |lhs: V, rhs: V| V::Binary {
            op: execution::BinOp::And,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            range: edge.range,
        };
        // the loop-back is only reached in a round where its source ran
        let level = self.parents[&target];
        let mut chain = vec![source];
        while self.parents[chain.last().unwrap()] != level {
            chain.push(self.parents[chain.last().unwrap()]);
        }
        let mut condition = V::Bool(true);
        if let Some(gate) = chain.into_iter().find(|node| self.gated(*node)) {
            let steps = self.route(gate, End::Guard)?;
            let included = self.block(steps, execution::Accessor::Included, vec![], edge.range);
            condition = V::Binary {
                op: execution::BinOp::Eq,
                lhs: Box::new(included),
                rhs: Box::new(V::Bool(true)),
                range: edge.range,
            };
        }
        for id in &edge.props {
            if self.graph.bindings[id.0].path == ["when"] && !self.weighted_guard(*id) {
                condition = both(condition, self.scoped(format!("guard_{}", id.0), source, edge.range)?);
            }
        }
        if let Some(fork) = edge.fork
            && self.graph.forks[fork.0].primitive == graph::Primitive::Weighted
        {
            let branch = (self.graph.forks[fork.0].branches.iter())
                .position(|branch| branch.contains(&id))
                .unwrap_or(0);
            let picked = V::Binary {
                op: execution::BinOp::Eq,
                lhs: Box::new(V::VarRef(format!("fork_pick_{}", fork.0))),
                rhs: Box::new(V::Num(execution::Number::Int(branch as i64))),
                range: edge.range,
            };
            condition = both(condition, picked);
        }
        Ok(condition)
    }

    // the repeat that runs the steps of a group
    fn repeat(&mut self, group: Group, children: Vec<execution::Child>) -> Result<execution::Child, Error> {
        use execution::Value as V;
        let node = self.group_id(group);
        let bindings = self.iteration_bindings(group)?;
        let named = |this: &Self, node: graph::NodeId, role: &str| {
            let step = &this.graph.nodes[node.0];
            Some(format!("{}_{role}", step.name.clone().unwrap_or_else(|| step.kind.clone())))
        };
        Ok(execution::Child::Repeat(match group {
            Group::Round(index) => {
                let (count, count_range) = self.round_count(index)?;
                execution::Repeat {
                    node,
                    name: self.graph.forks[index].name.clone(),
                    count,
                    count_range,
                    again: None,
                    bindings,
                    children,
                }
            }
            Group::Loop(target) => {
                let edge = self.graph.edges[self.loops[&target].edge.0].clone();
                let inside = self.all_groups(target);
                self.readers.push(inside.clone());
                let condition = self.again(target);
                self.readers.pop();
                let limit = edge
                    .props
                    .iter()
                    .copied()
                    .find(|id| self.graph.bindings[id.0].path == ["max_traversals"]);
                self.readers
                    .push(inside.into_iter().filter(|found| *found != group).collect());
                let limit = limit.map(|id| self.binding_value(id)).transpose();
                self.readers.pop();
                execution::Repeat {
                    node,
                    name: named(self, target, "round"),
                    count: V::Num(execution::Number::Int(1)),
                    count_range: edge.range,
                    again: Some(execution::Again {
                        condition: condition?,
                        limit: limit?,
                        range: edge.range,
                    }),
                    bindings,
                    children,
                }
            }
            Group::Emit(index) => {
                let junction = &self.graph.junctions[index];
                let target = junction.target;
                let range = junction.range;
                let mut count = V::Func {
                    func: execution::Func::Len {
                        target: Box::new(V::VarRef(format!("emissions_{index}"))),
                    },
                    range,
                };
                let (guards, picks) = self.own_gates(target);
                if !guards.is_empty() || !picks.is_empty() {
                    count = V::Cond {
                        cond: Box::new(self.gate_condition(target)),
                        then: Box::new(count),
                        otherwise: Box::new(V::Num(execution::Number::Int(0))),
                    };
                }
                execution::Repeat {
                    node,
                    name: named(self, target, "arrival"),
                    count,
                    count_range: range,
                    again: None,
                    bindings,
                    children,
                }
            }
        }))
    }

    // the span of a member, under a gate when it may not run
    fn child(&mut self, node: graph::NodeId) -> Result<execution::Child, Error> {
        use execution::{EmissionKind as Kind, Value as V};
        let kind = &self.graph.nodes[node.0].kind;
        let range = self.graph.nodes[node.0].range;
        // the root node kind and a holder of values emit no span
        let span = if kind == "node" || self.graph.nodes[node.0].primitive == graph::Primitive::Vars {
            execution::Child::Maybe(execution::Maybe {
                node: Self::id(node),
                name: self.graph.nodes[node.0].name.clone(),
                chance: V::Num(execution::Number::Int(1)),
                chance_range: self.graph.nodes[node.0].range,
                bindings: self.fork_bindings(node)?,
                children: self.children(node)?,
            })
        } else if self.graph.derives(kind, "scorer") {
            if self.parents.values().any(|parent| *parent == node) {
                return Err(located(range, "scorer spans cannot contain child spans yet"));
            }
            let score = self.binding(node, "score").ok_or("scorer needs a score field").at(range)?;
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
            let kind = if self.graph.derives(kind, "tool") {
                execution::SpanKind::Tool
            } else if self.graph.derives(kind, "llm") {
                execution::SpanKind::Llm
            } else if self.graph.derives(kind, "function") {
                execution::SpanKind::Function
            } else {
                // the capability check leaves task and span itself
                execution::SpanKind::Task
            };
            execution::Child::Span(execution::Span {
                node: Self::id(node),
                name: self.name(node)?,
                label: self.graph.nodes[node.0].name.clone(),
                kind,
                fields: self.fields(node)?,
                bindings: self.fork_bindings(node)?,
                children: self.children(node)?,
            })
        };
        if !self.gated(node) {
            return Ok(span);
        }
        let condition = self.gate_condition(node);
        let one = || V::Num(execution::Number::Int(1));
        let zero = || V::Num(execution::Number::Int(0));
        let active = match self.junction_index(node) {
            Some(index) => {
                let junction = self.graph.junctions[index].clone();
                let ready = |this: &Self, all: bool| {
                    (junction.inputs.iter()).fold(V::Bool(all), |ready, id| V::Binary {
                        op: if all { execution::BinOp::And } else { execution::BinOp::Or },
                        lhs: Box::new(ready),
                        rhs: Box::new(this.node_condition(this.graph.edges[id.0].target)),
                        range: junction.range,
                    })
                };
                match self.junction_kind(&junction) {
                    // accumulate needs every listed input
                    Kind::Accumulate => V::Func {
                        func: execution::Func::Arrivals {
                            ready: Box::new(ready(self, true)),
                        },
                        range: junction.range,
                    },
                    // first needs any one of them
                    Kind::First => V::Cond {
                        cond: Box::new(ready(self, false)),
                        then: Box::new(one()),
                        otherwise: Box::new(zero()),
                    },
                    // a quorum fires once or fails
                    Kind::Quorum => V::Func {
                        func: execution::Func::Len {
                            target: Box::new(self.emissions(index)?),
                        },
                        range: junction.range,
                    },
                    _ => one(),
                }
            }
            None => one(),
        };
        Ok(execution::Child::Maybe(execution::Maybe {
            node: self.guard_id(node),
            name: None,
            chance: V::Cond {
                cond: Box::new(condition),
                then: Box::new(active),
                otherwise: Box::new(zero()),
            },
            chance_range: self.graph.nodes[node.0].range,
            bindings: vec![],
            children: vec![span],
        }))
    }

    fn children(&mut self, parent: graph::NodeId) -> Result<Vec<execution::Child>, Error> {
        let nodes: Vec<_> = (self.order.iter().copied())
            .filter(|node| self.parents.get(node) == Some(&parent))
            .collect();
        let mut members = Vec::new();
        for node in nodes {
            let groups = self.level_groups(node);
            if let Some(Group::Round(index)) = groups.first()
                && self.graph.forks[*index].source != parent
            {
                return Err(located(
                    self.graph.nodes[node.0].range,
                    "each branch spans must be children of the fork source",
                ));
            }
            members.push((groups, self.child(node)?));
        }
        self.nest(members, 0)
    }

    // gathers the steps of each group into one repeat, placed where the group starts
    fn nest(&mut self, members: Vec<Member>, depth: usize) -> Result<Vec<execution::Child>, Error> {
        let mut result: Vec<Option<execution::Child>> = Vec::new();
        let mut gathered: Vec<(Group, usize, Vec<Member>)> = Vec::new();
        for (groups, child) in members {
            let Some(group) = groups.get(depth).copied() else {
                result.push(Some(child));
                continue;
            };
            match gathered.iter_mut().find(|(found, _, _)| *found == group) {
                Some((_, _, members)) => members.push((groups, child)),
                None => {
                    gathered.push((group, result.len(), vec![(groups, child)]));
                    result.push(None);
                }
            }
        }
        for (group, position, members) in gathered {
            let children = self.nest(members, depth + 1)?;
            result[position] = Some(self.repeat(group, children)?);
        }
        Ok(result.into_iter().flatten().collect())
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

    fn fields(&mut self, node: graph::NodeId) -> Result<execution::SpanFields, Error> {
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
                        return Err(located(
                            binding.range,
                            format!("{name} must be an object expression for mocking"),
                        ));
                    };
                    if name == "metadata" {
                        fields.metadata = Some(object);
                    } else {
                        fields.metrics = Some(object);
                    }
                }
                "tags" => {
                    let execution::Value::Array(array) = value else {
                        return Err(located(binding.range, "tags must be an array expression for mocking"));
                    };
                    for item in array.elem {
                        fields.tags.push(match item {
                            execution::ArrayElem::Item(execution::Value::Str(text)) => execution::Template {
                                parts: vec![execution::Part::Lit(text)],
                            },
                            execution::ArrayElem::Item(execution::Value::Template(template)) => template,
                            _ => return Err(located(binding.range, "mocking tags must be strings")),
                        });
                    }
                }
                "duration" => fields.duration = Some((execution::Value::VarRef(format!("binding_{}", id.0)), binding.range)),
                "scores" => {
                    return Err(located(binding.range, "span scores are not supported by agent mocking yet"));
                }
                _ => {}
            }
        }
        Ok(fields)
    }

    fn reference(&mut self, id: graph::BindingId) -> Result<execution::Value, Error> {
        let binding = &self.graph.bindings[id.0];
        if self.callable && binding.owner == graph::ElementId::Node(self.root) {
            return self.binding_value(id);
        }
        if self.callable && self.graph.derives(&self.graph.nodes[self.root.0].kind, "scorer") {
            return Err("callable scorers read external values through context".into());
        }
        if let graph::ElementId::Node(node) = binding.owner
            && !self.order.contains(&node)
            && node != self.agent
        {
            let mut owners = Vec::new();
            for (index, candidate) in self.graph.nodes.iter().enumerate() {
                let agent = graph::NodeId(index);
                if self.graph.derives(&candidate.kind, "agent") && structure(self.graph, agent)?.members.contains(&node) {
                    owners.push(agent);
                }
            }
            if owners.len() != 1 {
                return Err("external value references need one owning agent".into());
            }
            return AgentLowering::new(self.graph, owners[0], self.result)?.reference(id);
        }
        let range = binding.range;
        let graph::ElementId::Node(mut node) = binding.owner else {
            return match binding.owner {
                graph::ElementId::Fork(_) if matches!(binding.path[0].as_str(), "items" | "count") => {
                    Ok(execution::Value::VarRef(format!("fork_input_{}", id.0)))
                }
                graph::ElementId::Edge(edge) if binding.path == ["value"] => {
                    self.scoped(format!("edge_value_{}", id.0), self.graph.edges[edge.0].source, range)
                }
                graph::ElementId::Edge(edge) if binding.path == ["when"] => {
                    self.scoped(format!("guard_{}", id.0), self.graph.edges[edge.0].source, range)
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
        // a step's own values live in the scope the step runs in
        if !self.graph.derives(&self.graph.nodes[node.0].kind, "span") {
            return self.scoped(format!("binding_{}", id.0), node, range);
        }
        let Some(field) = Self::field(&binding.path[0]) else {
            return self.scoped(format!("binding_{}", id.0), node, range);
        };
        let path = (binding.path.iter().skip(1))
            .map(|key| execution::Selection::Index(execution::Value::Str(key.clone())))
            .collect();
        let steps = self.route(node, End::Span)?;
        Ok(self.block(steps, execution::Accessor::Field(field), path, range))
    }

    fn binding_value(&mut self, id: graph::BindingId) -> Result<execution::Value, Error> {
        if self.building.contains(&id) {
            return Err(located(self.graph.bindings[id.0].range, "circular value propagation"));
        }
        self.building.push(id);
        let value = self.value(&self.graph.bindings[id.0].value.clone());
        self.building.pop();
        value.at(self.graph.bindings[id.0].range)
    }

    fn value(&mut self, value: &graph::Value) -> Result<execution::Value, Error> {
        use execution::Value as V;
        use graph::Value as G;
        Ok(match value {
            G::Context { path, .. } if path.first().is_some_and(|key| key == "parent") => {
                return Err("`context.parent` is supplied to the provider and cannot be read by a field".into());
            }
            G::Context {
                owner: graph::ElementId::Node(node),
                path,
            } if self.supplied(*node, path).is_some() => {
                // the definition that provides this key supplies its value
                let supplied = self.supplied(*node, path).unwrap();
                let [binding] = supplied.as_slice() else {
                    return Err(format!("`context.{}` reaches this step from more than one supplier", path[0]).into());
                };
                let member = matches!(
                    self.graph.bindings[binding.0].owner,
                    graph::ElementId::Node(owner) if self.order.contains(&owner)
                );
                let mut value = if member {
                    self.reference(*binding)?
                } else {
                    self.binding_value(*binding)?
                };
                for key in path.iter().skip(1) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range: self.graph.nodes[node.0].range,
                    };
                }
                value
            }
            G::Input { binding, ty } => {
                let entry = self.inputs.get(binding).copied();
                let range = self.graph.bindings[binding.0].range;
                let value = match (entry, self.carried.get(binding).copied()) {
                    (Some(entry), None) => V::VarRef(format!("edge_value_{}", entry.0)),
                    (Some(entry), Some(carried)) => {
                        // the first round takes the entry value, a later one what the round before it sent
                        let graph::ElementId::Node(target) = self.graph.bindings[binding.0].owner else {
                            return Err("a fed field belongs to a node".into());
                        };
                        let group = Group::Loop(target);
                        self.across.push((group, Across::Previous));
                        let steps = self.route(target, End::Iteration(group));
                        self.across.pop();
                        let name = format!("edge_value_{}", carried.0);
                        let previous = self.block(steps?, execution::Accessor::Binding(name), vec![], range);
                        V::Cond {
                            cond: Box::new(V::Binary {
                                op: execution::BinOp::Eq,
                                lhs: Box::new(V::CtxRef(execution::CtxRef::RepeatIndexAt(self.group_id(group)))),
                                rhs: Box::new(V::Num(execution::Number::Int(0))),
                                range,
                            }),
                            then: Box::new(V::VarRef(format!("edge_value_{}", entry.0))),
                            otherwise: Box::new(previous),
                        }
                    }
                    (None, _) => return Err("upstream field has no feeds edge".into()),
                };
                if *ty == graph::FieldType::Any {
                    value
                } else {
                    V::Func {
                        func: execution::Func::Check {
                            value: Box::new(value),
                            ty: *ty,
                        },
                        range,
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
                if internal {
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
                    .collect::<Result<_, Error>>()?,
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
                    .collect::<Result<_, Error>>()?,
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
                    .collect::<Result<_, Error>>()?,
            }),
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
                // the earlier rounds of the innermost group the reader and the step share
                let (group, _) = self.enclosing(target).ok_or("history requires a repetition group")?;
                if !self.all_groups(*node).contains(&group) {
                    return Err("history must stay within the current repetition group".into());
                }
                // any payload field locates the step, a field of its own such as local does not
                let binding = (self.graph.nodes[target.0].fields.iter().copied())
                    .find(|id| Self::field(&self.graph.bindings[id.0].path[0]).is_some())
                    .ok_or("history target needs a payload field")?;
                self.across.push((group, Across::Before));
                let reference = self.reference(binding);
                self.across.pop();
                let V::BlockRef { ref_id, range } = reference? else {
                    return Err("history target needs a payload field".into());
                };
                let reference = &mut self.result.refs[ref_id.0 as usize];
                reference.accessor = execution::Accessor::Record;
                reference.path.clear();
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
            } if path.first().is_some_and(|key| key == "accumulator") => {
                let mut value = self.accumulator(*node)?;
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
                owner: graph::ElementId::Junction(junction),
                path,
            } if path.first().is_some_and(|key| key == "accumulator" || key == "arrival") => {
                let range = self.graph.junctions[junction.0].range;
                let read = |func: execution::Func| V::Func { func, range };
                let (mut value, skip) = if path[0] == "accumulator" {
                    (read(execution::Func::FoldAccumulator), 1)
                } else if !self.raw_fold {
                    (read(execution::Func::FoldArrival), 1)
                } else {
                    // a fold over bare results reads the result where a record would hold it
                    let edge = self.graph.junctions[junction.0].inputs[0];
                    match path.get(1).map(String::as_str) {
                        Some("value") => (read(execution::Func::FoldArrival), 2),
                        Some("edge") => (self.edge_name(edge), 2),
                        _ => (Self::arrival(self.edge_name(edge), read(execution::Func::FoldArrival)), 1),
                    }
                };
                for key in path.iter().skip(skip) {
                    value = V::Index {
                        target: Box::new(value),
                        index: Box::new(V::Str(key.clone())),
                        range,
                    };
                }
                value
            }
            G::Context {
                owner: graph::ElementId::Junction(junction),
                path,
            } if path
                .first()
                .is_some_and(|key| key == "item" || key == "index" || key == "count") =>
            {
                // a junction reads the round of its own fork
                let junction = &self.graph.junctions[junction.0];
                let range = junction.range;
                let Some(Group::Round(fork)) = self.junction_round(junction) else {
                    return Err("item, index and count require a junction on an each or repeat fork".into());
                };
                let mut value = self.round_value(fork, &path[0], false, range)?;
                for key in path.iter().skip(1) {
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
                let (fork, _) = self.each_ancestor(*node).ok_or("item and index require an each occurrence")?;
                let innermost = self.enclosing(*node).is_some_and(|(group, _)| group == Group::Round(fork));
                let mut value = self.round_value(fork, &path[0], innermost, self.graph.nodes[node.0].range)?;
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
                    _ => return Err(format!("unsupported mocking context field {first:?}").into()),
                };
                let mut value = if self.callable {
                    V::ArgRef(arg)
                } else {
                    let graph::ElementId::Node(node) = owner else {
                        return Err("scorer context requires a node".into());
                    };
                    if !self.graph.derives(&self.graph.nodes[node.0].kind, "scorer") {
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

    fn function(&mut self, func: &graph::Func) -> Result<execution::Func, Error> {
        Ok(match func {
            graph::Func::English { operation, args } => execution::Func::English {
                operation: *operation,
                args: args.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?,
            },
            graph::Func::Call { function, args, .. } => {
                let implementation = (super::functions::FUNCTIONS.iter())
                    .find(|known| known.name == function)
                    .ok_or_else(|| format!("function `{function}` has no implementation"))?;
                execution::Func::Provider {
                    function: implementation,
                    args: args.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?,
                }
            }
            graph::Func::Choice(values) => {
                execution::Func::Choice(values.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?)
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
                    .collect::<Result<_, Error>>()?,
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
            graph::Func::Format { pieces, args } => execution::Func::Format {
                pieces: pieces.clone(),
                args: args.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?,
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
                execution::Func::Min(values.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?)
            }
            graph::Func::Max(values) => {
                execution::Func::Max(values.iter().map(|v| self.value(v)).collect::<Result<_, Error>>()?)
            }
            graph::Func::Uuid => execution::Func::Uuid,
            graph::Func::Hex { length } => execution::Func::Hex { length: *length },
            graph::Func::Alphanum { length } => execution::Func::Alphanum { length: *length },
        })
    }
}
