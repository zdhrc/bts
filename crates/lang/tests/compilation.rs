use bts_lang::SrcRange;
use bts_lang::semantics::{Source, compile_module_with_definitions, compile_with_definitions};
use bts_lang::syntax::{
    BinOp, Expr, ExprKind, GraphNode, GraphNodeDecl, GraphOrigin, GraphReferenceKind, GraphStep, ObjectItem, TemplatePart,
    parse,
};
use bts_lang::{DiagPhase, Value, semantics::compile, syntax::parse_traversal};

const COMPILATION_SOURCE: &str = include_str!("fixtures/compilation.bt");

// compile the provider's actual source while core awaits its graph migration
#[path = "../../core/src/providers/braintrust/defs.rs"]
mod braintrust_defs;
const BRAINTRUST_FIXTURE: &str = include_str!("../../core/tests/fixtures/braintrust.bt");

fn compile_braintrust(source: &str) -> Result<bts_lang::Model, bts_lang::Diags> {
    compile_with_definitions(
        Source {
            name: "user.bt",
            text: source,
        },
        &[braintrust_defs::SOURCE],
    )
}

#[test]
fn braintrust_provider_schemas_compile_inherited_spans_and_propagation() {
    let model = compile_braintrust(BRAINTRUST_FIXTURE).unwrap_or_else(|errors| panic!("{errors:#?}"));
    assert_eq!(model.sources, ["user.bt", "braintrust.bt"]);
    for name in ["tool", "task", "llm", "function", "scorer"] {
        let kind = model.kinds.iter().find(|kind| kind.name == name).unwrap();
        assert_eq!(kind.base, "span");
        assert_eq!(kind.primitive, "node");
        assert_eq!(kind.range.source, 1);
        assert!(
            kind.fields
                .iter()
                .any(|field| field.name == "input" && field.role.as_deref() == Some("input"))
        );
        assert!(
            kind.fields
                .iter()
                .any(|field| field.name == "output" && field.role.as_deref() == Some("output"))
        );
    }
    let local = model.kinds.iter().find(|kind| kind.name == "patient_lookup").unwrap();
    assert_eq!(local.base, "tool");
    assert!(
        local
            .props
            .iter()
            .any(|field| field.name == "resource" && field.default.is_some())
    );
    assert!(model.edges.iter().any(|edge| edge.kind == "feeds"));
    assert!(model.nodes.iter().any(|node| node.name.is_none() && node.kind == "task"));
}

#[test]
fn braintrust_agent_keeps_entry_and_dataflow_separate() {
    compile_braintrust(include_str!("../../core/tests/fixtures/agent.bt")).unwrap();
    let model = compile_braintrust(BRAINTRUST_FIXTURE).unwrap();
    let agent = model.kinds.iter().find(|kind| kind.name == "agent").unwrap();
    assert_eq!(agent.base, "node");
    assert!(!agent.fields.iter().any(|field| field.name == "duration"));
    let node = |name: &str| {
        model
            .nodes
            .iter()
            .position(|node| node.name.as_deref() == Some(name))
            .unwrap()
    };
    let intake = node("intake");
    // the entry edge is the agent's only edge, membership is not authored
    assert_eq!(model.edges.iter().filter(|edge| edge.source.0 == intake).count(), 1);
    let entries: Vec<_> = model
        .edges
        .iter()
        .filter(|edge| edge.source.0 == intake && edge.kind == "next")
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target.0, node("request"));
    for kind in ["calls", "feeds"] {
        assert!(
            model
                .edges
                .iter()
                .any(|edge| edge.source.0 == node("request") && edge.target.0 == node("lookup") && edge.kind == kind)
        );
    }
    compile_braintrust("+(special_agent:node)-[:extends]->(agent); (a:special_agent) {input: 1, output: self.input};").unwrap();
}

#[test]
fn braintrust_calls_supplies_parents_and_keeps_its_direction() {
    let model = compile_braintrust(BRAINTRUST_FIXTURE).unwrap();
    // each step's parent is the source of the calls edge that supplies its parent key
    let parent = |child: &str| -> Vec<&str> {
        let bts_lang::ElementId::Node(id) = model.names[child] else {
            panic!("{child}")
        };
        (model.context[id.0].iter())
            .filter(|supply| supply.key == "parent")
            .flat_map(|supply| &supply.suppliers)
            .map(|supplier| match supplier {
                bts_lang::ElementId::Edge(edge) => {
                    assert_eq!(model.edges[edge.0].kind, "calls");
                    model.nodes[model.edges[edge.0].source.0].name.as_deref().unwrap()
                }
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert!(parent("request").is_empty());
    for (child, expected) in [
        ("lookup", "request"),
        ("route", "request"),
        ("reply", "route"),
        ("format_reply", "request"),
        ("grade", "request"),
    ] {
        assert_eq!(parent(child), [expected], "{child}");
    }
    let declarations = "+(nested:edge)-[:extends]->(calls); (parent:task); (child:tool);";
    compile_braintrust(&format!("{declarations} (parent)-[:nested]->(child);")).unwrap();
    for edge in ["(parent)-[:calls]-(child);", "(parent)-[:nested]-(child);"] {
        assert!(compile_braintrust(&format!("{declarations} {edge}")).is_err());
    }
    assert!(compile_braintrust("+(nested:edge {direction: \"either\"})-[:extends]->(calls); (a:task);").is_err());
}

#[test]
fn braintrust_provider_schemas_reject_invalid_known_fields_and_properties() {
    for source in [
        "(a:agent {name: 42});",
        "(a:agent {description: false});",
        "(a:agent {entrypoint: \"request\"});",
        "(a:agent) {metadata: false};",
        "(a:tool {typo: true});",
        "(a:span {name: 42});",
        "(a:task) {metadata: false};",
        "(a:llm) {metrics: 1};",
        "(a:function) {tags: \"tag\"};",
        "(a:tool) {duration: \"slow\"};",
        "(a:span) {scores: false};",
        "(a:scorer) {score: \"good\"};",
        "(a:scorer) {score: 1, reason: false};",
    ] {
        let errors = compile_braintrust(source).expect_err(source);
        assert!(errors.iter().all(|error| error.r#where.source == 0), "{errors:#?}");
    }
    assert!(compile_braintrust("(a:tool) {custom: {anything: true}};").is_err());
    compile_braintrust("(a:tool) {local: {anything: true}};").unwrap();
    compile_braintrust(
        "+(custom_tool:node) {custom: {type: \"string\", required: true}}-[:extends]->(tool); (a:custom_tool) {custom: \"value\"};",
    )
    .unwrap();
    assert!(compile_braintrust("+(tool:node); (a:tool);").is_err());
}

// illustrative provider definitions, not the final Braintrust schema
const PROVIDER_SOURCE: &str = r#"
+(span:node {name: {type: "string", required: true}}) {
    patient_id: {type: "string"},
    patient: {type: "object"},
    description: {type: "string"},
    output: {type: "object", role: "output"},
    metadata: {type: "object", default: {}},
}
    -[:requires {patient_id: {type: "string"}}]->(context);

+(tool_span:node)-[:extends]->(span);

# the fixture's working kinds take any content field
+(step:node {open: true, name: {type: "string"}});
+(task:node {open: true, accumulate: {type: "any"}})-[:extends]->(step);
+(tool:node {open: true})-[:extends]->(step);
+(llm:node {open: true, model: {type: "string"}})-[:extends]->(step);
+(function:node {open: true})-[:extends]->(step);
+(agent:node {open: true, label: {type: "string"}, scope: {type: "any"}});
+(scorer:node {open: true, lang: {type: "string"}})
    -[:requires {output: {type: "any"}, expected: {type: "any"}}]->(context);
+(dataset:node {open: true});
+(case:node {open: true});
+(experiment:node {open: true});
"#;

fn compile_fixture() -> bts_lang::Model {
    compile_with_definitions(
        Source {
            name: "compilation.bt",
            text: COMPILATION_SOURCE,
        },
        &[Source {
            name: "braintrust.bt",
            text: PROVIDER_SOURCE,
        }],
    )
    .unwrap_or_else(|errors| panic!("{errors:#?}"))
}

fn named_fixture_node<'a>(ast: &'a bts_lang::syntax::Ast, name: &str) -> &'a GraphNodeDecl {
    ast.paths
        .iter()
        .find_map(|path| match &path.start {
            GraphOrigin::Node(GraphNode::Declaration(node)) if node.header.name.as_deref() == Some(name) => Some(node),
            _ => None,
        })
        .expect("fixture node must exist")
}

#[test]
fn parses_agent_fields_references_and_source_ranges() {
    let ast = parse(COMPILATION_SOURCE).unwrap();
    let settings = named_fixture_node(&ast, "settings");
    assert_eq!(settings.header.kind, "vars");
    assert_eq!(settings.header.props.as_ref().unwrap()[0].key, "evaluate");
    assert!(matches!(field(settings, "request_id").kind, ExprKind::Func { .. }));
    let intake = named_fixture_node(&ast, "intake");
    assert_eq!(intake.header.kind, "agent");
    assert!(matches!(field(intake, "input").kind, ExprKind::Object(_)));
    let references = named_fixture_node(&ast, "graph_references");
    assert!(matches!(
        field(references, "node").kind,
        ExprKind::GraphRef {
            kind: GraphReferenceKind::Node,
            ..
        }
    ));
    assert!(matches!(
        field(references, "edge").kind,
        ExprKind::GraphRef {
            kind: GraphReferenceKind::Edge,
            ..
        }
    ));
    let qualified = field(references, "qualified_edge");
    assert!(matches!(qualified.kind, ExprKind::GraphMember { .. }));
    assert_eq!(
        &COMPILATION_SOURCE[qualified.range.start..qualified.range.end],
        "[patient_checks].[coverage]"
    );
    let patterns = named_fixture_node(&ast, "pattern_references");
    assert!(matches!(field(patterns, "tool_node").kind, ExprKind::PatternRef(_)));
    assert!(matches!(field(patterns, "next_edge").kind, ExprKind::PatternRef(_)));
}

#[test]
fn separates_graph_syntax_from_semantic_resolution() {
    for source in [
        "(run:agent) {output: missing.output};",
        "(run:agent) {metadata: true};",
        "(block:unknown);",
    ] {
        assert!(bts_lang::syntax::parse(source).is_ok(), "{source}");
    }
    let errors = bts_lang::syntax::parse("(run:agent) {input: };").unwrap_err();
    assert!(errors.iter().any(|error| error.when == DiagPhase::Parsing));
    assert!(bts_lang::syntax::parse("").unwrap().paths.is_empty());
    assert!(compile("").is_err());
    assert!(bts_lang::semantics::compile_module("", true).unwrap().nodes.is_empty());
}

#[test]
fn parses_nested_forks_and_junctions_in_the_fixture() {
    let ast = parse(COMPILATION_SOURCE).unwrap();
    let intake = &ast.paths[2];
    assert_eq!(
        node_declaration(&edge(&intake.steps[0]).target).header.name.as_deref(),
        Some("collect")
    );
    assert_eq!(
        node_declaration(&edge(&intake.steps[1]).target).header.name.as_deref(),
        Some("lookup")
    );
    let route = fork(&intake.steps[2]);
    assert_eq!(route.header.kind, "exclusive");
    assert_eq!(route.entries.len(), 2);
    let review = fork(&route.continuations[0].steps[0]);
    assert_eq!(review.header.kind, "exclusive");
    let checks = fork(&review.continuations[0].steps[0]);
    assert_eq!(checks.header.kind, "parallel");
    assert_eq!(checks.header.name.as_deref(), Some("patient_checks"));
    assert_eq!(checks.entries.len(), 2);
    assert_eq!(checks.continuations.len(), 1);
    let GraphOrigin::Junction(junction) = &checks.continuations[0].start else {
        panic!("expected junction")
    };
    assert_eq!(junction.inputs.len(), 2);
    assert!(matches!(junction.target, GraphNode::Reference(_)));
    assert!(checks.continuations[0].steps.is_empty());
}

#[test]
fn parses_companion_definitions_from_the_fixture() {
    let ast = parse(COMPILATION_SOURCE).unwrap();
    let scorer = named_fixture_node(&ast, "routing_correct");
    assert_eq!(scorer.header.kind, "scorer");
    assert!(matches!(&scorer.header.props.as_ref().unwrap()[0].value.kind, ExprKind::Str(lang) if lang == "python"));
    assert!(matches!(field(scorer, "score").kind, ExprKind::Cond { .. }));
    let dataset = named_fixture_node(&ast, "intake_cases");
    assert_eq!(dataset.header.kind, "dataset");
    let ExprKind::Array(cases) = &field(dataset, "cases").kind else {
        panic!("expected cases")
    };
    assert_eq!(cases.len(), 1);
    assert!(matches!(cases[0].kind, ExprKind::GraphRef { .. }));
    let case = named_fixture_node(&ast, "appointment_case");
    assert_eq!(case.header.kind, "case");
    assert!(matches!(field(case, "expected").kind, ExprKind::Object(_)));
    let experiment = named_fixture_node(&ast, "routing_check");
    assert_eq!(experiment.header.kind, "experiment");
    for name in ["dataset", "task"] {
        assert!(matches!(field(experiment, name).kind, ExprKind::GraphRef { .. }));
    }
    let ExprKind::Array(scorers) = &field(experiment, "scorers").kind else {
        panic!("expected scorers")
    };
    assert!(matches!(
        scorers.as_slice(),
        [Expr {
            kind: ExprKind::GraphRef { .. },
            ..
        }]
    ));
}

#[test]
fn parses_public_traversal_paths() {
    assert_eq!(
        parse_traversal("trace[\"run\"].task[\"answer\"].output").unwrap(),
        ["trace", "run", "task", "answer", "output"]
    );
    assert!(parse_traversal("trace[0].output").is_err());
}

fn origin_node(origin: &GraphOrigin) -> &GraphNode {
    let GraphOrigin::Node(node) = origin else {
        panic!("expected node origin")
    };
    node
}

fn edge(step: &GraphStep) -> &bts_lang::syntax::GraphEdgeStep {
    let GraphStep::Edge(edge) = step else {
        panic!("expected edge")
    };
    edge
}

fn declaration(origin: &GraphOrigin) -> &GraphNodeDecl {
    node_declaration(origin_node(origin))
}

fn node_declaration(node: &GraphNode) -> &GraphNodeDecl {
    let GraphNode::Declaration(node) = node else {
        panic!("expected a declaration")
    };
    node
}

fn field<'a>(node: &'a GraphNodeDecl, key: &str) -> &'a Expr {
    &node
        .body
        .as_ref()
        .unwrap()
        .iter()
        .find(|field| field.key == key)
        .unwrap()
        .value
}

#[test]
fn separates_declarations_references_properties_and_bodies() {
    let source = r#"(lookup:node {name: "patient_lookup"}) {output: {found: true}};
        (:node); ("patient lookup":node) {}; ("patient lookup"); (lookup);"#;
    let graph = parse(source).unwrap();
    assert_eq!(graph.paths.len(), 5);
    let lookup = declaration(&graph.paths[0].start);
    assert_eq!(lookup.header.name.as_deref(), Some("lookup"));
    assert_eq!(lookup.header.kind, "node");
    assert_eq!(lookup.header.name_range, Some(SrcRange::new(1, 7)));
    assert_eq!(lookup.header.props.as_ref().unwrap()[0].key, "name");
    assert_eq!(lookup.body.as_ref().unwrap()[0].key, "output");
    assert!(declaration(&graph.paths[1].start).header.name.is_none());
    assert!(declaration(&graph.paths[1].start).body.is_none());
    assert_eq!(declaration(&graph.paths[2].start).body, Some(vec![]));
    let GraphNode::Reference(reference) = origin_node(&graph.paths[3].start) else {
        panic!("expected reference")
    };
    let ExprKind::GraphRef { kind, target } = &reference.kind else {
        panic!("expected graph reference")
    };
    assert_eq!(*kind, GraphReferenceKind::Node);
    assert!(matches!(&target.kind, ExprKind::Ref { path } if path == &["patient lookup"]));
}

#[test]
fn preserves_edge_lists_cycles_and_complete_source_ranges() {
    let source = "(a:node)-[advance:next {when: source.ready}, :feeds]->(b:node)~[retry:next]~>(a);";
    let graph = parse(source).unwrap();
    let path = &graph.paths[0];
    assert_eq!(path.range, SrcRange::new(0, source.len()));
    assert_eq!(path.steps.len(), 2);
    assert!(!edge(&path.steps[0]).cycle);
    assert!(edge(&path.steps[1]).cycle);
    assert_eq!(edge(&path.steps[0]).edges.len(), 2);
    assert_eq!(edge(&path.steps[0]).edges[0].name.as_deref(), Some("advance"));
    assert_eq!(edge(&path.steps[0]).edges[1].kind, "feeds");
    assert_eq!(
        node_declaration(&edge(&path.steps[0]).target).header.name.as_deref(),
        Some("b")
    );
    let range = edge(&path.steps[1]).range;
    assert_eq!(&source[range.start..range.end], "~[retry:next]~>(a)");
}

#[test]
fn keeps_graph_qualification_distinct_from_value_indexing() {
    let graph = parse("(refs:vars) {node: (lookup), edge: [found], selected: [checks].[coverage], indexed: [checks].paths[0][0]}; ([checks].[coverage].target)-[:next]->(done);").unwrap();
    let node = declaration(&graph.paths[0].start);
    assert!(matches!(
        field(node, "node").kind,
        ExprKind::GraphRef {
            kind: GraphReferenceKind::Node,
            ..
        }
    ));
    assert!(matches!(
        field(node, "edge").kind,
        ExprKind::GraphRef {
            kind: GraphReferenceKind::Edge,
            ..
        }
    ));
    assert!(matches!(&field(node, "selected").kind, ExprKind::GraphMember { name, .. } if name == "coverage"));
    assert!(matches!(field(node, "indexed").kind, ExprKind::Index { .. }));
    let GraphNode::Reference(reference) = origin_node(&graph.paths[1].start) else {
        panic!("expected reference")
    };
    let ExprKind::GraphRef { target, .. } = &reference.kind else {
        panic!("expected graph reference")
    };
    let ExprKind::Index { target, index } = &target.kind else {
        panic!("expected target accessor")
    };
    assert!(matches!(target.kind, ExprKind::GraphMember { .. }));
    assert!(matches!(&index.kind, ExprKind::Str(name) if name == "target"));
}

#[test]
fn reuses_precedence_with_explicit_expression_grouping() {
    let graph = parse("(values:vars) {grouped: ${1 + 2} * 3, plain: 1 + 2 * 3, reference: (x), empty: uuid()};").unwrap();
    let node = declaration(&graph.paths[0].start);
    let ExprKind::Binary { op: BinOp::Mul, lhs, .. } = &field(node, "grouped").kind else {
        panic!("expected multiply")
    };
    assert!(matches!(lhs.kind, ExprKind::Binary { op: BinOp::Add, .. }));
    let ExprKind::Binary { op: BinOp::Add, rhs, .. } = &field(node, "plain").kind else {
        panic!("expected add")
    };
    assert!(matches!(rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
    assert!(matches!(field(node, "reference").kind, ExprKind::GraphRef { .. }));
    assert!(matches!(&field(node, "empty").kind, ExprKind::Func { args, .. } if args.is_empty()));
}

#[test]
fn parses_arrays_comprehensions_spreads_and_full_expression_interpolation() {
    let source = r#"(values:vars) {
        empty: [||], one: [|1|], refs: [|[found], (lookup),|],
        spread: [|1, ...settings.values|],
        list: [|for x in settings.values: x + 1 if x > 0|],
        map: {for i, x in settings.values: x => i},
        object: {...settings, enabled: true},
        slice: settings.values[1:],
        text: "Mode ${ready ? "yes" : hex(8)}",
    };"#;
    let graph = parse(source).unwrap();
    let node = declaration(&graph.paths[0].start);
    assert!(matches!(&field(node, "empty").kind, ExprKind::Array(items) if items.is_empty()));
    assert!(matches!(&field(node, "one").kind, ExprKind::Array(items) if items.len() == 1));
    assert!(matches!(
        field(node, "list").kind,
        ExprKind::For {
            cond: Some(_),
            key: None,
            ..
        }
    ));
    assert!(matches!(field(node, "map").kind, ExprKind::For { key: Some(_), .. }));
    let ExprKind::Object(items) = &field(node, "object").kind else {
        panic!("expected object")
    };
    assert!(matches!(items[0], ObjectItem::Spread(_)));
    let ExprKind::Template(parts) = &field(node, "text").kind else {
        panic!("expected template")
    };
    assert!(matches!(&parts[1], TemplatePart::Ref { expr, .. } if matches!(expr.kind, ExprKind::Cond { .. })));
}

#[test]
fn rejects_declarations_in_values_and_content_on_references() {
    for source in [
        "(a) {output: true};",
        "(a {when: true});",
        "(a:node) {(b:node)};",
        "(a:node {value: (b:node)});",
        "(a:node {value: {nested: [|(:node)|]}});",
        "(a:node) {value: (:node)};",
        "(a:node) {value: [edge:next]};",
        "(a:node) {value: [checks].[edge:next]};",
        "(a:node) {value: (b)-[:next]->(c)};",
        "(a:node)-[:next]->();",
    ] {
        assert!(parse(source).is_err(), "unexpectedly accepted {source}");
    }
}

#[test]
fn requires_separators_and_typed_connections() {
    for source in [
        "(a:node)",
        "(a:node),",
        "(a:node) (b:node);",
        "(a:node) {one: 1 two: 2};",
        "(a:node) {one: 1\ntwo: 2};",
        "(a:node) {nested: {one: 1\ntwo: 2}};",
        "(a:node {one: 1\ntwo: 2});",
        "(a:node) {one = 1};",
        "(a:node) {items: [|1 2|]};",
        "(a:node) {items: [1, 2]};",
        "(a)-[next]->(b);",
        "(a)-[:next]~>(b);",
        "(a)~[:next]->(b);",
    ] {
        let errors = parse(source).expect_err(source);
        assert!(errors.iter().all(|error| error.when == DiagPhase::Parsing));
    }
}

#[test]
fn leaves_resolution_and_kind_contracts_for_semantic_validation() {
    let graph = parse("(unknown)-[:custom]->(:custom {anything: 1}); (same:node); (same:node);").unwrap();
    assert_eq!(graph.paths.len(), 3);
}

#[test]
fn points_missing_commas_at_the_next_field() {
    let source = "(a:vars) {one: 1\n two: 2};";
    let errors = parse(source).unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].what, "expected `,` between fields");
    let range = errors[0].r#where;
    assert_eq!(&source[range.start..range.end], "two");
}

#[test]
fn handles_truncated_graph_input_without_panicking() {
    let source = r#"(a:node {name: "lookup"}) {text: "Value ${hex(8)}", items: [|for x in xs: x|]}-[next:next]->(b);"#;
    for (end, _) in source.char_indices() {
        let _ = parse(&source[..end]);
    }
    assert!(parse(source).is_ok());
}

fn fork(step: &GraphStep) -> &bts_lang::syntax::GraphFork {
    let GraphStep::Fork(fork) = step else {
        panic!("expected fork")
    };
    fork
}

#[test]
fn attaches_nested_selectors_to_their_indentation_level() {
    let source = "(a)\n    =[outer:parallel]=>[\n        -[to_b:next]->(b),\n        -[to_d:next]->(d),\n    ]\n        ->(b)\n            =[inner:parallel]=>[-[to_c:next]->(c)]\n                ->(c)-[:next]->(e)\n        ->([outer].[to_d].target)-[:next]->(f);";
    let graph = parse(source).unwrap();
    let outer = fork(&graph.paths[0].steps[0]);
    assert_eq!(outer.continuations.len(), 2);
    let inner = fork(&outer.continuations[0].steps[0]);
    assert_eq!(inner.continuations.len(), 1);
    assert!(matches!(outer.continuations[1].start, GraphOrigin::Selector(_)));
    assert_eq!(outer.continuations[1].steps.len(), 1);
    let deeper = source.replace("\n        ->([outer]", "\n                ->([outer]");
    let graph = parse(&deeper).unwrap();
    let outer = fork(&graph.paths[0].steps[0]);
    assert_eq!(outer.continuations.len(), 1);
    let inner = fork(&outer.continuations[0].steps[0]);
    assert_eq!(inner.continuations.len(), 2);
}

#[test]
fn attaches_overlapping_junctions_as_siblings() {
    let source = "(a)=[:parallel]=>[\n    -[to_b:next]->(b),\n    -[to_c:next]->(c),\n    -[to_d:next]->(d),\n]\n    >[:accumulate [to_b], [to_c]]>>(e)\n    >[:accumulate [to_c], [to_d]]>>(f);";
    let graph = parse(source).unwrap();
    let fork = fork(&graph.paths[0].steps[0]);
    assert_eq!(fork.entries.len(), 3);
    assert_eq!(fork.continuations.len(), 2);
    for path in &fork.continuations {
        let GraphOrigin::Junction(junction) = &path.start else {
            panic!("expected junction")
        };
        assert_eq!(junction.inputs.len(), 2);
        assert!(path.steps.is_empty());
    }
}

#[test]
fn parses_root_junctions_with_qualified_inputs_and_following_edges() {
    let source =
        ">[:accumulate [checks].[coverage], [records],]>>(ready:node) {input: [records].scope}\n    -[:next]->(finish);";
    let graph = parse(source).unwrap();
    let GraphOrigin::Junction(junction) = &graph.paths[0].start else {
        panic!("expected junction")
    };
    assert_eq!(junction.inputs.len(), 2);
    assert!(matches!(junction.inputs[0].kind, ExprKind::GraphMember { .. }));
    assert_eq!(node_declaration(&junction.target).header.name.as_deref(), Some("ready"));
    assert_eq!(graph.paths[0].steps.len(), 1);
}

#[test]
fn preserves_mixed_sibling_continuations_and_cycle_selection() {
    let source = "(a)\n  =[checks:parallel]=>[-[lookup:next]->(:node), -[note:next]->(b)]\n    ->([lookup].target)~[:again]~>(a)\n    >[:accumulate [lookup], [note]]>>(done);";
    let graph = parse(source).unwrap();
    let fork = fork(&graph.paths[0].steps[0]);
    assert_eq!(fork.continuations.len(), 2);
    assert!(edge(&fork.continuations[0].steps[0]).cycle);
    assert!(matches!(fork.continuations[1].start, GraphOrigin::Junction(_)));
    assert!(matches!(fork.entries[0].target, GraphNode::Declaration(_)));
}

#[test]
fn allows_one_inline_selector_or_junction() {
    for source in [
        "(a)=[:parallel]=>[-[work:next]->(b)] ->(b)-[:next]->(c);",
        "(a)=[:parallel]=>[-[work:next]->(b)] >[:accumulate [work]]>>(c);",
        "(a)=[:parallel]=>[-[work:next]->(b)] ->(b)~[:again]~>(a);",
        "(a)=[:parallel]=>[-[work:next]->(b)] ->(b)=[:parallel]=>[-[:next]->(c)] ->(c)-[:next]->(d);",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
}

#[test]
fn anchors_graph_layout_to_declarations_not_body_contents() {
    let source = "(a:node) {\n    text: \"\"\"\n        ->(not_a_selector)\n    \"\"\",\n}\n  =[:parallel]=>[\n    -[to_b:next]->(b:node) {\n        nested: {items: [|1, 2|]},\n    },\n  ]\n    # this comment does not affect attachment\n    ->(b)-[:next]->(done);";
    let graph = parse(source).unwrap();
    let fork = fork(&graph.paths[0].steps[0]);
    assert_eq!(fork.continuations.len(), 1);
    assert_eq!(fork.entries.len(), 1);
}

#[test]
fn rejects_ambiguous_or_unattached_graph_continuations() {
    for source in [
        "(a)->(b);",
        "(a)=[:parallel]=>[-[:next]->(b)] ->(b) ->(b);",
        "(a)=[:parallel]=>[-[work:next]->(b)] ->(b) >[:accumulate [work]]>>(c);",
        "(a)=[:parallel]=>[-[work:next]->(b)] >[:accumulate [work]]>>(c) ->(b);",
        "(a)\n    =[:parallel]=>[-[:next]->(b)]\n    ->(b);",
        "(a)\n    =[:parallel]=>[-[:next]->(b)]\n        ->(b)\n      ->(b);",
        "(a)\n    =[:parallel]=>[-[:next]->(b)]\n        ->(b)\n          ->(b);",
        "(a)\n\t=[:parallel]=>[-[:next]->(b)];",
        "(a)=[:parallel]=>[-[:next]->(b)] ->(new:node);",
    ] {
        assert!(parse(source).is_err(), "unexpectedly accepted {source}");
    }
}

// every edge as its source and target node names, in declaration order
fn endpoints(source: &str) -> Vec<(String, String)> {
    let model = compiled(source);
    let name = |id: bts_lang::NodeId| model.nodes[id.0].name.clone().unwrap();
    model
        .edges
        .iter()
        .map(|edge| (name(edge.source), name(edge.target)))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(source, target)| (source.to_string(), target.to_string()))
        .collect()
}

#[test]
fn aligned_continuation_lines_share_an_origin() {
    let source = "(a:node)\n    -[:next]->(b:node)\n    -[:next]->(c:node)\n    -[:next]->(d:node);";
    let graph = parse(source).unwrap();
    assert_eq!(graph.paths[0].steps.len(), 1);
    let GraphStep::Arms(arms) = &graph.paths[0].steps[0] else {
        panic!("expected arms")
    };
    assert_eq!(arms.len(), 3);
    assert!(arms.iter().all(|arm| arm.len() == 1));
    assert_eq!(endpoints(source), pairs(&[("a", "b"), ("a", "c"), ("a", "d")]));
}

#[test]
fn deeper_continuation_lines_chain() {
    let source = "(a:node)\n    -[:next]->(b:node)\n        -[:next]->(c:node)\n            -[:next]->(d:node);";
    let graph = parse(source).unwrap();
    assert_eq!(graph.paths[0].steps.len(), 3);
    assert!(graph.paths[0].steps.iter().all(|step| matches!(step, GraphStep::Edge(_))));
    assert_eq!(endpoints(source), pairs(&[("a", "b"), ("b", "c"), ("c", "d")]));
}

#[test]
fn a_single_continuation_line_attaches_to_the_last_node_above() {
    assert_eq!(endpoints("(a:node)\n    -[:next]->(b:node);"), pairs(&[("a", "b")]));
    assert_eq!(
        endpoints("(a:node)-[:next]->(b:node)\n    -[:next]->(c:node);"),
        pairs(&[("a", "b"), ("b", "c")])
    );
    let graph = parse("(a:node)-[:next]->(b:node)\n    -[:next]->(c:node);").unwrap();
    assert_eq!(graph.paths[0].steps.len(), 2);
}

#[test]
fn mixes_fan_out_chains_bodies_and_loop_backs() {
    let source = "(a:node)\n    -[:next]->(b:node)-[:next]->(c:node)\n        -[:next]->(d:node)\n        ~[:next]~>(a)\n    -[:next]->(e:node) {\n        note: 1,\n    }\n    -[:next]->(f:node);";
    assert_eq!(
        endpoints(source),
        pairs(&[("a", "b"), ("b", "c"), ("c", "d"), ("c", "a"), ("a", "e"), ("a", "f")])
    );
    assert!(compiled(source).edges[3].cycle);
    // a step after a body on its closing line chains, and the body sets no level
    let source = "(a:node) {\n        n: 1,\n}-[:next]->(b:node)\n    -[:next]->(c:node)\n    -[:next]->(d:node);";
    assert_eq!(endpoints(source), pairs(&[("a", "b"), ("b", "c"), ("b", "d")]));
}

#[test]
fn fans_out_under_selectors_and_junctions() {
    let source = "(a:node)=[f:parallel]=>[-[p:next]->(b:node), -[q:next]->(c:node)]\n    ->(b)\n        -[:next]->(d:node)\n        -[:next]->(e:node)\n    >[:accumulate [p], [q]]>>(g:node)\n        -[:next]->(h:node)\n        -[:next]->(i:node);";
    assert_eq!(
        endpoints(source),
        pairs(&[("a", "b"), ("a", "c"), ("b", "d"), ("b", "e"), ("g", "h"), ("g", "i")])
    );
}

#[test]
fn a_fork_is_an_arm_beside_aligned_edges() {
    let source = "(a:node)\n    -[:link]->(b:node)\n    =[f:parallel]=>[-[p:next]->(c:node), -[q:next]->(d:node)]\n        ->(c)-[:next]->(e:node)\n    -[:link]->(g:node);";
    let graph = parse(source).unwrap();
    let GraphStep::Arms(arms) = &graph.paths[0].steps[0] else {
        panic!("expected arms")
    };
    assert_eq!(arms.len(), 3);
    assert_eq!(fork(&arms[1][0]).continuations.len(), 1);
    let model = compiled(source);
    assert_eq!(model.nodes[model.forks[0].source.0].name.as_deref(), Some("a"));
    assert_eq!(
        endpoints(source),
        pairs(&[("a", "b"), ("a", "c"), ("a", "d"), ("c", "e"), ("a", "g")])
    );
}

#[test]
fn rejects_inconsistent_continuation_indentation() {
    let indentation = "graph continuations require spaces and aligned sibling indentation";
    let selection = "select a fork target or add a junction before continuing the path";
    for (source, message) in [
        // a dedent to a level no earlier line established
        ("(a:node)\n        -[:next]->(b:node)\n    -[:next]->(c:node);", indentation),
        (
            "(a:node)\n    -[:next]->(b:node)\n        -[:next]->(d:node)\n      -[:next]->(c:node);",
            indentation,
        ),
        // a continuation line is deeper than the line it attaches to
        ("(a:node)\n-[:next]->(b:node);", indentation),
        ("  (a:node)\n    -[:next]->(b:node)\n-[:next]->(c:node);", indentation),
        // edge lines do not align with a fork's selectors and junctions
        (
            "(a:node)=[:parallel]=>[-[:next]->(b:node)]\n    ->(b)\n    -[:next]->(c:node);",
            selection,
        ),
        (
            "(a:node)\n    =[:parallel]=>[-[:next]->(b:node)]\n        ->(b)\n        -[:next]->(c:node);",
            selection,
        ),
        (
            "(a:node)\n    =[:parallel]=>[-[:next]->(b:node)]-[:next]->(c:node);",
            selection,
        ),
    ] {
        let errors = parse(source).expect_err(source);
        assert_eq!(errors[0].what, message, "{source}");
        // the diagnostic points at the offending line's first token
        assert_eq!(errors[0].r#where.start, source.rfind("-[").unwrap(), "{source}");
    }
}

#[test]
fn requires_single_steps_and_typed_forks_and_exact_junction_inputs() {
    for source in [
        "(a)=[parallel]=>[-[:next]->(b)];",
        "(a)=[:parallel]=>[-[:next]->(b)-[:next]->(c)];",
        "(a)=[:parallel]=>[-[:next]->(b) -[:next]->(c)];",
        "(a)=[:parallel]=>[-[:next]->(b)=[:parallel]=>[-[:next]->(c)]];",
        "(a)=[:parallel]=>[-[:next]->(b)]-[:next]->(c);",
        ">[:accumulate (a), (b)]>>(c);",
        ">[:accumulate [a:next], [b]]>>(c);",
        ">[:accumulate [a].target, [b]]>>(c);",
        ">[:accumulate [a], [b]];",
    ] {
        assert!(parse(source).is_err(), "unexpectedly accepted {source}");
    }
}

#[test]
fn preserves_fork_contracts_without_enforcing_runtime_policies() {
    for kind in ["parallel", "conditional", "exclusive", "custom"] {
        let source = format!("(a)=[choice:{kind} {{policy: true}}]=>[-[one:next {{when: ready}}]->(b), -[two:next]->(c)];");
        let graph = parse(&source).unwrap();
        let fork = fork(&graph.paths[0].steps[0]);
        assert_eq!(fork.header.kind, kind);
        assert_eq!(fork.header.name.as_deref(), Some("choice"));
        assert_eq!(fork.header.props.as_ref().unwrap().len(), 1);
        assert!(fork.continuations.is_empty());
    }
}

#[test]
fn parses_compilation_fixture() {
    let graph = parse(COMPILATION_SOURCE).unwrap();
    assert_eq!(graph.paths.len(), 63);
    let intake = &graph.paths[2];
    assert_eq!(intake.steps.len(), 3);
    let outer = fork(&intake.steps[2]);
    assert_eq!(outer.header.kind, "exclusive");
    assert_eq!(outer.continuations.len(), 1);
}

#[test]
fn public_expressions_use_the_same_syntax_as_node_fields() {
    for source in [
        "[|1, 2|]",
        "{key: 1}",
        "${1 + 2} * 3",
        "[checks].[coverage]",
        "(lookup)",
        "\"Result ${hex(8)}\"",
    ] {
        let expression = bts_lang::syntax::parse_expression(source).unwrap();
        let graph = parse(&format!("(values:vars) {{value: {source}}};")).unwrap();
        assert_eq!(
            std::mem::discriminant(&expression.kind),
            std::mem::discriminant(&field(declaration(&graph.paths[0].start), "value").kind),
        );
    }
    assert!(bts_lang::syntax::parse_expression("[1, 2]").is_err());
    assert!(bts_lang::syntax::parse_expression("{key = 1}").is_err());
}

#[test]
fn accepts_comments_blank_lines_and_crlf_between_sibling_selectors() {
    let source = "(a)\r\n  =[:parallel]=>[-[:next]->(b), -[:next]->(c)]\r\n    ->(b)-[:next]->(d)\r\n\r\n // comment\r\n    ->(c)-[:next]->(e);";
    let graph = parse(source).unwrap();
    assert_eq!(fork(&graph.paths[0].steps[0]).continuations.len(), 2);
}

#[test]
fn handles_truncated_forks_and_junctions_without_panicking() {
    let source = "(a)=[work:parallel]=>[-[one:next]->(b), -[two:next]->(c)]\n    ->(b)-[:next]->(d)\n    >[:accumulate [one], [two]]>>(finish);";
    for (end, _) in source.char_indices() {
        let _ = parse(&source[..end]);
    }
    assert!(parse(source).is_ok());
}

#[test]
fn pattern_references_select_nodes_or_edges_without_declarations() {
    use bts_lang::syntax::GraphPatternNode;
    let source = "(refs:vars) {node: ?<(collect)-[:next]->(@:node)>, edge: ?<(collect)-[@:next]->(lookup)>};";
    let graph = parse(source).unwrap();
    let refs = declaration(&graph.paths[0].start);
    let ExprKind::PatternRef(node) = &field(refs, "node").kind else {
        panic!("expected pattern")
    };
    assert!(matches!(node.start, GraphPatternNode::Reference(_)));
    let GraphPatternNode::Constraint(target) = &node.steps[0].target else {
        panic!("expected node constraint")
    };
    assert!(target.selected);
    assert_eq!(target.kind, "node");
    assert!(!node.steps[0].edges[0].selected);
    let ExprKind::PatternRef(edge) = &field(refs, "edge").kind else {
        panic!("expected pattern")
    };
    assert!(edge.steps[0].edges[0].selected);
    assert_eq!(edge.steps[0].edges[0].kind, "next");
    assert!(matches!(edge.steps[0].target, GraphPatternNode::Reference(_)));
    assert_eq!(&source[node.range.start..node.range.end], "?<(collect)-[:next]->(@:node)>");
}

#[test]
fn patterns_preserve_constraints_chains_edge_lists_and_cycles() {
    use bts_lang::syntax::GraphPatternNode;
    let source = "?<(@:node {name: \"start\"})-[:next {when: score > 1}, :feeds]->(:node {tags: [|\"a\"|]})~[:again]~>(start)>";
    let expr = bts_lang::syntax::parse_expression(source).unwrap();
    let ExprKind::PatternRef(pattern) = expr.kind else {
        panic!("expected pattern")
    };
    let GraphPatternNode::Constraint(start) = &pattern.start else {
        panic!("expected constraint")
    };
    assert!(start.selected);
    assert_eq!(start.props.as_ref().unwrap()[0].key, "name");
    assert_eq!(pattern.steps.len(), 2);
    assert_eq!(pattern.steps[0].edges.len(), 2);
    assert!(!pattern.steps[0].cycle);
    assert!(pattern.steps[1].cycle);
}

#[test]
fn pattern_references_work_in_endpoints_junctions_and_accessors() {
    let source = "(a)-[:next]->(?<(b)-[:next]->(@:node)>); >[:accumulate ?<(a)-[@:next]->(b)>, [other]]>>(done); (values:vars) {name: ?<(a)-[:next]->(@:node)>.props.name, text: \"Found ${?<(@:node)>.props.name}\"};";
    let graph = parse(source).unwrap();
    let GraphNode::Reference(expr) = &edge(&graph.paths[0].steps[0]).target else {
        panic!("expected reference")
    };
    let ExprKind::GraphRef { target, .. } = &expr.kind else {
        panic!("expected node reference")
    };
    assert!(matches!(target.kind, ExprKind::PatternRef(_)));
    let GraphOrigin::Junction(junction) = &graph.paths[1].start else {
        panic!("expected junction")
    };
    assert!(matches!(junction.inputs[0].kind, ExprKind::PatternRef(_)));
    assert!(matches!(
        field(declaration(&graph.paths[2].start), "name").kind,
        ExprKind::Index { .. }
    ));
}

#[test]
fn separates_pattern_closers_from_adjacent_expression_operators() {
    for (operator, op) in [("==", BinOp::Eq), ("!=", BinOp::Ne), (">", BinOp::Gt), (">=", BinOp::Ge)] {
        let source = format!("?<(@:node)>{operator}(lookup)");
        let expression = bts_lang::syntax::parse_expression(&source).unwrap();
        let ExprKind::Binary { op: actual, lhs, .. } = expression.kind else {
            panic!("expected binary expression")
        };
        assert_eq!(actual, op);
        assert!(matches!(lhs.kind, ExprKind::PatternRef(_)));
    }
    assert!(bts_lang::syntax::parse_expression("?<(@:node {text: \"> -> ~>\", nested: [|{ready: score >= 2}|]})>").is_ok());
}

#[test]
fn rejects_patterns_with_missing_or_multiple_selection_markers() {
    for source in [
        "?<(a)-[:next]->(:node)>",
        "?<(@:node)-[@:next]->(b)>",
        "?<(@:node)-[:next]->(@:node)>",
    ] {
        let errors = bts_lang::syntax::parse_expression(source).unwrap_err();
        assert!(
            errors.iter().any(|error| error.what.contains("exactly one `@`")),
            "{errors:?}"
        );
    }
}

#[test]
fn rejects_declarations_and_malformed_steps_in_patterns() {
    for source in [
        "?<(created:node)-[:next]->(@:node)>",
        "?<(a)-[created:next]->(@:node)>",
        "?<(@:node) {output: true}>",
        "?<(@:node {value: (created:node)})>",
        "?<(a)-[@next]->(b)>",
        "?<(a)-[@:next]~>(b)>",
        "?<(a)~[@:next]->(b)>",
        "?<>",
    ] {
        assert!(
            bts_lang::syntax::parse_expression(source).is_err(),
            "unexpectedly accepted {source}"
        );
    }
}

#[test]
fn handles_truncated_pattern_references_without_panicking() {
    let source = "?<(@:node {values: [|1, 2|], text: \"${hex(8)}\"})-[:next]->(done)>";
    for (end, _) in source.char_indices() {
        let _ = bts_lang::syntax::parse_expression(&source[..end]);
    }
    assert!(bts_lang::syntax::parse_expression(source).is_ok());
}

#[test]
fn lowers_the_compilation_fixture_to_a_domain_neutral_graph() {
    let model = compile_fixture();
    assert_eq!(model.paths.len(), 63);
    // the fixture's handoff edge supplies its key to both steps downstream
    let supplied = |node: &str| {
        let bts_lang::ElementId::Node(id) = model.names[node] else {
            panic!("{node}")
        };
        (model.context[id.0].iter())
            .find(|supply| supply.key == "assigned_by")
            .map(|supply| supply.suppliers.clone())
    };
    assert!(supplied("triage").is_none());
    assert!(matches!(supplied("assess").as_deref(), Some([bts_lang::ElementId::Edge(_)])));
    assert_eq!(supplied("assess"), supplied("discharge"));
    assert!(model.nodes.len() > 30);
    assert!(model.edges.len() > 30);
    assert!(model.forks.len() > 5);
    assert!(model.junctions.len() > 2);
    let traits = |kind: &str| -> Vec<&str> {
        model.kinds[model.kind_names[kind].0]
            .traits
            .iter()
            .map(|id| model.traits[id.0].name.as_str())
            .collect()
    };
    assert_eq!(traits("patient_export"), ["consented", "audited", "retained"]);
    assert_eq!(traits("audited_data"), ["audited"]);
}

// a node kind for tests that need a value only a consumer can supply
// it reads these keys itself and passes them to its edges, forks and junctions
const SUPPLIED: &str = r#"
+(supplied:node {open: true})
    -[:requires {member: {}, route: {}, flag: {}, decision: {}}]->(context)
    -[:provides {request_id: {}, route: {}, count: {}, weight: {}, items: {}, limit: {}, records: {}}]->(context);
"#;

fn compiled(source: &str) -> bts_lang::Model {
    compile(source).unwrap_or_else(|errors| panic!("{source}\n{errors:#?}"))
}

fn binding<'a>(model: &'a bts_lang::Model, node: &str, field: &str) -> &'a bts_lang::Binding {
    let owner = model.names[node];
    model
        .bindings
        .iter()
        .find(|binding| binding.owner == owner && !binding.property && binding.path == [field])
        .unwrap()
}

fn expression(source: &str) -> Value {
    let model = compiled(&format!("(values:vars) {{value: {source}}};"));
    binding(&model, "values", "value").value.clone()
}

#[test]
fn lowers_open_kinds_forward_references_and_identity_aliases() {
    let model = compiled("(a:node {custom: true}) {destination: (b)} -[advance:link]->(a.destination); (b:node) {value: 42};");
    let bts_lang::ElementId::Node(a) = model.names["a"] else {
        panic!()
    };
    let bts_lang::ElementId::Node(b) = model.names["b"] else {
        panic!()
    };
    assert_eq!(model.edges[0].source, a);
    assert_eq!(model.edges[0].target, b);
    let Value::GraphRef { ref_id, .. } = binding(&model, "a", "destination").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["b"]);
}

#[test]
fn reports_semantic_errors_at_valid_source_ranges() {
    for (source, message) in [
        ("(a:node); (a:node);", "duplicate graph name"),
        ("(a:node)-[a:next]->(:node);", "duplicate graph name"),
        ("(a:node) {value: 1, value: 2};", "duplicate field"),
        ("(a:node) {props: 1};", "reserved"),
        ("(missing);", "unresolved"),
        ("(a:node)-[e:next]->(:node); (e);", "boundary"),
        ("(a:node) {value: [a]};", "boundary"),
        ("(a:node) {value: self.missing};", "unknown field"),
        ("(a:node) {value: self.value};", "circular field dependency"),
        (
            "(a:node) {value: b.value}; (b:node) {value: a.value};",
            "circular field dependency",
        ),
        ("(a:node) {value: source.value};", "endpoint"),
        ("(a:vars {evaluate: \"sometimes\"});", "evaluate"),
        ("(a:node)-[:next {when: 1}]->(:node);", "boolean"),
        ("(a:node); (b:node) {value: a.output};", "unknown field"),
    ] {
        let errors = compile(source).expect_err(source);
        assert!(
            errors.iter().any(|error| error.what.contains(message)),
            "{source}\n{errors:#?}"
        );
        assert!(
            errors.iter().all(|error| error.when == DiagPhase::Validation
                && error.r#where.start <= error.r#where.end
                && error.r#where.end <= source.len()),
            "{errors:#?}"
        );
    }
    let source = "(first:node);\n(first:node);";
    let errors = compile(source).unwrap_err();
    assert_eq!(errors.len(), 2);
    assert!(
        errors
            .iter()
            .all(|error| &source[error.r#where.start..error.r#where.end] == "first")
    );
}

#[test]
fn retains_explicit_cycles_and_independent_field_dependencies() {
    let model = compiled("(a:node) {metrics: {base: 3, total: self.metrics.base + 2}} -[:next]->(b:node) ~[:next]~>(a);");
    assert!(!model.edges[0].cycle);
    assert!(model.edges[1].cycle);
    assert_eq!(model.edges[1].target, model.edges[0].source);
    let Value::Object(object) = &binding(&model, "a", "metrics").value else {
        panic!()
    };
    assert!(matches!(object.elem[1].value, Value::Num(bts_lang::Number::Int(5))));
}

#[test]
fn preserves_dynamic_binding_identity_through_aliases() {
    let model =
        compiled("(v:vars) {draw: range(1, 9), alias: self.draw, result: self.alias + 1, text: \"draw ${self.alias}\"};");
    let draw = model.bindings.iter().position(|item| item.path == ["draw"]).unwrap();
    assert!(matches!(binding(&model,"v","alias").value, Value::BindingRef(id, _) if id.0 == draw));
    assert!(matches!(binding(&model, "v", "result").value, Value::Binary { .. }));
    assert!(matches!(binding(&model, "v", "text").value, Value::Template(_)));
    compiled(
        "(v:vars) {draws: [|range(1,9), range(2,5)|], alias: self.draws, result: [|for x in self.alias: x + 1|], selected: self.alias[0] + 1};",
    );
}

#[test]
fn resolves_edge_context_against_explicit_endpoints() {
    let model = compiled(
        &[
            "+(summed:edge {sum: {type: \"number\"}})-[:extends]->(next); (:node) {value: 2} -[e:summed {sum: source.value + target.value}]->(:node) {value: 3}; (v:supplied) {scope: [e].scope.member, external: context.member};",
            SUPPLIED,
        ]
        .concat(),
    );
    let property = &model.bindings[model.edges[0].props[0].0].value;
    assert!(matches!(property, Value::Num(bts_lang::Number::Int(5))));
    assert!(matches!(
        binding(&model, "v", "scope").value,
        Value::Context {
            owner: bts_lang::ElementId::Edge(_),
            ..
        }
    ));
    assert!(matches!(
        binding(&model, "v", "external").value,
        Value::Context {
            owner: bts_lang::ElementId::Node(_),
            ..
        }
    ));
}

#[test]
fn lowers_overlapping_junctions_without_implicit_connections() {
    let model = compiled(
        "(a:node)\n    =[f:parallel]=>[-[eb:next]->(b:node), -[ec:next]->(c:node), -[ed:next]->(d:node)]\n        >[:accumulate [eb], [ec]]>>(e:node)\n        >[:accumulate [ec], [ed]]>>(g:node);",
    );
    assert_eq!(model.edges.len(), 3);
    assert_eq!(model.junctions.len(), 2);
    assert_eq!(model.junctions[0].inputs[1], model.junctions[1].inputs[0]);
    assert_ne!(model.junctions[0].target, model.junctions[1].target);
    assert_eq!(model.forks[0].continuations.len(), 2);
}

#[test]
fn enforces_fork_membership() {
    for source in [
        "(a:node) =[f:parallel]=>[-[eb:next]->(b:node)] ->(c); (c:node);",
        "(a:node) =[f:parallel]=>[-[eb:next]->(b:node)] >[:accumulate [ec]]>>(d:node); (c:node)-[ec:next]->(b);",
        "(a:node) =[f:parallel]=>[-[eb:next]->(b:node)]; (c:node)-[ec:next]->(b); (v:vars) {value: [f].[ec]};",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("(a:node) =[f:parallel]=>[-[eb:next]->(b:node)] ->([f].[eb].target)-[:next]->(c:node);");
}

#[test]
fn validates_graph_primitives_without_demanding_defaults() {
    for (kind, guards, valid) in [
        ("parallel", "-[:next]->(:node), -[:next]->(:node)", true),
        ("parallel", "-[:next {when: true}]->(:node)", false),
        (
            "conditional",
            "-[:next {when: true}]->(:node), -[:next {when: true}]->(:node)",
            true,
        ),
        (
            "exclusive",
            "-[:next {when: false}]->(:node), -[:next {when: false}]->(:node)",
            true,
        ),
        ("exclusive", "-[:next]->(:node), -[:next]->(:node)", false),
        (
            "exclusive",
            "-[:next {when: source.flag}]->(:node), -[:next {when: !source.flag}]->(:node)",
            true,
        ),
        (
            "exclusive",
            "-[:next {when: chance(0.5)}]->(:node), -[:next {when: !chance(0.5)}]->(:node)",
            false,
        ),
    ] {
        let source = format!("(a:node) {{flag: chance(0.5)}} =[:{kind}]=>[{guards}];");
        assert_eq!(compile(&source).is_ok(), valid, "{source}\n{:?}", compile(&source));
    }
}

#[test]
fn resolves_patterns_by_distinct_identity() {
    let model = compiled(
        "+(marked:edge {selected: {type: \"boolean\"}})-[:extends]->(next); (a:node)-[e:marked {selected: true}]->(b:node {selected: true}); (a)-[:next]->(b); (v:vars) {node: ?<(a)-[:next]->(@:node {selected: true})>, edge: ?<(a)-[@:marked {selected: true}]->(b)>};",
    );
    let Value::GraphRef { ref_id, .. } = binding(&model, "v", "node").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["b"]);
    for source in [
        "(a:node); (v:vars) {value: ?<(a)-[:next]->(@:node)>};",
        "(a:node)-[:next]->(b:node); (a)-[:next]->(c:node); (v:vars) {value: ?<(a)-[:next]->(@:node)>};",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn folds_arithmetic_collections_and_interpolation() {
    for (source, expected) in [
        ("1 + 2 * 3", 7),
        ("${1 + 2} * 3", 9),
        ("true ? 7 : 9", 7),
        ("[|2, 7, 9|][1]", 7),
        ("{x: 7}[\"x\"]", 7),
    ] {
        assert!(
            matches!(expression(source), Value::Num(bts_lang::Number::Int(value)) if value == expected),
            "{source}"
        );
    }
    assert!(matches!(expression("\"answer ${1 + 2}\""), Value::Str(value) if value == "answer 3"));
    for source in [
        "[|1, ...[|2, 3|]|]",
        "{...{a: 1}, b: 2}",
        "[|for x in [|1, 2|]: x * 2|]",
        "{for k, v in {a: 1}: k => v + 1}",
        "[|1, 2, 3|][1:3]",
    ] {
        expression(source);
    }
}

#[test]
fn retains_expression_validation_and_checked_arithmetic() {
    for source in [
        "9223372036854775807 + 1",
        "1 / 0",
        "1 % 0",
        "1e309",
        "[|1|][2]",
        "[|1|][-1]",
        "{x: 1}[\"missing\"]",
        "[|...1|]",
        "{...1}",
        "[|for x in 2: x|]",
        "{x: 1, x: 2}",
        "unknown_function()",
        "range(2, 1)",
        "chance(2)",
        "hex(-1)",
        "split(\"x\", \"\")",
        "clamp(3, 5, 1)",
        "noise(-1)",
        "1 + true",
        "\"${[|1|]}\"",
    ] {
        let module = format!("(v:vars) {{value: {source}}};");
        assert!(compile(&module).is_err(), "{source}");
    }
}

#[test]
fn compiles_spec_examples_and_parses_function_examples() {
    for example in bts_lang::spec::SPEC.examples {
        assert_eq!(compile(example.source).is_ok(), example.valid, "{}", example.id.as_str());
    }
    let settings = r#"(settings:vars) {
        model: "example", path: "a/b", tags: [|"prod"|], models: [|"example"|],
        prompt: "hello", temperature: 0.5, score: 0.5, limit: 10,
        intent: {}, speaker: {}, previous: {context: {}}, arrival: {},
        availability: {}, guest: {}, context: {},
    };"#;
    for function in bts_lang::spec::SPEC.functions {
        for example in function.examples {
            assert!(
                bts_lang::syntax::parse_expression(example).is_ok(),
                "{}: {example}",
                function.name
            );
            compiled(&format!("{settings} (example:vars) {{value: {example}}};"));
        }
    }
}

#[test]
fn shares_nested_dynamic_fields_without_resampling() {
    let model = compiled("(v:vars) {metrics: {base: range(1, 9), total: self.metrics.base + 2}, alias: self.metrics};");
    let calls = model
        .bindings
        .iter()
        .filter(|binding| matches!(binding.value, Value::Func { .. }))
        .count();
    assert_eq!(calls, 1);
    let Value::Object(object) = &binding(&model, "v", "metrics").value else {
        panic!()
    };
    assert!(matches!(object.elem[0].value, Value::BindingRef(_, _)));
}

#[test]
fn checks_known_dynamic_collection_indices_without_inlining_draws() {
    compiled(r#"(v:vars) {mixed: [|range(1,9), "text"|], value: self.mixed[0] + 1};"#);
    for index in ["3", "-1", "0.5"] {
        let source = format!("(v:vars) {{items: [|range(1,9)|], selected: self.items[{index}]}};");
        assert!(compile(&source).is_err(), "{source}");
    }
}

#[test]
fn preserves_function_result_types_and_argument_checks() {
    compiled(
        r#"(v:vars) {
        result: expression({}), alias: self.result,
        text: upper(self.alias.text), count: len(self.alias.used_facts),
        selected: self.alias.selection.intent,
    };"#,
    );
    for function in bts_lang::spec::SPEC.functions {
        if function.name != "uuid" {
            assert!(
                compile(&format!("(v:vars) {{value: {}()}};", function.name)).is_err(),
                "{}",
                function.name
            );
        }
    }
    for value in [
        "upper(1)",
        "range(true, false)",
        "weighted([|1, -2|])",
        "normal(0, -1)",
        "beta(0, 1)",
        "poisson(-1)",
        r#"format("{} {}", 1)"#,
        "uuid(1)",
    ] {
        assert!(compile(&format!("(v:vars) {{value: {value}}};")).is_err(), "{value}");
    }
}

#[test]
fn resolves_pattern_endpoints_without_unrelated_dependency_cycles() {
    let model = compiled("(a:node)-[:next]->(b:node); (c:node)-[:link]->(?<(a)-[:next]->(@:node)>);");
    assert_eq!(model.edges[0].target, model.edges[1].target);
}

#[test]
fn proves_complementary_comparisons_without_conflating_calls() {
    compiled(
        "(a:node) {n: range(1, 9)} =[:exclusive]=>[-[:next {when: source.n > 5}]->(:node), -[:next {when: !${source.n > 5}}]->(:node)];",
    );
}

#[test]
fn preserves_generic_compilation_policy_and_rule_inventory() {
    assert!(compile(r#"trace "old" {}"#).is_err());
    assert!(
        bts_lang::semantics::compile_module("# empty module", true)
            .unwrap()
            .nodes
            .is_empty()
    );
    assert!(bts_lang::semantics::compile_module("(missing);", true).is_err());
    let spec = &bts_lang::spec::SPEC;
    let mut ids = std::collections::HashSet::new();
    for rule in spec.rules {
        assert!(ids.insert(rule.id), "{}", rule.id.as_str());
        assert_eq!(spec.rule(rule.id), Some(rule));
    }
    for function in spec.functions {
        assert_eq!(spec.function(function.name), Some(function));
        for rule in function.rules {
            assert!(spec.rule(rule.id).is_some(), "{}", rule.id.as_str());
        }
    }
}

#[test]
fn retains_dependencies_of_dynamic_indices_and_slice_bounds() {
    compiled(
        "(v:vars) {items: [|10, 20, 30|], index: range(0, 1), selected: self.items[self.index] + 1, slice: self.items[self.index:2]};",
    );
    assert!(compile(r#"(v:vars) {items: [|1, 2|], index: choice("a", "b"), selected: self.items[self.index]};"#).is_err());
}

#[test]
fn reserves_contextual_names_without_reserving_field_keys() {
    for name in ["self", "source", "target", "context", "true", "false", "null"] {
        for source in [
            format!("({name}:task);"),
            format!("(\"{name}\":node);"),
            format!("(:node)-[{name}:next]->(:node);"),
            format!("(:node)=[{name}:parallel]=>[-[:next]->(:node)];"),
        ] {
            let errors = compile(&source).expect_err(&source);
            assert!(errors.iter().any(|error| error.what.contains("reserved graph name")));
        }
    }
    for name in ["self", "source", "target", "context"] {
        assert!(compile(&format!("(a:vars) {{values: [|for {name} in [|1|]: {name}|]}};")).is_err());
    }
    compiled("(a:node) {source: 1, target: 2, context: 3, self: 4};");
}

#[test]
fn resolves_graph_identities_through_collections_and_static_expressions() {
    let model = compiled(
        r#"(a:node) {value: 7}; (b:node);
        (refs:vars) {
            nodes: [|(a), (b)|],
            alias: self.nodes,
            mixed: [|(a), range(1, 9)|],
            nested: [|[|(a), range(1, 9)|]|],
            selected: self.alias[0].value,
            nested_value: self.nested[0][0].value,
        };
        (refs.alias[0])-[:next]->(refs.mixed[0]);
        (true ? refs.nodes[0] : refs.nodes[1])-[:next]->([|(b)|][0]);"#,
    );
    let a = model.names["a"];
    assert_eq!(bts_lang::ElementId::Node(model.edges[0].source), a);
    assert_eq!(bts_lang::ElementId::Node(model.edges[0].target), a);
    assert_eq!(bts_lang::ElementId::Node(model.edges[1].source), a);
    assert_eq!(bts_lang::ElementId::Node(model.edges[1].target), model.names["b"]);
    assert!(matches!(
        binding(&model, "refs", "selected").value,
        Value::Num(bts_lang::Number::Int(7))
    ));
    assert!(matches!(
        binding(&model, "refs", "nested_value").value,
        Value::Num(bts_lang::Number::Int(7))
    ));
    assert!(compile("(a:node); (v:vars) {nodes: [|(a)|], index: range(0, 1)}; (v.nodes[v.index]);").is_err());
}

#[test]
fn resolves_edge_expressions_and_stored_fork_paths() {
    let model = compiled(
        r#"+(limited:edge {limit: {type: "number"}})-[:extends]->(next);
        (a:node) =[split:parallel]=>[-[input_left:limited {limit: 3}]->(:node), -[input_right:next]->(:node)];
        (refs:vars) {paths: [split].paths, edges: [|[input_left], [input_right]|], alias: self.edges[0]};
        (refs.paths[0][0].target)-[follow:limited {limit: [refs.alias].props.limit}]->(b:node);
        >[:accumulate [refs.alias], [refs.edges[1]]]>>(b);"#,
    );
    assert_eq!(model.edges[2].source, model.edges[0].target);
    assert_eq!(model.junctions[0].inputs, [bts_lang::EdgeId(0), bts_lang::EdgeId(1)]);
    assert!(matches!(
        model.bindings[model.edges[2].props[0].0].value,
        Value::Num(bts_lang::Number::Int(3))
    ));
    for source in [
        "(a:node); (v:vars) {node: (a), wrong: [self.node]};",
        "(a:node)-[edge:next]->(b:node); (v:vars) {edge: [edge]}; (v.edge);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn keeps_junction_properties_separate_from_destination_properties() {
    let source = &[
        r#"(config:vars) {policy: "all"};
        (a:supplied) =[f:parallel]=>[-[input_left:next]->(b:node), -[input_right:next]->(c:node)]
            >[:gathered {mode: config.policy, correlate: context.request_id} [input_left], [input_right]]>>(d:node {accumulate: "each"});
        >[:custom {custom: [input_right].target} [input_left]]>>(d);
        +(gathered:junction {mode: {type: "string"}})-[:extends]->(accumulate);
        +(custom:junction {custom: {type: "any"}});"#,
        SUPPLIED,
    ]
    .concat();
    let ast = parse(source).unwrap();
    let GraphOrigin::Junction(junction) = &fork(&ast.paths[1].steps[0]).continuations[0].start else {
        panic!()
    };
    assert_eq!(junction.props.as_ref().unwrap().len(), 2);
    let range = junction.props.as_ref().unwrap()[0].range;
    assert_eq!(&source[range.start..range.end], "mode: config.policy");
    let model = compiled(source);
    let props = &model.junctions[0].props;
    assert_eq!(props.len(), 2);
    assert_eq!(
        model.bindings[props[0].0].owner,
        bts_lang::ElementId::Junction(bts_lang::JunctionId(0))
    );
    assert!(matches!(&model.bindings[props[0].0].value, Value::Str(value) if value == "all"));
    assert!(
        matches!(&model.bindings[props[1].0].value, Value::Context {owner: bts_lang::ElementId::Junction(id), path} if id.0 == 0 && path == &["request_id"])
    );
    let target = &model.nodes[model.junctions[0].target.0];
    assert!(matches!(&model.bindings[target.props[0].0].value, Value::Str(value) if value == "each"));
    assert_eq!(model.junctions[1].props.len(), 1);
    for props in [
        "{limit: 1, limit: 2}",
        "{limit: config.missing}",
        "{limit: upper(2)}",
        "{limit: (bad:node)}",
    ] {
        let source = format!("(a:node)-[edge:next]->(b:node); >[:accumulate {props} [edge]]>>(c:node);");
        assert!(compile(&source).is_err(), "{source}");
    }
}

#[test]
fn cycle_edges_keep_independent_kinds_properties_and_references() {
    let model = compiled(
        r#"+(attempted:edge {max_attempts: {type: "number"}})-[:extends]->(next);
        (settings:vars) {limit: 3};
        (a:node)-[advance:next]->(b:node)
            ~[retry:attempted {max_attempts: settings.limit}, feedback:feeds {value: source.props}]~>(a);
        (refs:vars) {retry_edge: [retry], limit: [retry].props.max_attempts};"#,
    );
    assert_eq!(model.edges.len(), 3);
    assert!(!model.edges[0].cycle);
    assert!(model.edges[1].cycle && model.edges[2].cycle);
    assert_eq!(model.edges[1].kind, "attempted");
    assert_eq!(model.edges[2].kind, "feeds");
    assert_ne!(model.edges[1].props, model.edges[2].props);
    assert!(matches!(
        binding(&model, "refs", "limit").value,
        Value::Num(bts_lang::Number::Int(3))
    ));
}

#[test]
fn matches_parallel_edges_without_enumerating_permutations() {
    let headers = (0..16).map(|i| format!("edge_{i}:link")).collect::<Vec<_>>().join(",");
    let constraints = vec![":link"; 16].join(",");
    let source = format!("(a:node)-[{headers}]->(b:node); (refs:vars) {{selected: ?<(a)-[{constraints}]->(@:node)>}};");
    let model = compiled(&source);
    let Value::GraphRef { ref_id, .. } = binding(&model, "refs", "selected").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["b"]);

    compiled(
        "+(tagged:edge {special: {type: \"boolean\"}})-[:extends]->(link); (a:node)-[one:tagged {special: true}, two:link]->(b:node); (v:vars) {selected: ?<(a)-[:link, @:tagged {special: true}]->(b)>};",
    );
    for source in [
        "(a:node)-[one:link, two:link]->(b:node); (v:vars) {selected: ?<(a)-[@:link, :link]->(b)>};",
        "(a:node)-[one:link]->(b:node); (v:vars) {selected: ?<(a)-[:link, :link]->(@:node)>};",
        "(a:node)-[one:link {p: true}, two:link, three:link]->(b:node); (v:vars) {selected: ?<(a)-[:link {p: true}, :link {p: true}, :link]->(@:node)>};",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn proves_common_exclusive_conditions_and_preserves_independent_draws() {
    for (left, right, valid) in [
        ("source.flag", "!source.alias", true),
        ("source.route == \"billing\"", "source.route == \"appointments\"", true),
        ("\"billing\" == source.route", "source.route != \"billing\"", true),
        ("source.n < 5", "source.n >= 5", true),
        ("source.n > 5", "source.n <= 5", true),
        ("source.n == 1", "source.n == 2", true),
        (
            "source.flag && source.route == \"billing\"",
            "source.alias && source.route == \"appointments\"",
            true,
        ),
        ("source.n <= 5", "source.n >= 5", false),
        ("source.route != \"billing\"", "source.route != \"appointments\"", false),
        ("source.flag", "source.alias", false),
        ("chance(0.5)", "!chance(0.5)", false),
        ("source.draw == 1", "source.other_draw == 2", false),
        ("context.route == \"billing\"", "context.route == \"appointments\"", false),
    ] {
        let source = format!(
            r#"(a:supplied) {{flag: chance(0.5), alias: self.flag, route: context.route, n: range(0, 10), draw: range(1, 2), other_draw: range(1, 2)}}
            =[:exclusive]=>[-[:next {{when: {left}}}]->(:node), -[:next {{when: {right}}}]->(:node)];
            {SUPPLIED}"#
        );
        assert_eq!(compile(&source).is_ok(), valid, "{source}\n{:?}", compile(&source));
    }
}

#[test]
fn requires_typed_junctions_with_enclosed_properties() {
    for source in [
        ">[[input_left], [input_right]]>>(target);",
        ">[[input_left], [input_right]] {mode: \"all\"}->(target);",
        ">[:accumulate [input_left], [input_right]] {mode: \"all\"}->(target);",
        ">[:accumulate {}]>>(target);",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    let model = compiled(
        "+(custom:junction {policy: {type: \"number\"}}); (a:node)-[edge:link]->(b:node); >[:custom {policy: 1} [edge]]>>(b);",
    );
    assert_eq!(model.junctions[0].kind, "custom");
    assert_eq!(model.junctions[0].props.len(), 1);
}

#[test]
fn registers_edge_primitives_and_requires_defined_consumer_kinds() {
    for kind in ["link", "next", "requires", "contains", "feeds"] {
        let primitive = bts_lang::spec::SPEC.primitive("edge", kind).unwrap();
        assert_eq!(primitive.name, kind);
        let model = compiled(&format!("(a:node)-[:{kind}]->(b:node);"));
        assert_eq!(model.edges[0].kind, kind);
    }
    assert!(bts_lang::spec::SPEC.primitive("edge", "parallel").is_none());
    let consumer = "(a:node)-[:consumer_kind {custom: 1}]->(b:node);";
    assert!(compile(consumer).is_err());
    compiled(&format!("+(consumer_kind:edge {{custom: {{type: \"number\"}}}}); {consumer}"));
}

#[test]
fn validates_builtin_junction_configuration() {
    let definitions = "(a:node)-[input_left:link]->(b:node); (a)-[input_right:link]->(c:node);";
    for kind in ["accumulate", "merge", "first", "quorum"] {
        assert!(bts_lang::spec::SPEC.primitive("junction", kind).is_some());
        let props = if kind == "quorum" { "{count: 2}" } else { "{}" };
        let model = compiled(&format!(
            "{definitions} >[:{kind} {props} [input_left], [input_right]]>>(d:node);"
        ));
        assert_eq!(model.junctions[0].kind, kind);
        assert!(
            compile(&format!(
                "{definitions} >[:{kind} {props} [input_left], [input_left]]>>(d:node);"
            ))
            .is_err()
        );
    }
    for props in [
        "{}",
        "{count: 0}",
        "{count: 3}",
        "{count: 1.5}",
        "{count: context.count}",
        "{count: range(1, 2)}",
    ] {
        assert!(
            compile(&format!(
                "{definitions} >[:quorum {props} [input_left], [input_right]]>>(d:node);"
            ))
            .is_err()
        );
    }
    compiled(&format!(
        "{definitions} (config:vars) {{count: 1 + 1}}; >[:quorum {{count: config.count}} [input_left], [input_right]]>>(d:node);"
    ));
}

#[test]
fn fixture_lowers_kind_schemas_upstream_inputs_and_destinations() {
    use bts_lang::{ElementId, FieldType};
    let ast = parse(COMPILATION_SOURCE).unwrap();
    assert_eq!(ast.definitions.len(), 19);
    assert_eq!(ast.definitions[0].head.header.kind, "trait");
    assert_eq!(ast.definitions[3].head.header.kind, "node");
    assert_eq!(ast.definitions[4].head.header.kind, "edge");
    assert_eq!(ast.definitions[5].head.header.kind, "fork");
    assert_eq!(ast.definitions[6].head.header.kind, "junction");
    let marker = field(node_declaration(&edge(&ast.paths[37].steps[0]).target), "patient_id");
    assert!(matches!(marker.kind, ExprKind::Upstream));
    assert_eq!(&COMPILATION_SOURCE[marker.range.start..marker.range.end], "<-");
    let model = compile_fixture();
    assert_eq!(binding(&model, "patient_request", "patient_id").ty, FieldType::String);
    assert!(matches!(
        binding(&model, "patient_request", "description").value,
        Value::Template(_)
    ));
    let span = model.kind_names["tool_span"];
    let lookup = &model.kinds[model.kind_names["patient_lookup"].0];
    assert_eq!(lookup.base_kind, Some(span));
    assert!(
        lookup
            .fields
            .iter()
            .any(|field| field.name == "patient_id" && field.required && field.ty == FieldType::String)
    );
    assert!(matches!(
        binding(&model, "patient_step", "patient_id").value,
        Value::Input {
            ty: FieldType::String,
            ..
        }
    ));
    assert!(matches!(binding(&model, "patient_step", "metadata").value, Value::Object(_)));
    assert!(matches!(binding(&model, "patient_step", "note").value, Value::Str(_)));
    let ElementId::Edge(delivery) = model.names["patient_delivery"] else {
        panic!()
    };
    let into = model.edges[delivery.0]
        .props
        .iter()
        .map(|id| &model.bindings[id.0])
        .find(|binding| binding.path == ["into"])
        .unwrap();
    let Value::FieldRef(input) = into.value else { panic!() };
    assert_eq!(model.bindings[input.0].owner, model.names["patient_step"]);
    assert_eq!(model.bindings[input.0].path, ["patient_id"]);
    assert!(
        model
            .kinds
            .iter()
            .any(|kind| kind.name == "patient_data" && kind.primitive == "feeds")
    );
    assert!(model.junctions.iter().any(|junction| junction.kind == "patient_join"));
}

#[test]
fn resolves_forward_kind_bases_and_validates_specialization() {
    let model = compiled(
        r#"+(child:node) {value: {required: true}}-[:extends]->(base);
        +(base:node {name: {type: "string", default: "example"}}) {value: {type: "number"}};
        (a:child) {value: 2};"#,
    );
    assert_eq!(model.kinds[0].base_kind, Some(model.kind_names["base"]));
    assert!(
        bts_lang::semantics::compile_module("+(only:node);", true)
            .unwrap()
            .kinds
            .len()
            == 1
    );
    for source in [
        "+(a:node)-[:extends]->(b); +(b:node)-[:extends]->(a);",
        "+(a:node)-[:extends]->(missing);",
        "+(a:node); +(a:node);",
        "+(node:node);",
        "+(self:node);",
        "+(a:node)-[:extends]->(feeds);",
        "+(a:edge)-[:extends]->(node);",
        "+(base:node); (:base)-[:base]->(:node);",
        "+(base:node) {v: {type: \"string\", required: true}}; +(child:node) {v: {required: false}}-[:extends]->(base);",
        "+(base:node) {v: {type: \"string\"}}; +(child:node) {v: {type: \"number\"}}-[:extends]->(base);",
        "+(base:node) {v: {type: \"any\", default: \"text\"}}; +(child:node) {v: {type: \"number\"}}-[:extends]->(base);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn keeps_properties_strict_and_fields_open_with_typed_defaults() {
    let schema = r#"+(record:node {open: true, name: {type: "string", required: true}, enabled: {type: "boolean", default: true}}) {value: {type: "number", required: true}, fallback: {type: "object", default: {}}};"#;
    compiled(&format!(
        "{schema} (a:record {{name: \"a\"}}) {{value: 2, arbitrary: [|1, 2|]}};"
    ));
    for instance in [
        "(a:record) {value: 2};",
        "(a:record {name: 2}) {value: 2};",
        "(a:record {name: \"a\", typo: true}) {value: 2};",
        "(a:record {name: \"a\"});",
        "(a:record {name: \"a\"}) {value: \"wrong\"};",
        "(a:record {name: \"a\"}) {value: choice(\"wrong\")};",
    ] {
        assert!(compile(&format!("{schema} {instance}")).is_err(), "{instance}");
    }
    for schema in [
        "+(bad:node {v: {type: \"imaginary\"}});",
        "+(bad:node {v: {type: \"number\", default: \"wrong\"}});",
        "+(bad:node) {v: {default: range(1, 2)}};",
        "+(bad:node) {props: {}};",
        "+(bad:node) {v: {typo: true}};",
    ] {
        assert!(compile(schema).is_err(), "{schema}");
    }
}

#[test]
fn feeds_references_input_destinations_without_reading_them() {
    let schema = "+(receiver:node) {id: {type: \"string\", required: true}, upper: {type: \"string\"}};";
    let model = compiled(&format!(
        "{schema} (a:node) {{id: \"p-1\"}} -[:feeds {{value: source.id, into: target.id}}]->(b:receiver) {{id: <-, upper: upper(self.id)}};"
    ));
    assert!(matches!(binding(&model, "b", "upper").value, Value::Func { .. }));
    for path in [
        "(a:node)-[:feeds {value: 1, into: target.id}]->(b:receiver) {id: <-};",
        "(a:node)-[:feeds {value: \"p-1\", into: target.id}]->(b:receiver) {id: \"fixed\"};",
        "(a:node)-[:feeds {value: \"p-1\", into: target.missing}]->(b:receiver) {id: <-};",
        "(a:node) {id: <-} -[:feeds {value: \"p-1\", into: source.id}]->(b:receiver) {id: <-};",
        "(a:node)-[:feeds {into: target.id}]->(b:receiver) {id: <-};",
        "(a:node)-[:feeds {value: \"p-1\", into: \"id\"}]->(b:receiver) {id: <-};",
        "(b:receiver) {id: <-, wrong: self.id + 1};",
        "(a:node {config: <-});",
        "(a:node) {nested: {input: <-}};",
    ] {
        assert!(compile(&format!("{schema} {path}")).is_err(), "{path}");
    }
    assert!(bts_lang::syntax::parse_expression("<-").is_err());
}

#[test]
fn specialized_primitives_keep_their_builtin_validation() {
    for source in [
        "+(choice:fork)-[:extends]->(exclusive); (:node)=[:choice]=>[-[:next]->(:node), -[:next]->(:node)];",
        "+(all:fork)-[:extends]->(parallel); (:node)=[:all]=>[-[:next {when: true}]->(:node)];",
        "+(some:junction)-[:extends]->(quorum); (:node)-[e:link]->(:node); >[:some {count: 2} [e]]>>(:node);",
        "+(values:node)-[:extends]->(vars); (:values {evaluate: \"sometimes\"});",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("+(joined:junction)-[:extends]->(accumulate); (:node)-[e:link]->(:node); >[:joined [e]]>>(:node);");
}

#[test]
fn rejects_invalid_kind_placements_and_handles_truncated_declarations() {
    let sources = [
        "+(span:node) {input: {type: \"string\"}};",
        "+(payload:edge {label: {type: \"string\"}})-[:extends]->(feeds);",
        "+(split:fork)-[:extends]->(parallel);",
        "+(joined:junction)-[:extends]->(accumulate);",
        "(a:node) {id: <-};",
    ];
    for source in sources {
        for end in source.char_indices().map(|(index, _)| index) {
            let prefix = &source[..end];
            assert!(std::panic::catch_unwind(|| parse(prefix)).is_ok(), "{prefix}");
        }
    }
    for source in [
        "(a:node) {kind: +(span:node)};",
        "+(span:node); (a:span {name: <-});",
        "+(span:node) {input: {default: <-}};",
        "+(span: :node {});",
        "+(span:node) {extra: 1};",
        "+(span:node); (a:span) {values: [|<-|]};",
        "+(span:node); (a:span) {input: <- + 1};",
        ">>[:accumulate [e]]->(a);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn upstream_requirements_suppress_defaults_and_allow_alternative_suppliers() {
    let model = compiled(
        r#"+(receiver:node {open: true}) {id: {type: "string", default: "fallback"}};
        (b:receiver) {id: <-, text: "Patient ${self.id}"};
        (a:node)-[:feeds {value: "first", into: target.id}]->(b);
        (c:node)-[:feeds {value: "second", into: target.id}]->(b);
        (d:receiver);"#,
    );
    assert!(matches!(&binding(&model, "d", "id").value, Value::Str(value) if value == "fallback"));
    assert!(matches!(binding(&model, "b", "id").value, Value::Input { .. }));
    let destinations: Vec<_> = model
        .edges
        .iter()
        .map(|edge| {
            edge.props
                .iter()
                .find_map(|id| match model.bindings[id.0].value {
                    Value::FieldRef(destination) => Some(destination),
                    _ => None,
                })
                .unwrap()
        })
        .collect();
    assert_eq!(destinations[0], destinations[1]);
    assert!(matches!(binding(&model, "b", "text").value, Value::Template(_)));
}

#[test]
fn preserves_schema_types_through_context_and_aliases() {
    let schema = r#"+(record:node {open: true, label: {type: "string"}}) {id: {type: "string"}}-[:requires {label: {}, id: {}, flag: {}}]->(context);"#;
    let model = compiled(&format!(
        r#"{schema} (a:record {{label: context.label}}) {{id: context.id, alias: self.id, text: "${{self.alias}} ${{self.props.label}}"}};"#
    ));
    assert_eq!(binding(&model, "a", "id").ty, bts_lang::FieldType::String);
    assert!(matches!(binding(&model, "a", "id").value, Value::Context { .. }));
    for expression in ["self.id + 1", "self.alias + 1", "self.props.label + 1"] {
        let source =
            format!("{schema} (a:record {{label: context.label}}) {{id: context.id, alias: self.id, bad: {expression}}};");
        let errors = compile(&source).unwrap_err();
        assert!(
            errors.iter().any(|error| error.what.contains("number operands")),
            "{errors:?}"
        );
    }
    let source = format!(
        r#"{schema} +(receiver:node) {{id: {{type: "number"}}}};
        (a:record) {{id: context.id}} -[:feeds {{value: source.id, into: target.id}}]->(b:receiver) {{id: <-}};"#
    );
    assert!(
        compile(&source)
            .unwrap_err()
            .iter()
            .any(|error| error.what.contains("number operands"))
    );
}

#[test]
fn checks_every_possible_conditional_type() {
    let schema = r#"+(record:node) {id: {type: "string"}}-[:requires {flag: {}, value: {}, other: {}, id: {}}]->(context);"#;
    for value in [
        "context.flag ? \"yes\" : 42",
        "context.flag ? 42 : \"yes\"",
        "context.flag ? context.value : 42",
        "context.flag ? \"yes\" : context.other ? \"no\" : 42",
    ] {
        let errors = compile(&format!("{schema} (a:record) {{id: {value}}};")).unwrap_err();
        assert!(
            errors.iter().any(|error| error.what.contains("string operands")),
            "{errors:?}"
        );
    }
    for guard in ["context.flag ? true : 1", "source.guard"] {
        let source = format!(
            "(a:supplied) {{guard: context.flag ? true : 1}} =[:conditional]=>[-[:next {{when: {guard}}}]->(b:node)]; {SUPPLIED}"
        );
        assert!(
            compile(&source)
                .unwrap_err()
                .iter()
                .any(|error| error.what.contains("boolean operands"))
        );
    }
    compiled(&format!("{schema} (a:record) {{id: context.flag ? \"yes\" : context.id}};"));
    compiled(&format!("{schema} (a:record) {{id: true ? \"yes\" : 42}};"));
}

#[test]
fn requires_identifier_kind_names_for_every_construct() {
    for construct in ["node", "edge", "fork", "junction", "trait -[:on]->(node)"] {
        let (construct, relationship) = construct.split_once(' ').unwrap_or((construct, ""));
        compiled(&format!("+(patient_kind:{construct}){relationship};"));
        for name in ["\"patient span\"", "\"patient_kind\""] {
            let errors = parse(&format!("+({name}:{construct}){relationship};")).unwrap_err();
            assert!(errors.iter().all(|error| error.when == DiagPhase::Parsing));
        }
    }
    for source in [
        "+(span);",
        "+(:node);",
        "+>[:accumulate [e]]>>(a:node);",
        "+-[a:link]-;",
        "+(a::node {});",
    ] {
        let errors = parse(source).unwrap_err();
        assert!(errors.iter().all(|error| error.when == DiagPhase::Parsing), "{source}");
    }
}

#[test]
fn undirected_edges_match_in_reverse_without_changing_endpoint_identity() {
    use bts_lang::{Direction, ElementId};
    let source = r#"
        (a:node) {value: 2}; (b:node) {value: 3};
        (a)-[ab:summed {sum: left.value + right.value}]-(b);
        +(summed:edge {sum: {type: "number"}})-[:extends]->(link);
        (refs:node) {edge: ?<(b)-[@:summed]-(a)>, l: [ab].left, r: [ab].right};
    "#;
    let ast = parse(source).unwrap();
    assert_eq!(edge(&ast.paths[2].steps[0]).direction, Direction::Undirected);
    let model = compiled(source);
    assert_eq!(model.edges[0].direction, Direction::Undirected);
    let Value::GraphRef { ref_id, .. } = binding(&model, "refs", "edge").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["ab"]);
    for (field, endpoint) in [("l", "a"), ("r", "b")] {
        let Value::GraphRef { ref_id, .. } = binding(&model, "refs", field).value else {
            panic!()
        };
        assert_eq!(model.refs[ref_id.0].target, model.names[endpoint]);
    }
    assert_eq!(model.names["ab"], ElementId::Edge(bts_lang::EdgeId(0)));
    for wrong in ["[ab].source", "[ab].target"] {
        assert!(compile(&format!("{source} (bad:node) {{value: {wrong}}};")).is_err());
    }
    assert!(compile(&format!("{source} (bad:node) {{value: ?<(a)-[@:link]->(b)>}};")).is_err());
    compiled("(a:node)-[:link]-(a); (a)~[:link]~>(a);");
    assert!(parse("(a:node)~[:link]~(a);").is_err());
    assert!(compile("(a:node)-[:link {value: source} ]-(b:node);").is_err());
    assert!(compile("(a:node)-[:link {value: left} ]->(b:node);").is_err());
}

#[test]
fn inherits_and_narrows_edge_direction_allowances() {
    use bts_lang::Directions;
    let model = compiled(
        r#"
        +(friendship:edge {direction: "undirected"})-[:extends]->(link);
        +(close_friend:edge)-[:extends]->(friendship);
        (a:node)-[:close_friend]-(b:node);
        (a)-[:link]-(b); (a)-[:link]->(b);
    "#,
    );
    assert_eq!(
        model.kinds[model.kind_names["close_friend"].0].direction,
        Some(Directions::Undirected)
    );
    for source in [
        "+(bad:edge {direction: \"either\"})-[:extends]->(next);",
        "+(base:edge {direction: \"undirected\"})-[:extends]->(link); +(bad:edge {direction: \"directed\"})-[:extends]->(base);",
        "+(bad:node {direction: \"either\"});",
        "+(bad:edge {direction: \"sideways\"})-[:extends]->(link);",
        "(a:node)-[:next]-(b:node);",
        "(a:node)-[:feeds]-(b:node);",
        "+(friendship:edge {direction: \"undirected\"})-[:extends]->(link); (a:node)~[:friendship]~>(a);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("(a:node) =[:parallel]=>[-[ab:link]-(b:node)] ->(b)-[:next]->(c:node);");
    compiled("(a:node) =[:parallel]=>[-[ab:link]-(b:node)] >[:accumulate [ab]]>>(c:node);");
}

#[test]
fn validates_explicit_network_membership_and_preserves_runtime_obligations() {
    use bts_lang::ValidationStage;
    let definitions = "(a:node)-[ab:link]->(b:node); (c:node);";
    for properties in [
        "nodes: [|(a)|], edges: [|[ab]|]",
        "nodes: [|(a), (b), (a)|], edges: [|[ab]|]",
        "nodes: [|(a), (b)|], edges: [|[ab], [ab]|]",
        "nodes: [|[ab]|], edges: [||]",
        "nodes: [|(a), (b)|], edges: [|(a)|]",
        "nodes: [|(a)|]",
        "nodes: context.nodes, edges: [||]",
        "nodes: [|(a)|], edges: [||], connected: \"maybe\"",
        "nodes: [|(a)|], edges: [||], incoming: {max: -1}",
        "nodes: [|(a)|], edges: [||], degree: {min: 2, max: 1}",
        "nodes: [|(a)|], edges: [||], outgoing: {limit: 1}",
        "nodes: [|(a)|], edges: [||], validate: \"later\"",
        "nodes: [|(a)|], edges: [||], self_loops: 1",
        "nodes: [|(a)|], edges: [||], max_parallel: 1.5",
        "nodes: [|(a)|], edges: [||], unknown: true",
    ] {
        let source = format!("{definitions} (g:network {{{properties}}});");
        assert!(compile(&source).is_err(), "{source}");
    }
    for (stage, valid) in [("definition", false), ("both", false), ("materialized", true)] {
        let source = format!(
            "{definitions} (g:network {{nodes: [|(a), (b), (c)|], edges: [|[ab]|], connected: \"weak\", validate: \"{stage}\"}});"
        );
        let result = compile(&source);
        assert_eq!(result.is_ok(), valid, "{result:?}");
        if let Ok(model) = result {
            assert_eq!(model.networks[0].validate, ValidationStage::Materialized);
        }
    }
    let model = compiled(&format!(
        "{definitions} (g:network {{nodes: [|(a), (b), (c)|], edges: [|[ab]|]}});"
    ));
    assert_eq!(model.networks[0].nodes.len(), 3);
    assert_eq!(model.networks[0].edges.len(), 1);
}

#[test]
fn checks_network_cycles_connectivity_and_multigraph_counts() {
    for (edges, rule, valid) in [
        (
            "(a)-[ab:link]->(b); (b)-[bc:link]->(c);",
            "acyclic: \"directed\", connected: \"weak\", incoming: {max: 1}",
            true,
        ),
        ("(a)-[ab:link]->(b); (b)-[bc:link]->(c);", "connected: \"strong\"", false),
        (
            "(a)-[ab:link]-(b); (b)-[bc:link]-(c);",
            "acyclic: \"underlying\", connected: \"strong\", degree: {min: 1, max: 2}",
            true,
        ),
        ("(a)-[ab:link]-(b); (b)-[bc:link]-(c);", "acyclic: \"directed\"", false),
        ("(a)-[ab:link]->(b); (b)~[bc:link]~>(a);", "acyclic: \"directed\"", false),
        ("(a)-[ab:link]->(b); (a)-[bc:link]->(b);", "acyclic: \"directed\"", true),
        ("(a)-[ab:link]->(b); (a)-[bc:link]->(b);", "acyclic: \"underlying\"", false),
        ("(a)-[ab:link]-(b); (b)-[bc:link]-(a);", "max_parallel: 1", false),
        ("(a)-[ab:link]->(b); (b)-[bc:link]->(a);", "max_parallel: 1", true),
        ("(a)-[ab:link]->(b); (a)-[bc:link]-(b);", "max_parallel: 1", true),
        ("(a)-[ab:link]->(c); (b)-[bc:link]->(c);", "incoming: {max: 1}", false),
        ("(a)-[ab:link]->(b); (a)-[bc:link]->(c);", "outgoing: {max: 1}", false),
        ("(a)-[ab:link]-(a); (b)-[bc:link]-(c);", "self_loops: false", false),
        ("(a)-[ab:link]-(a); (b)-[bc:link]-(c);", "degree: {max: 1}", false),
        ("(a)-[ab:link]-(a); (b)-[bc:link]-(c);", "degree: {max: 2}", true),
        (
            "(a)-[ab:link]->(a); (b)-[bc:link]->(c);",
            "incoming: {max: 1}, outgoing: {max: 1}",
            true,
        ),
    ] {
        let source = format!(
            "(a:node); (b:node); (c:node); {edges} (g:network {{nodes: [|(a), (b), (c)|], edges: [|[ab], [bc]|], {rule}}});"
        );
        assert_eq!(compile(&source).is_ok(), valid, "{source}");
    }
    for connected in ["weak", "strong"] {
        assert!(
            compile(&format!(
                "(g:network {{nodes: [||], edges: [||], connected: \"{connected}\"}});"
            ))
            .is_err()
        );
        compiled(&format!(
            "(a:node); (g:network {{nodes: [|(a)|], edges: [||], connected: \"{connected}\"}});"
        ));
    }
    compiled(
        "(a:node)-[ab:contains]->(b:node)~[:next]~>(a); (g:network {nodes: [|(a), (b)|], edges: [|[ab]|], acyclic: \"directed\"});",
    );
}

#[test]
fn relation_incidence_roles_preserve_hyperedge_identity() {
    compiled(
        r#"
        +(conversion:node {label: {type: "string"}})-[:extends]->(relation);
        +(participant:edge)-[:extends]->(incidence);
        (r:conversion {label: "convert", inputs: {min: 2}, outputs: {min: 1}, participants: {max: 3}});
        (a:node)-[:participant {role: "input"}]-(r);
        (r)-[:participant {role: "input"}]-(b:node);
        (r)-[:participant {role: "output"}]-(c:node);
    "#,
    );
    compiled("(r:relation); (r)-[:incidence]-(a:node); (s:relation); (s)-[:incidence]-(a);");
    for source in [
        "(a:node)-[:incidence]-(b:node);",
        "(a:relation)-[:incidence]-(b:relation);",
        "(r:relation)-[:incidence]->(a:node);",
        "(r:relation)-[:incidence {role: \"input\"}]-(a:node);",
        "(r:relation)-[:incidence {role: \"bad\"}]-(a:node);",
        "(r:relation)-[:incidence]-(a:node); (r)-[:incidence]-(a);",
        "(r:relation {participants: {min: 1}});",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("(r:relation {unique: false})-[:incidence]-(a:node); (r)-[:incidence]-(a);");
}

#[test]
fn weighted_each_and_reduce_validate_configuration_without_executing() {
    for (props, weights, valid) in [
        ("", "3", true),
        ("count: 1", "3", false),
        ("count: 2", "3", false),
        ("", "-1", false),
        ("", "0", true),
        ("", "\"heavy\"", false),
        ("", "context.weight", true),
    ] {
        let source = format!(
            "(a:supplied) =[:weighted {{{props}}}]=>[-[:next {{weight: {weights}}}]->(b:node), -[:next {{weight: 1}}]->(c:node)]; {SUPPLIED}"
        );
        assert_eq!(compile(&source).is_ok(), valid, "{source}");
    }
    compiled("(a:node) =[:weighted]=>[-[:link {weight: 2}, :link {weight: 2}]-(b:node)];");
    compiled("(a:node) =[:weighted]=>[-[:next {weight: 0, when: false}]->(b:node)];");
    compiled(&format!(
        "(a:supplied) =[:each {{items: context.items, concurrency: context.limit}}]=>[-[:next]->(b:node)]; {SUPPLIED}"
    ));
    assert!(compile("(a:node) =[:weighted]=>[-[:next]->(b:node)];").is_err());
    assert!(compile("(a:node) =[:weighted]=>[-[:link {weight: 1}, :link {weight: 2}]-(b:node)];").is_err());
    for props in ["", "{items: 1}", "{items: [||], concurrency: 0}"] {
        assert!(compile(&format!("(a:node) =[:each {props}]=>[-[:next]->(b:node)];")).is_err());
    }
    let source = &[
        r#"
        +(batch:fork)-[:extends]->(each); +(sum:junction)-[:extends]->(reduce);
        (a:supplied) =[work:batch {items: context.records}]=>[-[done:next]->(b:node) {item: context.item, index: context.index}]
            >[:sum {observe: "result", initial: 0, combine: context.accumulator + context.arrival.value, on_failure: "skip"} [done]]>>(c:node);
    "#,
        SUPPLIED,
    ]
    .concat();
    let model = compiled(source);
    assert_eq!(model.nodes.len(), 3);
    assert_eq!(model.forks[0].kind, "batch");
    assert_eq!(model.junctions[0].kind, "sum");
    for props in [
        "initial: 0",
        "combine: context.value",
        "initial: 0, combine: 1, observe: \"node\"",
        "initial: 0, combine: 1, on_failure: \"ignore\"",
        "initial: 0, combine: 1, order: \"random\"",
    ] {
        let source = format!("(a:node)-[ab:next]->(b:node); >[:reduce {{{props}}} [ab]]>>(c:node);");
        assert!(compile(&source).is_err(), "{source}");
    }
}

#[test]
fn fixture_covers_networks_hyperrelations_and_runtime_kind_contracts() {
    let model = compile_fixture();
    assert_eq!(model.networks.len(), 2);
    assert_eq!(model.networks[0].acyclic, bts_lang::Acyclic::Underlying);
    assert_eq!(model.networks[1].validate, bts_lang::ValidationStage::Materialized);
    assert!(
        model
            .edges
            .iter()
            .any(|edge| edge.direction == bts_lang::Direction::Undirected)
    );
    for kind in ["weighted", "each"] {
        assert!(model.forks.iter().any(|fork| fork.kind == kind));
    }
    assert!(model.junctions.iter().any(|junction| junction.kind == "reduce"));
    assert_eq!(model.edges.iter().filter(|edge| edge.kind == "incidence").count(), 3);
}

#[test]
fn provider_definitions_extend_and_preserve_source_identity() {
    let model = compile_fixture();
    assert_eq!(model.sources, ["compilation.bt", "braintrust.bt"]);
    let span = &model.kinds[model.kind_names["span"].0];
    let tool = &model.kinds[model.kind_names["tool_span"].0];
    let local = &model.kinds[model.kind_names["patient_lookup"].0];
    assert_eq!(tool.base_kind, Some(model.kind_names["span"]));
    assert_eq!(local.base_kind, Some(model.kind_names["tool_span"]));
    assert_eq!(span.range.source, 1);
    assert_eq!(tool.range.source, 1);
    assert_eq!(local.range.source, 0);
    assert!(PROVIDER_SOURCE[span.range.start..span.range.end].starts_with("+(span:node"));
    assert!(COMPILATION_SOURCE[local.range.start..local.range.end].starts_with("+(patient_lookup:node"));
    let metadata = local.fields.iter().find(|field| field.name == "metadata").unwrap();
    assert_eq!(metadata.range.source, 1);
}

#[test]
fn supplied_kinds_cover_every_construct_and_cross_source_forward_inheritance() {
    let definitions = [
        Source {
            name: "derived.bt",
            text: "+(tool_span:node)-[:extends]->(span); +(delivery:edge)-[:extends]->(feeds); +(split:fork)-[:extends]->(parallel); +(join:junction)-[:extends]->(accumulate);",
        },
        Source {
            name: "base.bt",
            text: "+(span:node) {id: {type: \"string\"}};",
        },
    ];
    let source = Source {
        name: "user.bt",
        text: r#"
        +(special:node)-[:extends]->(tool_span);
        (a:special) {id: "p-1"}
            =[branches:split]=>[
                -[one:delivery {value: source.id, into: target.id}]->(b:tool_span) {id: <-},
                -[two:next]->(c:span)
            ]
                >[:join [one], [two]]>>(d:span);
    "#,
    };
    let model = compile_with_definitions(source, &definitions).unwrap();
    assert_eq!(model.kinds[model.kind_names["special"].0].primitive, "node");
    assert_eq!(model.kinds[model.kind_names["delivery"].0].primitive, "feeds");
    assert_eq!(model.kinds[model.kind_names["split"].0].primitive, "parallel");
    assert_eq!(model.kinds[model.kind_names["join"].0].primitive, "accumulate");
    assert_eq!(model.kinds[model.kind_names["span"].0].range.source, 2);
}

#[test]
fn provider_conflicts_and_inheritance_errors_use_the_existing_rules() {
    for (user, first, second, message, source_index) in [
        ("+(span:node);", "+(span:node);", "", "duplicate kind", 0),
        ("(a:node);", "+(span:node);", "+(span:node);", "duplicate kind", 2),
        ("(a:node);", "+(node:node);", "", "reserved", 1),
        ("(a:node);", "+(span:node)-[:extends]->(missing);", "", "unknown base", 1),
        (
            "(a:node);",
            "+(span:node)-[:extends]->(tool_span);",
            "+(tool_span:node)-[:extends]->(span);",
            "circular",
            1,
        ),
        (
            "(a:node);",
            "+(span:node)-[:extends]->(delivery);",
            "+(delivery:edge)-[:extends]->(link);",
            "crosses",
            1,
        ),
        (
            "+(local:node {name: {required: false}})-[:extends]->(span);",
            "+(span:node {name: {type: \"string\", required: true}});",
            "",
            "required",
            0,
        ),
    ] {
        let errors = compile_with_definitions(
            Source {
                name: "user.bt",
                text: user,
            },
            &[
                Source {
                    name: "first.bt",
                    text: first,
                },
                Source {
                    name: "second.bt",
                    text: second,
                },
            ],
        )
        .unwrap_err();
        assert!(errors[0].what.contains(message), "{errors:?}");
        assert_eq!(errors[0].r#where.source, source_index, "{errors:?}");
    }
}

#[test]
fn supplied_sources_report_local_ranges_in_every_phase() {
    for (text, phase) in [
        ("# é\n`", DiagPhase::Lexing),
        ("# é\n+(span:node {", DiagPhase::Parsing),
        ("# é\n+(span:node) {id: {type: \"unknown\"}};", DiagPhase::Validation),
        ("# é\n(a:node);", DiagPhase::Validation),
    ] {
        let error = compile_with_definitions(
            Source {
                name: "user.bt",
                text: "(a:node);",
            },
            &[Source {
                name: "provider.bt",
                text,
            }],
        )
        .unwrap_err()
        .remove(0);
        assert_eq!(error.when, phase);
        assert_eq!(error.r#where.source, 1);
        assert!(error.r#where.end <= text.len());
        assert!(error.render("provider.bt", text).starts_with("provider.bt:2:"));
    }
    for text in ["(a:span);", "(a:span {name: 1});", "(a:span {name: \"ok\", extra: true});"] {
        let error = compile_with_definitions(
            Source { name: "user.bt", text },
            &[Source {
                name: "provider.bt",
                text: PROVIDER_SOURCE,
            }],
        )
        .unwrap_err()
        .remove(0);
        assert_eq!(error.r#where.source, 0);
        assert!(error.r#where.end <= text.len());
        assert_eq!(error.when, DiagPhase::Validation);
    }
}

#[test]
fn empty_user_modules_do_not_bypass_provider_validation() {
    let source = Source {
        name: "empty.bt",
        text: "# empty",
    };
    let definitions = [Source {
        name: "provider.bt",
        text: PROVIDER_SOURCE,
    }];
    assert!(compile_with_definitions(source, &definitions).is_err());
    let model = compile_module_with_definitions(source, &definitions, true).unwrap();
    assert!(model.nodes.is_empty());
    assert_eq!(model.kinds.len(), 12);
    assert!(
        compile_module_with_definitions(
            source,
            &[Source {
                name: "invalid.bt",
                text: "+(span:node)-[:extends]->(missing);",
            }],
            true
        )
        .is_err()
    );
    let model = compile_module_with_definitions(source, &[], true).unwrap();
    assert_eq!(model.sources, ["empty.bt"]);
    assert!(model.kinds.is_empty());
    let model = compile_with_definitions(
        Source {
            name: "user.bt",
            text: "(a:node);",
        },
        &[],
    )
    .unwrap();
    assert_eq!(model.sources, ["user.bt"]);
    assert_eq!(model.nodes[0].range.source, 0);
}

#[test]
fn input_roles_preserve_fallbacks_without_folding_caller_reads() {
    let model = compile_fixture();
    let binding = |field: &str| {
        model
            .bindings
            .iter()
            .find(|binding| {
                let bts_lang::ElementId::Node(node) = binding.owner else {
                    return false;
                };
                model.nodes[node.0].name.as_deref() == Some("caller_values") && binding.path == [field]
            })
            .unwrap()
    };
    assert!(matches!(binding("payload").value, Value::Object(_)));
    assert!(matches!(binding("observed").value, Value::BindingRef(_, _)));
    assert!(matches!(binding("selected").value, Value::Cond { .. }));

    let source = r#"
+(payload:node {open: true}) {input: {type: "number", role: "input"}};
(a:payload) {input: 1, first: a.input, second: a.input};
"#;
    let model = bts_lang::semantics::compile(source).unwrap();
    let reads: Vec<_> = model
        .bindings
        .iter()
        .filter_map(|binding| {
            if matches!(binding.path.first().map(String::as_str), Some("first" | "second"))
                && let Value::BindingRef(id, range) = binding.value
            {
                return Some((id, range));
            }
            None
        })
        .collect();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].0, reads[1].0);
    assert_ne!(reads[0].1, reads[1].1);
    for (_, range) in reads {
        assert_eq!(&source[range.start..range.end], "a.input");
    }

    let source = r#"
+(flags:node) {first: {type: "boolean", role: "input"}, second: {type: "boolean", role: "input"}};
(a:flags) {first: true, second: false}
    =[:exclusive]=>[
        -[:next {when: a.first}]->(b:node),
        -[:next {when: a.second}]->(c:node),
    ];
"#;
    assert!(compile(source).is_err(), "fallbacks cannot prove caller inputs disjoint");
}

#[test]
fn repeat_forks_require_nonnegative_integer_counts() {
    for source in [
        "(a:node)=[:repeat {count: 0}]=>[-[:next]->(b:node)];",
        "(a:supplied)=[:repeat {count: context.count}]=>[-[:next]->(b:node)];",
    ] {
        compiled(&[source, SUPPLIED].concat());
    }
    for source in [
        "(a:node)=[:repeat]=>[-[:next]->(b:node)];",
        "(a:node)=[:repeat {count: -1}]=>[-[:next]->(b:node)];",
        "(a:node)=[:repeat {count: 1.5}]=>[-[:next]->(b:node)];",
        "(a:node)=[:repeat {count: true}]=>[-[:next]->(b:node)];",
        "(a:node)=[:repeat {count: 2}]=>[-[:next {when: true}]->(b:node)];",
    ] {
        assert!(bts_lang::semantics::compile(source).is_err(), "{source}");
    }
}

#[test]
fn cycle_traversal_limits_belong_to_cycle_edges() {
    for limit in ["0", "3", "context.limit"] {
        compiled(&format!("(a:supplied)~[:next {{max_traversals: {limit}}}]~>(a); {SUPPLIED}"));
    }
    for limit in ["-1", "1.5", "true", "\"three\""] {
        assert!(compile(&format!("(a:node)~[:next {{max_traversals: {limit}}}]~>(a);")).is_err());
    }
    assert!(compile("(a:node)-[:next {max_traversals: 3}]->(b:node);").is_err());
}

#[test]
fn fork_branches_preserve_cycle_identity_and_bundle_configuration() {
    let model = compiled(
        &[
            r#"
(a:supplied) {output: context.decision};
(b:node) {input: <-};
(a) =[:exclusive]=>[
    ~[retry:next {when: source.output.retry, max_traversals: 3},
      carried:feeds {when: source.output.retry, value: source.output.value, into: target.input}]~>(b),
    -[done:next {when: !source.output.retry}]->(finish:node),
];
"#,
            SUPPLIED,
        ]
        .concat(),
    );
    let fork = &model.forks[0];
    assert_eq!(fork.branches[0].len(), 2);
    assert!(fork.branches[0].iter().all(|edge| model.edges[edge.0].cycle));
    assert!(!model.edges[fork.branches[1][0].0].cycle);
    assert_eq!(
        model.edges[fork.branches[0][0].0].target,
        model.edges[fork.branches[0][1].0].target
    );
}

#[test]
fn reports_every_independent_mistake_at_its_own_location_without_cascades() {
    let source = r#"
(start:node) {value: 1};
(start:node) {value: 2};
(start)-[:next]->(a:node) {seen: missing.value, again: self.seen};
(a)-[:next {when: 5}]->(b:node);
(b)-[:next]->(nowhere)-[:next]->(c:node);
(c)=[:parallel]=>[-[one:next]->(d:node), -[two:next]->(e:node)]
    >[:quorum {count: 9} [one], [ghost]]>>(f:node);
"#;
    let errors = compile(source).unwrap_err();
    let reported: Vec<_> = errors
        .iter()
        .map(|error| (&source[error.r#where.start..error.r#where.end], error.what.as_str()))
        .collect();
    let expected = [
        // a duplicate name, with a note on the first declaration
        ("start", "duplicate graph name `start`"),
        ("start", "previous declaration of `start`"),
        // an unresolved endpoint, reported once for the two edges that share it
        ("nowhere", "unresolved graph name `nowhere`"),
        // a bad junction input, without a quorum count error for the unresolved junction
        ("ghost", "unresolved graph name `ghost`"),
        // an unresolved field reference, reported once although `again` reads the same field
        ("missing.value", "unresolved graph name `missing`"),
        // a guard with the wrong type
        ("when: 5", "expects boolean operands"),
    ];
    assert_eq!(reported.len(), expected.len(), "{reported:#?}");
    for ((text, what), (expected_text, expected_what)) in reported.iter().zip(expected) {
        assert_eq!(*text, expected_text, "{reported:#?}");
        assert!(what.contains(expected_what), "{reported:#?}");
    }
    assert!(errors.iter().all(|error| error.when == DiagPhase::Validation));
}

#[test]
fn reports_every_broken_kind_declaration_without_checking_its_instances() {
    let source = r#"
+(orphan:node)-[:extends]->(nothing);
+(typo:node {a: {type: "strin"}, b: {required: 1}});
+(derived:node)-[:extends]->(orphan);
(x:derived {anything: 1});
(y:typo {anything: 1});
"#;
    let errors = compile(source).unwrap_err();
    let reported: Vec<_> = errors.iter().map(|error| error.what.as_str()).collect();
    assert_eq!(
        reported,
        [
            "unknown base kind `nothing`",
            "unknown schema value type",
            "required must be a literal boolean",
        ]
    );
}

fn located(source: &str) -> Vec<(String, String)> {
    compile(source)
        .unwrap_err()
        .into_iter()
        .map(|diag| (diag.what, source[diag.r#where.start..diag.r#where.end].to_owned()))
        .collect()
}

#[test]
fn a_kind_has_its_traits_and_those_of_its_base() {
    use bts_lang::{Construct, FieldType};
    let model = compiled(
        r#"
        +(metered:trait {unit: {type: "string", default: "ms"}}) {metrics: {type: "object"}}
            -[:on]->(node);
        +(audited:trait {auditor: {type: "string"}})
            -[:on]->(node)
            -[:on]->(edge);
        +(span:node) {input: {type: "any"}}
            -[:has]->(metered);
        +(tool:node {service: {type: "string"}}) {metrics: {required: true}, shown: {type: "string"}}
            -[:extends]->(span)
            -[:has]->(audited)
            -[:has]->(metered);
        +(reviewed:edge)
            -[:extends]->(link)
            -[:has]->(audited);
        (a:tool {service: "billing", auditor: "ops"}) {metrics: {tokens: 3}, shown: self.props.unit}
            -[:reviewed {auditor: "ops"}]->(b:span);
        "#,
    );
    let names = |kind: &str| -> Vec<&str> {
        model.kinds[model.kind_names[kind].0]
            .traits
            .iter()
            .map(|id| model.traits[id.0].name.as_str())
            .collect()
    };
    assert_eq!(names("span"), ["metered"]);
    assert_eq!(names("tool"), ["metered", "audited"]);
    assert_eq!(names("reviewed"), ["audited"]);
    let audited = &model.traits[model.trait_names["audited"].0];
    assert_eq!(audited.on, [Construct::Node, Construct::Edge]);
    assert!(!model.kind_names.contains_key("metered"));
    let tool = &model.kinds[model.kind_names["tool"].0];
    let metrics = tool.fields.iter().find(|field| field.name == "metrics").unwrap();
    assert_eq!(metrics.ty, FieldType::Object);
    assert!(metrics.required);
    assert!(tool.props.iter().any(|field| field.name == "unit"));
    assert!(matches!(&binding(&model, "a", "shown").value, Value::Str(unit) if unit == "ms"));
    // trait properties are strict like any declared property, and trait fields are typed
    for source in [
        "+(m:trait {unit: {type: \"string\"}})-[:on]->(node); +(k:node)-[:has]->(m); (a:k {typo: 1});",
        "+(m:trait) {metrics: {type: \"object\"}}-[:on]->(node); +(k:node)-[:has]->(m); (a:k) {metrics: 1};",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn a_trait_extends_one_trait_and_narrows_where_it_applies() {
    use bts_lang::Construct;
    let model = compiled(
        r#"
        +(metered:trait {unit: {type: "string"}})
            -[:on]->(node)
            -[:on]->(edge);
        +(billed:trait {unit: {required: true}}) {cost: {type: "number"}}
            -[:extends]->(metered)
            -[:on]->(node);
        +(timed:trait)-[:extends]->(metered);
        +(base:node)-[:has]->(metered);
        +(priced:node)
            -[:extends]->(base)
            -[:has]->(billed);
        (a:priced {unit: "usd"}) {cost: 2};
        "#,
    );
    let billed = &model.traits[model.trait_names["billed"].0];
    assert_eq!(billed.base, Some(model.trait_names["metered"]));
    assert_eq!(billed.on, [Construct::Node]);
    assert_eq!(
        model.traits[model.trait_names["timed"].0].on,
        [Construct::Node, Construct::Edge]
    );
    let priced = &model.kinds[model.kind_names["priced"].0];
    assert_eq!(priced.traits, [model.trait_names["metered"], model.trait_names["billed"]]);
    assert!(priced.props.iter().any(|field| field.name == "unit" && field.required));
    assert_eq!(priced.props.iter().filter(|field| field.name == "unit").count(), 1);
    for (source, message, at) in [
        (
            "+(m:trait)-[:on]->(node); +(b:trait)\n -[:extends]->(m)\n -[:on]->(fork);",
            "trait `m` is not on fork",
            "-[:on]->(fork)",
        ),
        (
            "+(m:trait)-[:on]->(node); +(b:trait)\n -[:on]->(node)\n -[:has]->(m);",
            "cannot have traits",
            "-[:has]->(m)",
        ),
        (
            "+(k:node); +(b:trait)-[:extends]->(k);",
            "can only extend a trait",
            "-[:extends]->(k)",
        ),
        (
            "+(b:trait)-[:extends]->(missing);",
            "unknown trait `missing`",
            "-[:extends]->(missing)",
        ),
        (
            "+(a:trait)-[:extends]->(b); +(b:trait)-[:extends]->(a);",
            "circular trait extension",
            "+(a:trait)-[:extends]->(b);",
        ),
        (
            "+(m:trait)-[:on]->(node); +(n:trait)-[:on]->(node); +(b:trait)\n -[:extends]->(m)\n -[:extends]->(n);",
            "at most one trait",
            "-[:extends]->(n)",
        ),
        (
            "+(m:trait {unit: {type: \"string\"}});",
            "at least one `on`",
            "+(m:trait {unit: {type: \"string\"}});",
        ),
        ("+(m:trait)-[:on]->(span);", "`on` targets a construct", "-[:on]->(span)"),
        (
            "+(m:trait) {metrics: {type: \"object\"}}\n -[:on]->(node)\n -[:on]->(edge);",
            "only nodes have content fields",
            "+(m:trait) {metrics: {type: \"object\"}}\n -[:on]->(node)\n -[:on]->(edge);",
        ),
        ("+(m:trait)-[:on]->(node); (a:m);", "`m` is a trait, not a kind", "m"),
        ("+(m:trait)-[:on]->(node); +(m:node);", "duplicate kind or trait name", "m"),
        ("+(trait:node);", "reserved", "trait"),
    ] {
        let errors = located(source);
        assert!(
            errors.iter().any(|(what, text)| what.contains(message) && text == at),
            "{source}\n{errors:#?}"
        );
    }
}

#[test]
fn locates_definition_relationship_errors() {
    let traits =
        "+(m:trait) {metrics: {type: \"object\"}}-[:on]->(node); +(n:trait) {metrics: {type: \"object\"}}-[:on]->(node);";
    for (source, message, at) in [
        ("+(k:node)-[:extends]->(missing);".to_owned(), "unknown base kind `missing`", "-[:extends]->(missing)"),
        ("+(k:node)-[:has]->(missing);".to_owned(), "unknown trait `missing`", "-[:has]->(missing)"),
        ("+(b:node); +(k:node)-[:has]->(b);".to_owned(), "`b` is a kind, not a trait", "-[:has]->(b)"),
        ("+(k:node)-[:has]->(vars);".to_owned(), "`vars` is a kind, not a trait", "-[:has]->(vars)"),
        (
            "+(a:node); +(b:node); +(k:node)\n -[:extends]->(a)\n -[:extends]->(b);".to_owned(),
            "a kind extends one base",
            "-[:extends]->(b)",
        ),
        ("+(k:node)-[:extends]->(feeds);".to_owned(), "crosses graph construct categories", "-[:extends]->(feeds)"),
        (
            "+(e:edge); +(k:node)-[:extends]->(e);".to_owned(),
            "crosses graph construct categories",
            "-[:extends]->(e)",
        ),
        (
            format!("{traits} +(k:node)\n -[:has]->(m)\n -[:has]->(n);"),
            "field `metrics` is declared by both trait `m` and trait `n`",
            "-[:has]->(n)",
        ),
        (
            "+(m:trait) {input: {type: \"any\"}}-[:on]->(node); +(b:node) {input: {type: \"any\"}}; +(k:node)\n -[:extends]->(b)\n -[:has]->(m);"
                .to_owned(),
            "trait `m` declares field `input`, which `k` already has from its base",
            "-[:has]->(m)",
        ),
        (
            "+(m:trait {when: {type: \"boolean\"}})-[:on]->(edge); +(k:edge)-[:has]->(m);".to_owned(),
            "trait `m` declares property `when`",
            "-[:has]->(m)",
        ),
        (
            "+(m:trait)-[:on]->(node); +(k:edge)-[:has]->(m);".to_owned(),
            "trait `m` is not on edge",
            "-[:has]->(m)",
        ),
        (
            "+(a:node)-[:extends]->(b); +(b:node)-[:extends]->(a);".to_owned(),
            "circular kind extension",
            "+(a:node)-[:extends]->(b);",
        ),
        ("+(m:trait)-[:on]->(node); +(k:node)-[:extends]->(m);".to_owned(), "a kind cannot extend a trait", "-[:extends]->(m)"),
        ("+(k:node)-[:on]->(node);".to_owned(), "`on` belongs to a trait definition", "-[:on]->(node)"),
        ("+(tool:span);".to_owned(), "a definition head names its construct", "tool:span"),
        ("+(k:node)-[:calls]->(node);".to_owned(), "unknown relationship `calls`", ":calls"),
        ("+(b:node); +(k:node)-[:extends, :has]->(b);".to_owned(), "holds one relationship", "-[:extends, :has]->(b)"),
        ("+(k:node)-[:extends]->(b:node);".to_owned(), "references a definition by name", "-[:extends]->(b:node)"),
        ("+(b:node); +(k:node)-[e:extends]->(b);".to_owned(), "no name and no properties", "e:extends"),
        ("+(b:node); +(k:node)-[:extends]-(b);".to_owned(), "is written -[:kind]->", "-[:extends]-(b)"),
        (
            "+(m:trait)-[:on]->(node); +(b:node); +(k:node)-[:extends]->(b)-[:has]->(m);".to_owned(),
            "starts at the definition head",
            "-[:has]->(m)",
        ),
        (
            "+(m:trait)-[:on]->(node); +(b:node); +(k:node)-[:extends]->(b)\n -[:has]->(m);".to_owned(),
            "starts at the definition head",
            "-[:has]->(m)",
        ),
        (
            "+(k:node)=[:parallel]=>[-[:next]->(:node)];".to_owned(),
            "relationship edges only",
            "=[:parallel]=>[-[:next]->(:node)]",
        ),
        ("+(k:fork) {label: {type: \"string\"}};".to_owned(), "only node kinds have content fields", "label: {type: \"string\"}"),
        ("+(k:node {direction: \"directed\"});".to_owned(), "schema descriptors must be objects", "\"directed\""),
    ] {
        let errors = located(&source);
        assert!(
            errors.iter().any(|(what, text)| what.contains(message) && text == at),
            "{source}\n{errors:#?}"
        );
    }
    // a kind that fails is reported once, and kinds built on it stay quiet
    let errors = located("+(k:node)-[:has]->(missing); +(d:node)-[:extends]->(k); (a:d {anything: 1});");
    assert_eq!(errors.len(), 1, "{errors:#?}");
}

#[test]
fn closed_roots_unknown_kinds_and_strict_properties_are_located_errors() {
    use bts_lang::FieldType;
    let model = compiled(
        r#"
        +(record:node) {value: {type: "number"}, payload: {type: "object"}, anything: {type: "any"}};
        +(notes:node {open: true}) {title: {type: "string"}};
        +(sealed:node {open: false})-[:extends]->(notes);
        +(settings:node {open: true})-[:extends]->(vars);
        (a:record) {value: 1, payload: {free: {form: true}}, anything: [|1, "two"|]};
        (b:notes) {title: "t", extra: 1};
        (c:sealed) {title: "t"};
        (d:settings) {limit: 3};
        (e:node {any: "property"}) {any: "field"};
        (f:vars) {free: 1};
        "#,
    );
    let kind = |name: &str| &model.kinds[model.kind_names[name].0];
    assert!(!kind("record").open && kind("notes").open && !kind("sealed").open && kind("settings").open);
    assert_eq!(binding(&model, "b", "title").ty, FieldType::String);
    assert!(!kind("notes").props.iter().any(|field| field.name == "open"));
    let record = "+(record:node {name: {type: \"string\"}}) {output: {type: \"any\"}};";
    for (source, message, at) in [
        (
            format!("{record} (a:record) {{outptu: 1}};"),
            "unknown field `outptu` for kind `record`, did you mean `output`?",
            "outptu",
        ),
        (
            format!("{record} (a:record) {{unrelated: 1}};"),
            "unknown field `unrelated` for kind `record`",
            "unrelated",
        ),
        (
            format!("{record} (a:record {{nmae: \"x\"}});"),
            "unknown property `nmae` for kind `record`, did you mean `name`?",
            "nmae",
        ),
        (
            format!("{record} (a:recrod);"),
            "unknown node kind `recrod`, did you mean `record`?",
            "recrod",
        ),
        ("(q:tol);".to_owned(), "unknown node kind `tol`", "tol"),
        (
            "(a:node)-[:nxet]->(b:node);".to_owned(),
            "unknown edge kind `nxet`, did you mean `next`?",
            "nxet",
        ),
        (
            "(a:node)=[:exclusiv]=>[-[:next]->(b:node)];".to_owned(),
            "unknown fork kind `exclusiv`, did you mean `exclusive`?",
            "exclusiv",
        ),
        (
            "(a:node)-[e:link]->(b:node); >[:acumulate [e]]>>(c:node);".to_owned(),
            "unknown junction kind `acumulate`, did you mean `accumulate`?",
            "acumulate",
        ),
        (
            "(a:node)-[:next {whne: true}]->(b:node);".to_owned(),
            "unknown property `whne` for built-in kind `next`, did you mean `when`?",
            "whne",
        ),
        (
            "(a:node)-[:link {label: 1}]->(b:node);".to_owned(),
            "unknown property `label` for built-in kind `link`",
            "label",
        ),
        (
            "(a:vars {evalaute: \"once\"});".to_owned(),
            "unknown property `evalaute` for built-in kind `vars`, did you mean `evaluate`?",
            "evalaute",
        ),
        (
            "+(base:node); +(k:node {open: true})-[:extends]->(base);".to_owned(),
            "a kind cannot reopen the closed root of its base",
            "open: true",
        ),
        (
            "+(k:node {open: \"yes\"});".to_owned(),
            "open must be a literal boolean",
            "open: \"yes\"",
        ),
        (
            "+(k:node {open: true, open: false});".to_owned(),
            "duplicate schema key",
            "open: false",
        ),
        (
            "+(k:edge {open: true});".to_owned(),
            "schema descriptors must be objects",
            "true",
        ),
    ] {
        let errors = located(&source);
        assert!(
            errors.iter().any(|(what, text)| what == message && text == at),
            "{source}\n{errors:#?}"
        );
    }
    // an instance of a definition that failed is not also reported as an unknown kind
    let errors = located("+(k:node)-[:has]->(missing); (a:k) {anything: 1};");
    assert_eq!(errors.len(), 1, "{errors:#?}");
}

#[test]
fn context_reads_name_declared_keys_that_reach_them() {
    use bts_lang::FieldType;
    let keys = |keys: &[bts_lang::FieldSchema]| -> Vec<(String, FieldType)> {
        keys.iter().map(|key| (key.name.clone(), key.ty)).collect()
    };
    let key = |name: &str, ty| (name.to_owned(), ty);
    let model = compiled(
        r#"
        +(tenant_scoped:trait)
            -[:on]->(node)
            -[:provides {tenant: {type: "string"}}]->(context)
            -[:requires {caller: {}}]->(context);
        +(regional:trait)
            -[:extends]->(tenant_scoped)
            -[:provides {region: {type: "string"}}]->(context);
        +(entry:node {name: {type: "any"}})
            -[:has]->(regional)
            -[:provides {request_id: {type: "string"}}]->(context);
        +(batch:fork)-[:extends]->(each);
        +(sum:junction)
            -[:extends]->(reduce)
            -[:provides {accumulator: {type: "number"}}]->(context);
        +(handoff:edge)-[:extends]->(next);

        # a kind reads what it requires, its edges, forks and junctions read what it provides
        (a:entry {name: context.caller})
            -[first:handoff {when: context.tenant == "acme"}]->(b:node) {tenant: context.tenant, region: context.region}
                -[:next]->(c:node) {records: [|1, 2|], id: context.request_id, earlier: context.history.b};
        # fork keys reach branch destinations and what follows them along next edges
        (c)
            =[work:batch {items: source.records, concurrency: len(context.request_id)}]=>[
                -[done:next]->(d:node) {item: context.item, index: context.index},
            ]
                ->(d)-[:next]->(e:node) {item: context.item}
                >[:sum {initial: 0, combine: context.accumulator + context.arrival.value + context.index} [done]]>>(f:node) {
                    total: context.accumulator,
                    seen: context.arrivals,
                };
        # a loop-back carries context, and a node reached by several paths reads what any of them carries
        (g:node) {tenant: context.tenant};
        (b)-[:next]->(g);
        (g)~[:next]~>(g);
        (plain:node)-[:next]->(g);
        "#,
    );
    let kind = |name: &str| &model.kinds[model.kind_names[name].0];
    let regional = &model.traits[model.trait_names["regional"].0];
    assert_eq!(
        keys(&regional.provides),
        [key("tenant", FieldType::String), key("region", FieldType::String)]
    );
    assert_eq!(keys(&regional.requires), [key("caller", FieldType::Any)]);
    assert_eq!(
        keys(&kind("entry").provides),
        [
            key("tenant", FieldType::String),
            key("region", FieldType::String),
            key("request_id", FieldType::String)
        ]
    );
    assert_eq!(keys(&kind("entry").requires), [key("caller", FieldType::Any)]);
    // built-in kinds declare their context in the language reference and derived kinds inherit it
    assert_eq!(
        keys(&kind("batch").provides),
        [key("item", FieldType::Any), key("index", FieldType::Number)]
    );
    assert_eq!(
        keys(&kind("sum").provides),
        [key("accumulator", FieldType::Number), key("arrivals", FieldType::Array)]
    );
    assert_eq!(
        keys(&kind("sum").requires),
        [key("accumulator", FieldType::Any), key("arrival", FieldType::Object)]
    );
    assert!(matches!(binding(&model, "f", "total").value, Value::Context { .. }));

    let scoped = r#"+(scoped:node {open: true})-[:provides {tenant: {type: "string"}}]->(context);"#;
    for (source, message, at) in [
        (
            "(a:node)=[:parallel]=>[-[one:next]->(b:node), -[two:next]->(c:node)] >[:accumulate [one], [two]]>>(d:node) {seen: context.arivals};".to_owned(),
            "context key `arivals` is not available here, did you mean `arrivals`? (available: arrivals, history)",
            "context.arivals",
        ),
        (
            "(a:node)-[:next]->(b:node) {value: context.item};".to_owned(),
            "context key `item` is not available here (available: history)",
            "context.item",
        ),
        // a kind does not read the keys it provides
        (
            format!("{scoped} (a:scoped) {{tenant: context.tenant}};"),
            "context key `tenant` is not available here (available: history)",
            "context.tenant",
        ),
        // only next edges carry context
        (
            format!("{scoped} (a:scoped)-[:link]->(b:node) {{tenant: context.tenant}};"),
            "context key `tenant` is not available here (available: history)",
            "context.tenant",
        ),
        (
            format!("{scoped} (a:scoped)-[:contains]->(b:node) {{tenant: context.tenant}};"),
            "context key `tenant` is not available here (available: history)",
            "context.tenant",
        ),
        // an edge reads what its source carries, not what its target receives
        (
            "(a:node)-[e:link]->(b:node); >[:merge [e]]>>(c:node)-[:next {when: context.arrivals == 1}]->(d:node); (x:node)-[:next {when: context.arrivals == 1}]->(c);".to_owned(),
            "context key `arrivals` is not available here (none are available)",
            "context.arrivals",
        ),
        // the reduce target does not read the single arrival its combine property requires
        (
            "(a:node)-[e:next]->(b:node); >[:reduce {initial: 0, combine: context.arrival.value} [e]]>>(c:node) {last: context.arrival};".to_owned(),
            "context key `arrival` is not available here, did you mean `arrivals`? (available: accumulator, arrivals, history)",
            "context.arrival",
        ),
        (
            "(a:node)=[:repeat {count: context.count}]=>[-[:next]->(b:node)];".to_owned(),
            "context key `count` is not available here (none are available)",
            "context.count",
        ),
        (
            "(lookup:node) {earlier: context.history.lokup};".to_owned(),
            "unknown history node `lokup`, did you mean `lookup`?",
            "context.history.lokup",
        ),
        (
            "(a:node)-[e:next {when: len(context.history.a) > 1}]->(b:node);".to_owned(),
            "context key `history` is not available here (none are available)",
            "context.history.a",
        ),
    ] {
        let errors = compile(&source).unwrap_err();
        assert_eq!(errors.len(), 1, "{source}\n{errors:#?}");
        assert_eq!(errors[0].what, message, "{source}");
        let range = errors[0].r#where;
        assert_eq!(&source[range.start..range.end], at, "{source}");
    }

    for (source, message, at) in [
        (
            "+(a:node)-[:provides]->(context);",
            "`provides` lists context keys and targets context, written -[:provides {key: {type: \"any\"}}]->(context)",
            "-[:provides]->(context)",
        ),
        (
            "+(a:node)-[:requires {tenant: {}}]->(node);",
            "`requires` lists context keys and targets context, written -[:requires {key: {type: \"any\"}}]->(context)",
            "-[:requires {tenant: {}}]->(node)",
        ),
        (
            "+(a:node)-[keys:provides {tenant: {}}]->(context);",
            "a relationship edge has no name",
            "keys:provides {tenant: {}}",
        ),
        (
            "+(a:node)-[:extends {tenant: {}}]->(vars);",
            "a relationship edge has no name and no properties",
            ":extends {tenant: {}}",
        ),
        (
            "+(a:node)-[:supplies {tenant: {}}]->(context);",
            "unknown relationship `supplies`, expected extends, has, on, provides or requires",
            ":supplies {tenant: {}}",
        ),
        (
            "+(a:node)-[:provides {tenant: {type: \"string\", required: true}}]->(context);",
            "unknown context descriptor `required`, a provided context key takes type and value",
            "required: true",
        ),
        (
            "+(a:node)-[:requires {tenant: {type: \"string\", required: true}}]->(context);",
            "unknown context descriptor `required`, a required context key takes type only",
            "required: true",
        ),
        (
            "+(a:node)-[:requires {tenant: {type: \"string\", value: \"acme\"}}]->(context);",
            "a required context key is supplied from outside the graph and takes no value",
            "value: \"acme\"",
        ),
        (
            "+(a:node)-[:provides {tenant: {type: \"text\"}}]->(context);",
            "unknown schema value type",
            "type: \"text\"",
        ),
        (
            "+(a:node)-[:provides {tenant: {}, tenant: {}}]->(context);",
            "duplicate context key",
            "tenant",
        ),
        (
            "+(a:node)-[:requires {history: {}}]->(context);",
            "context.history is defined by the language",
            "history",
        ),
        (
            "+(a:junction)\n    -[:extends]->(accumulate)\n    -[:provides {arrivals: {type: \"string\"}}]->(context);",
            "a context key cannot change its inherited type",
            "arrivals: {type: \"string\"}",
        ),
        (
            "+(t:trait)\n    -[:on]->(node)\n    -[:provides {tenant: {}}]->(context);\n+(u:trait)\n    -[:on]->(node)\n    -[:provides {tenant: {}}]->(context);\n+(a:node)\n    -[:has]->(t)\n    -[:has]->(u);",
            "provided context key `tenant` is declared by both trait `t` and trait `u`",
            "-[:has]->(u)",
        ),
        (
            "+(t:trait)\n    -[:on]->(junction)\n    -[:provides {arrivals: {}}]->(context);\n+(a:junction)\n    -[:extends]->(merge)\n    -[:has]->(t);",
            "trait `t` declares provided context key `arrivals`, which `a` already has from its base",
            "-[:has]->(t)",
        ),
    ] {
        let errors = compile(source).unwrap_err();
        assert_eq!(errors.len(), 1, "{source}\n{errors:#?}");
        assert_eq!(errors[0].what, message, "{source}");
        let range = errors[0].r#where;
        let found = &source[range.start..range.end];
        assert!(found.starts_with(at), "{source}\n{found}");
    }
}

#[test]
fn edge_kinds_provide_context_and_models_record_suppliers() {
    let definitions = r#"
        +(delegated:edge {direction: "directed", note: {type: "any"}})
            -[:extends]->(next)
            -[:provides {owner: {type: "string"}}]->(context);
        +(gated:edge {direction: "directed"})
            -[:extends]->(requires)
            -[:provides {owner: {type: "string"}}]->(context);
        +(owning:node {open: true})-[:provides {owner: {type: "string"}}]->(context);
        +(counted:edge {direction: "directed"})
            -[:extends]->(next)
            -[:provides {index: {type: "number"}}]->(context);
        +(claimed:junction)
            -[:extends]->(merge)
            -[:provides {owner: {type: "string"}}]->(context);
    "#;
    let model = compiled(&format!(
        r#"{definitions}
        (a:node)-[e1:delegated]->(b:node) {{by: context.owner}};
        (b)-[:next]->(c:node) {{by: context.owner}};
        (c)-[e2:delegated]->(d:node) {{by: context.owner}};
        (d)~[:next]~>(d);
        (a)-[e3:gated]->(g:node) {{by: context.owner}};
        (c)-[:requires]->(h:node);
        (n:owning)-[:next]->(p:node) {{by: context.owner}};
        (n)-[e4:delegated]->(q:node) {{by: context.owner}};
        (b)-[e5:delegated]->(q);
        (r:node)=[f:repeat {{count: 2}}]=>[-[e6:counted]->(s:node) {{at: context.index}}, -[e7:next]->(t:node) {{at: context.index}}];
        (u:node)=[:parallel]=>[-[e8:delegated]->(v:node), -[e9:delegated]->(w:node)]
            >[:merge [e8], [e9]]>>(x:node) {{by: context.owner}};
        (u2:node)=[:parallel]=>[-[e10:delegated]->(v2:node), -[e11:next]->(w2:node)]
            >[:claimed [e10], [e11]]>>(x2:node) {{by: context.owner}};
        "#
    ));
    let suppliers = |node: &str, key: &str| -> Vec<bts_lang::ElementId> {
        let bts_lang::ElementId::Node(id) = model.names[node] else {
            panic!("{node}")
        };
        (model.context[id.0].iter())
            .find(|supply| supply.key == key)
            .map(|supply| supply.suppliers.clone())
            .unwrap_or_default()
    };
    let named = |names: &[&str]| -> Vec<bts_lang::ElementId> {
        let mut ids: Vec<_> = names.iter().map(|name| model.names[*name]).collect();
        ids.sort();
        ids
    };
    assert_eq!(model.context.len(), model.nodes.len());
    // the key reaches the edge's target and continues along next
    assert_eq!(suppliers("a", "owner"), []);
    assert_eq!(suppliers("b", "owner"), named(&["e1"]));
    assert_eq!(suppliers("c", "owner"), named(&["e1"]));
    // the nearest provider on a path supplies the key, also around a loop-back
    assert_eq!(suppliers("d", "owner"), named(&["e2"]));
    // an edge kind provides its own keys whatever its primitive, and carries nothing else across
    assert_eq!(suppliers("g", "owner"), named(&["e3"]));
    assert_eq!(suppliers("h", "owner"), []);
    // a node kind supplies along next, and an edge kind is nearer than its source node
    assert_eq!(suppliers("p", "owner"), named(&["n"]));
    // two paths give two suppliers
    assert_eq!(suppliers("q", "owner"), named(&["e4", "e5"]));
    // an edge kind is nearer than its fork
    assert_eq!(suppliers("s", "index"), named(&["e6"]));
    assert_eq!(suppliers("t", "index"), named(&["f"]));
    // a junction target receives what its input edges carry
    assert_eq!(suppliers("x", "owner"), named(&["e8", "e9"]));
    // a junction kind is nearer than its input edges
    assert_eq!(
        suppliers("x2", "owner"),
        [bts_lang::ElementId::Junction(bts_lang::JunctionId(1))]
    );

    for (source, at) in [
        // the key does not reach the edge's source
        ("(a:node) {by: context.owner}; (a)-[:delegated]->(b:node);", "context.owner"),
        // an edge does not read the keys its own kind provides
        ("(a:node)-[:delegated {note: context.owner}]->(b:node);", "context.owner"),
        // an edge of another primitive carries no keys from its source
        (
            "(n:owning)-[:gated]->(g:node); (n)-[:requires]->(h:node) {by: context.owner};",
            "context.owner",
        ),
    ] {
        let source = format!("{definitions}{source}");
        let errors = compile(&source).unwrap_err();
        assert_eq!(errors.len(), 1, "{source}\n{errors:#?}");
        assert!(
            errors[0].what.starts_with("context key `owner` is not available here"),
            "{}",
            errors[0].what
        );
        let range = errors[0].r#where;
        assert!(source[range.start..range.end].starts_with(at), "{source}");
    }
}

#[test]
fn provided_context_keys_carry_values_lowered_for_each_element() {
    let definitions = r#"
        +(tenant_scoped:trait {tenant: {type: "string", required: true}})
            -[:on]->(node)
            -[:provides {tenant: {type: "string", value: self.props.tenant}}]->(context);
        +(entry:node {open: true})-[:has]->(tenant_scoped);
        +(stamped:edge {direction: "directed"})
            -[:extends]->(next)
            -[:provides {stamp: {type: "object", value: {from: source.label}}}]->(context);
        +(narrowed:node {open: true})
            -[:extends]->(entry)
            -[:provides {tenant: {type: "string"}}]->(context);
    "#;
    let model = compiled(&format!(
        r#"{definitions}
        (a:entry {{tenant: "acme"}}) {{label: "first"}};
        (a)-[e:stamped]->(b:node) {{tenant: context.tenant, stamp: context.stamp}};
        (c:narrowed {{tenant: "globex"}})-[:next]->(d:node) {{tenant: context.tenant}};
        "#
    ));
    let supplied = |owner: &str, key: &str| -> &bts_lang::Binding {
        let owner = model.names[owner];
        let value = (model.context_values.iter())
            .find(|value| value.owner == owner && value.key == key)
            .unwrap_or_else(|| panic!("{key}"));
        &model.bindings[value.binding.0]
    };
    // one value for each element of a providing kind, and none for the kinds that only read
    assert_eq!(model.context_values.len(), 3);
    assert!(matches!(&supplied("a", "tenant").value, bts_lang::Value::Str(text) if text == "acme"));
    // a derived kind keeps the value when it repeats the key without one
    assert!(matches!(&supplied("c", "tenant").value, bts_lang::Value::Str(text) if text == "globex"));
    // an edge supplies a value that reads its endpoints
    assert_eq!(supplied("e", "stamp").owner, model.names["e"]);
    // a supplied value is not one of its element's fields or properties
    let bts_lang::ElementId::Node(a) = model.names["a"] else {
        panic!()
    };
    assert_eq!(model.nodes[a.0].fields.len(), 1);
    assert_eq!(model.nodes[a.0].props.len(), 1);

    for (source, message, at) in [
        (
            // the value must match the declared type
            r#"+(t:trait)-[:on]->(node)-[:provides {tenant: {type: "string", value: 7}}]->(context);
            +(k:node)-[:has]->(t); (a:k);"#,
            "",
            "value: 7",
        ),
        (
            // the value reads only what the supplying element has
            r#"+(k:node)-[:provides {tenant: {type: "string", value: self.props.missing}}]->(context); (a:k);"#,
            "unknown property `missing`",
            "self.props.missing",
        ),
    ] {
        let errors = compile(source).unwrap_err();
        assert!(
            errors.iter().any(|error| {
                let text = &source[error.r#where.start..error.r#where.end];
                error.what.contains(message) && (text.contains(at) || at.contains(text))
            }),
            "{source}\n{errors:#?}"
        );
    }
}
