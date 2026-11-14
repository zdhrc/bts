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
fn braintrust_provider_schemas_reject_invalid_known_fields_and_properties() {
    for source in [
        "(a:tool {typo: true});",
        "(a:span {name: 42});",
        "(a:task) {metadata: false};",
        "(a:llm) {metrics: 1};",
        "(a:function) {tags: \"tag\"};",
        "(a:tool) {duration: \"slow\"};",
        "(a:span) {scores: false};",
        "(a:scorer);",
        "(a:scorer) {score: \"good\"};",
        "(a:scorer) {score: 1, reason: false};",
    ] {
        let errors = compile_braintrust(source).expect_err(source);
        assert!(errors.iter().all(|error| error.r#where.source == 0), "{errors:#?}");
    }
    compile_braintrust("(a:tool) {custom: {anything: true}};").unwrap();
    compile_braintrust(
        "+(custom_tool::tool {fields: {custom: {type: \"string\", required: true}}}); (a:custom_tool) {custom: \"value\"};",
    )
    .unwrap();
    assert!(compile_braintrust("+(tool::node {}); (a:tool);").is_err());
}

// illustrative provider definitions, not the final Braintrust schema
const PROVIDER_SOURCE: &str = r#"
+(span::node {
    props: {
        name: {type: "string", required: true},
    },
    fields: {
        patient_id: {type: "string"},
        output: {type: "object", role: "output"},
        metadata: {type: "object", default: {}},
    },
});

+(tool_span::span {});
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
    assert!(matches!(field(scorer, "code").kind, ExprKind::Object(_)));
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
    let source = r#"(lookup:tool {name: "patient_lookup"}) {output: {found: true}};
        (:task); ("patient lookup":tool) {}; ("patient lookup"); (lookup);"#;
    let graph = parse(source).unwrap();
    assert_eq!(graph.paths.len(), 5);
    let lookup = declaration(&graph.paths[0].start);
    assert_eq!(lookup.header.name.as_deref(), Some("lookup"));
    assert_eq!(lookup.header.kind, "tool");
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
    let source = "(a:task)-[advance:next {when: source.ready}, :feeds]->(b:tool)~[retry:next]~>(a);";
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
        "(a:task) {(b:task)};",
        "(a:task {value: (b:task)});",
        "(a:task {value: {nested: [|(:task)|]}});",
        "(a:task) {value: (:task)};",
        "(a:task) {value: [edge:next]};",
        "(a:task) {value: [checks].[edge:next]};",
        "(a:task) {value: (b)-[:next]->(c)};",
        "(a:task)-[:next]->();",
    ] {
        assert!(parse(source).is_err(), "unexpectedly accepted {source}");
    }
}

#[test]
fn requires_separators_and_typed_connections() {
    for source in [
        "(a:task)",
        "(a:task),",
        "(a:task) (b:task);",
        "(a:task) {one: 1 two: 2};",
        "(a:task) {one: 1\ntwo: 2};",
        "(a:task) {nested: {one: 1\ntwo: 2}};",
        "(a:task {one: 1\ntwo: 2});",
        "(a:task) {one = 1};",
        "(a:task) {items: [|1 2|]};",
        "(a:task) {items: [1, 2]};",
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
    let graph = parse("(unknown)-[:custom]->(:custom {anything: 1}); (same:task); (same:tool);").unwrap();
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
    let source = r#"(a:tool {name: "lookup"}) {text: "Value ${hex(8)}", items: [|for x in xs: x|]}-[next:next]->(b);"#;
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
        ">[:accumulate [checks].[coverage], [records],]>>(ready:task) {input: [records].scope}\n    -[:next]->(finish);";
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
    let source = "(a)\n  =[checks:parallel]=>[-[lookup:next]->(:tool), -[note:next]->(b)]\n    ->([lookup].target)~[:again]~>(a)\n    >[:accumulate [lookup], [note]]>>(done);";
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
    let source = "(a:task) {\n    text: \"\"\"\n        ->(not_a_selector)\n    \"\"\",\n}\n  =[:parallel]=>[\n    -[to_b:next]->(b:task) {\n        nested: {items: [|1, 2|]},\n    },\n  ]\n    # this comment does not affect attachment\n    ->(b)-[:next]->(done);";
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
        "(a)=[:parallel]=>[-[:next]->(b)] ->(new:task);",
    ] {
        assert!(parse(source).is_err(), "unexpectedly accepted {source}");
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
    assert_eq!(graph.paths.len(), 53);
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
    let source = "(refs:vars) {node: ?<(collect)-[:next]->(@:tool)>, edge: ?<(collect)-[@:next]->(lookup)>};";
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
    assert_eq!(target.kind, "tool");
    assert!(!node.steps[0].edges[0].selected);
    let ExprKind::PatternRef(edge) = &field(refs, "edge").kind else {
        panic!("expected pattern")
    };
    assert!(edge.steps[0].edges[0].selected);
    assert_eq!(edge.steps[0].edges[0].kind, "next");
    assert!(matches!(edge.steps[0].target, GraphPatternNode::Reference(_)));
    assert_eq!(&source[node.range.start..node.range.end], "?<(collect)-[:next]->(@:tool)>");
}

#[test]
fn patterns_preserve_constraints_chains_edge_lists_and_cycles() {
    use bts_lang::syntax::GraphPatternNode;
    let source = "?<(@:task {name: \"start\"})-[:next {when: score > 1}, :feeds]->(:tool {tags: [|\"a\"|]})~[:again]~>(start)>";
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
    let source = "(a)-[:next]->(?<(b)-[:next]->(@:tool)>); >[:accumulate ?<(a)-[@:next]->(b)>, [other]]>>(done); (values:vars) {name: ?<(a)-[:next]->(@:tool)>.props.name, text: \"Found ${?<(@:tool)>.props.name}\"};";
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
        let source = format!("?<(@:tool)>{operator}(lookup)");
        let expression = bts_lang::syntax::parse_expression(&source).unwrap();
        let ExprKind::Binary { op: actual, lhs, .. } = expression.kind else {
            panic!("expected binary expression")
        };
        assert_eq!(actual, op);
        assert!(matches!(lhs.kind, ExprKind::PatternRef(_)));
    }
    assert!(bts_lang::syntax::parse_expression("?<(@:tool {text: \"> -> ~>\", nested: [|{ready: score >= 2}|]})>").is_ok());
}

#[test]
fn rejects_patterns_with_missing_or_multiple_selection_markers() {
    for source in [
        "?<(a)-[:next]->(:tool)>",
        "?<(@:task)-[@:next]->(b)>",
        "?<(@:task)-[:next]->(@:tool)>",
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
        "?<(created:task)-[:next]->(@:tool)>",
        "?<(a)-[created:next]->(@:tool)>",
        "?<(@:task) {output: true}>",
        "?<(@:task {value: (created:tool)})>",
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
    let source = "?<(@:tool {values: [|1, 2|], text: \"${hex(8)}\"})-[:next]->(done)>";
    for (end, _) in source.char_indices() {
        let _ = bts_lang::syntax::parse_expression(&source[..end]);
    }
    assert!(bts_lang::syntax::parse_expression(source).is_ok());
}

#[test]
fn lowers_the_compilation_fixture_to_a_domain_neutral_graph() {
    let model = compile_fixture();
    assert_eq!(model.paths.len(), 53);
    assert!(model.nodes.len() > 30);
    assert!(model.edges.len() > 30);
    assert!(model.forks.len() > 5);
    assert!(model.junctions.len() > 2);
}

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
    let model =
        compiled("(a:anything {custom: true}) {destination: (b)} -[advance:custom]->(a.destination); (b:other) {value: 42};");
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
        ("(a:x); (a:y);", "duplicate graph name"),
        ("(a:x)-[a:next]->(:x);", "duplicate graph name"),
        ("(a:x) {value: 1, value: 2};", "duplicate field"),
        ("(a:x) {props: 1};", "reserved"),
        ("(missing);", "unresolved"),
        ("(a:x)-[e:next]->(:x); (e);", "boundary"),
        ("(a:x) {value: [a]};", "boundary"),
        ("(a:x) {value: self.missing};", "unknown field"),
        ("(a:x) {value: self.value};", "circular field dependency"),
        ("(a:x) {value: b.value}; (b:x) {value: a.value};", "circular field dependency"),
        ("(a:x) {value: source.value};", "endpoint"),
        ("(a:vars {evaluate: \"sometimes\"});", "evaluate"),
        ("(a:x)-[:next {when: 1}]->(:x);", "boolean"),
        ("(a:tool); (b:scorer) {value: a.output};", "unknown field"),
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
    let source = "(first:x);\n(first:y);";
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
    let model = compiled("(a:x) {metrics: {base: 3, total: self.metrics.base + 2}} -[:next]->(b:x) ~[:retry]~>(a);");
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
    assert!(matches!(binding(&model,"v","alias").value, Value::BindingRef(id) if id.0 == draw));
    assert!(matches!(binding(&model, "v", "result").value, Value::Binary { .. }));
    assert!(matches!(binding(&model, "v", "text").value, Value::Template(_)));
    compiled(
        "(v:vars) {draws: [|range(1,9), range(2,5)|], alias: self.draws, result: [|for x in self.alias: x + 1|], selected: self.alias[0] + 1};",
    );
}

#[test]
fn resolves_edge_context_against_explicit_endpoints() {
    let model = compiled(
        "(:x) {value: 2} -[e:next {sum: source.value + target.value}]->(:x) {value: 3}; (v:vars) {scope: [e].scope.member, external: context.member};",
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
        "(a:x)\n    =[f:parallel]=>[-[eb:next]->(b:x), -[ec:next]->(c:x), -[ed:next]->(d:x)]\n        >[:accumulate [eb], [ec]]>>(e:x)\n        >[:accumulate [ec], [ed]]>>(g:x);",
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
        "(a:x) =[f:parallel]=>[-[eb:next]->(b:x)] ->(c); (c:x);",
        "(a:x) =[f:parallel]=>[-[eb:next]->(b:x)] >[:accumulate [ec]]>>(d:x); (c:x)-[ec:next]->(b);",
        "(a:x) =[f:parallel]=>[-[eb:next]->(b:x)]; (c:x)-[ec:next]->(b); (v:vars) {value: [f].[ec]};",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("(a:x) =[f:parallel]=>[-[eb:next]->(b:x)] ->([f].[eb].target)-[:next]->(c:x);");
}

#[test]
fn validates_graph_primitives_without_demanding_defaults() {
    for (kind, guards, valid) in [
        ("parallel", "-[:next]->(:x), -[:next]->(:x)", true),
        ("parallel", "-[:next {when: true}]->(:x)", false),
        (
            "conditional",
            "-[:next {when: true}]->(:x), -[:next {when: true}]->(:x)",
            true,
        ),
        (
            "exclusive",
            "-[:next {when: false}]->(:x), -[:next {when: false}]->(:x)",
            true,
        ),
        ("exclusive", "-[:next]->(:x), -[:next]->(:x)", false),
        (
            "exclusive",
            "-[:next {when: source.flag}]->(:x), -[:next {when: !source.flag}]->(:x)",
            true,
        ),
        (
            "exclusive",
            "-[:next {when: chance(0.5)}]->(:x), -[:next {when: !chance(0.5)}]->(:x)",
            false,
        ),
    ] {
        let source = format!("(a:x) {{flag: chance(0.5)}} =[:{kind}]=>[{guards}];");
        assert_eq!(compile(&source).is_ok(), valid, "{source}\n{:?}", compile(&source));
    }
}

#[test]
fn resolves_patterns_by_distinct_identity() {
    let model = compiled(
        "(a:x)-[e:next {selected: true}]->(b:tool {selected: true}); (a)-[:next]->(b); (v:vars) {node: ?<(a)-[:next]->(@:tool {selected: true})>, edge: ?<(a)-[@:next {selected: true}]->(b)>};",
    );
    let Value::GraphRef { ref_id, .. } = binding(&model, "v", "node").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["b"]);
    for source in [
        "(a:x); (v:vars) {value: ?<(a)-[:next]->(@:tool)>};",
        "(a:x)-[:next]->(b:tool); (a)-[:next]->(c:tool); (v:vars) {value: ?<(a)-[:next]->(@:tool)>};",
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
    assert!(matches!(object.elem[0].value, Value::BindingRef(_)));
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
    let model = compiled("(a:task)-[:next]->(b:task); (c:task)-[:copy]->(?<(a)-[:next]->(@:task)>);");
    assert_eq!(model.edges[0].target, model.edges[1].target);
}

#[test]
fn proves_complementary_comparisons_without_conflating_calls() {
    compiled(
        "(a:x) {n: range(1, 9)} =[:exclusive]=>[-[:next {when: source.n > 5}]->(:x), -[:next {when: !${source.n > 5}}]->(:x)];",
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
            format!("(\"{name}\":task);"),
            format!("(:task)-[{name}:next]->(:task);"),
            format!("(:task)=[{name}:parallel]=>[-[:next]->(:task)];"),
        ] {
            let errors = compile(&source).expect_err(&source);
            assert!(errors.iter().any(|error| error.what.contains("reserved graph name")));
        }
    }
    for name in ["self", "source", "target", "context"] {
        assert!(compile(&format!("(a:vars) {{values: [|for {name} in [|1|]: {name}|]}};")).is_err());
    }
    compiled("(a:task) {source: 1, target: 2, context: 3, self: 4};");
}

#[test]
fn resolves_graph_identities_through_collections_and_static_expressions() {
    let model = compiled(
        r#"(a:task) {value: 7}; (b:task);
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
    assert!(compile("(a:task); (v:vars) {nodes: [|(a)|], index: range(0, 1)}; (v.nodes[v.index]);").is_err());
}

#[test]
fn resolves_edge_expressions_and_stored_fork_paths() {
    let model = compiled(
        r#"(a:task) =[split:parallel]=>[-[input_left:next {limit: 3}]->(:task), -[input_right:next]->(:task)];
        (refs:vars) {paths: [split].paths, edges: [|[input_left], [input_right]|], alias: self.edges[0]};
        (refs.paths[0][0].target)-[follow:next {limit: [refs.alias].props.limit}]->(b:task);
        >[:accumulate [refs.alias], [refs.edges[1]]]>>(b);"#,
    );
    assert_eq!(model.edges[2].source, model.edges[0].target);
    assert_eq!(model.junctions[0].inputs, [bts_lang::EdgeId(0), bts_lang::EdgeId(1)]);
    assert!(matches!(
        model.bindings[model.edges[2].props[0].0].value,
        Value::Num(bts_lang::Number::Int(3))
    ));
    for source in [
        "(a:task); (v:vars) {node: (a), wrong: [self.node]};",
        "(a:task)-[edge:next]->(b:task); (v:vars) {edge: [edge]}; (v.edge);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn keeps_junction_properties_separate_from_destination_properties() {
    let source = r#"(config:vars) {policy: "all"};
        (a:task) =[f:parallel]=>[-[input_left:next]->(b:task), -[input_right:next]->(c:task)]
            >[:accumulate {mode: config.policy, correlate: context.request_id} [input_left], [input_right]]>>(d:task {accumulate: "each"});
        >[:custom {custom: [input_right].target} [input_left]]>>(d);"#;
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
        "{limit: (bad:task)}",
    ] {
        let source = format!("(a:task)-[edge:next]->(b:task); >[:accumulate {props} [edge]]>>(c:task);");
        assert!(compile(&source).is_err(), "{source}");
    }
}

#[test]
fn cycle_edges_keep_independent_kinds_properties_and_references() {
    let model = compiled(
        r#"(settings:vars) {limit: 3};
        (a:task)-[advance:next]->(b:task)
            ~[retry:next {max_attempts: settings.limit}, feedback:feeds {value: source.props}]~>(a);
        (refs:vars) {retry_edge: [retry], limit: [retry].props.max_attempts};"#,
    );
    assert_eq!(model.edges.len(), 3);
    assert!(!model.edges[0].cycle);
    assert!(model.edges[1].cycle && model.edges[2].cycle);
    assert_eq!(model.edges[1].kind, "next");
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
    let source = format!("(a:task)-[{headers}]->(b:task); (refs:vars) {{selected: ?<(a)-[{constraints}]->(@:task)>}};");
    let model = compiled(&source);
    let Value::GraphRef { ref_id, .. } = binding(&model, "refs", "selected").value else {
        panic!()
    };
    assert_eq!(model.refs[ref_id.0].target, model.names["b"]);

    compiled(
        "(a:x)-[one:link {special: true}, two:link]->(b:x); (v:vars) {selected: ?<(a)-[:link, @:link {special: true}]->(b)>};",
    );
    for source in [
        "(a:x)-[one:link, two:link]->(b:x); (v:vars) {selected: ?<(a)-[@:link, :link]->(b)>};",
        "(a:x)-[one:link]->(b:x); (v:vars) {selected: ?<(a)-[:link, :link]->(@:x)>};",
        "(a:x)-[one:link {p: true}, two:link, three:link]->(b:x); (v:vars) {selected: ?<(a)-[:link {p: true}, :link {p: true}, :link]->(@:x)>};",
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
            r#"(a:task) {{flag: chance(0.5), alias: self.flag, route: context.route, n: range(0, 10), draw: range(1, 2), other_draw: range(1, 2)}}
            =[:exclusive]=>[-[:next {{when: {left}}}]->(:task), -[:next {{when: {right}}}]->(:task)];"#
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
    let model = compiled("(a:x)-[edge:link]->(b:x); >[:custom {policy: 1} [edge]]>>(b);");
    assert_eq!(model.junctions[0].kind, "custom");
    assert_eq!(model.junctions[0].props.len(), 1);
}

#[test]
fn registers_edge_primitives_without_closing_consumer_kinds() {
    for kind in ["link", "next", "requires", "contains", "feeds"] {
        let primitive = bts_lang::spec::SPEC.primitive("edge", kind).unwrap();
        assert_eq!(primitive.name, kind);
        let model = compiled(&format!("(a:custom)-[:{kind}]->(b:custom);"));
        assert_eq!(model.edges[0].kind, kind);
    }
    assert!(bts_lang::spec::SPEC.primitive("edge", "parallel").is_none());
    compiled("(a:custom)-[:consumer_kind {custom: 1}]->(b:custom);");
}

#[test]
fn validates_builtin_junction_configuration() {
    let definitions = "(a:task)-[input_left:link]->(b:task); (a)-[input_right:link]->(c:task);";
    for kind in ["accumulate", "merge", "first", "quorum"] {
        assert!(bts_lang::spec::SPEC.primitive("junction", kind).is_some());
        let props = if kind == "quorum" { "{count: 2}" } else { "{}" };
        let model = compiled(&format!(
            "{definitions} >[:{kind} {props} [input_left], [input_right]]>>(d:task);"
        ));
        assert_eq!(model.junctions[0].kind, kind);
        assert!(
            compile(&format!(
                "{definitions} >[:{kind} {props} [input_left], [input_left]]>>(d:task);"
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
                "{definitions} >[:quorum {props} [input_left], [input_right]]>>(d:task);"
            ))
            .is_err()
        );
    }
    compiled(&format!(
        "{definitions} (config:vars) {{count: 1 + 1}}; >[:quorum {{count: config.count}} [input_left], [input_right]]>>(d:task);"
    ));
}

#[test]
fn fixture_lowers_kind_schemas_upstream_inputs_and_destinations() {
    use bts_lang::{Construct, ElementId, FieldType};
    let ast = parse(COMPILATION_SOURCE).unwrap();
    assert_eq!(ast.kinds.len(), 5);
    assert_eq!(ast.kinds[0].construct, Construct::Node);
    assert_eq!(ast.kinds[1].construct, Construct::Edge);
    assert_eq!(ast.kinds[2].construct, Construct::Fork);
    assert_eq!(ast.kinds[3].construct, Construct::Junction);
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
    assert!(matches!(
        binding(&model, "patient_step", "extra_content").value,
        Value::Str(_)
    ));
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
        r#"+(child::base {fields: {value: {required: true}}});
        +(base::node {props: {name: {type: "string", default: "example"}}, fields: {value: {type: "number"}}});
        (a:child) {value: 2, extra: true};"#,
    );
    assert_eq!(model.kinds[0].base_kind, Some(model.kind_names["base"]));
    assert!(
        bts_lang::semantics::compile_module("+(only::node {});", true)
            .unwrap()
            .kinds
            .len()
            == 1
    );
    for source in [
        "+(a::b {}); +(b::a {});",
        "+(a::missing {});",
        "+(a::node {}); +(a::node {});",
        "+(node::node {});",
        "+(self::node {});",
        "+(a::feeds {});",
        "+-[a::node {}]-;",
        "+(base::node {}); (:base)-[:base]->(:node);",
        "+(base::node {fields: {v: {type: \"string\", required: true}}}); +(child::base {fields: {v: {required: false}}});",
        "+(base::node {fields: {v: {type: \"string\"}}}); +(child::base {fields: {v: {type: \"number\"}}});",
        "+(base::node {fields: {v: {type: \"any\", default: \"text\"}}}); +(child::base {fields: {v: {type: \"number\"}}});",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn keeps_properties_strict_and_fields_open_with_typed_defaults() {
    let schema = r#"+(record::node {props: {name: {type: "string", required: true}, enabled: {type: "boolean", default: true}}, fields: {value: {type: "number", required: true}, fallback: {type: "object", default: {}}}});"#;
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
        "+(bad::node {props: {v: {type: \"imaginary\"}}});",
        "+(bad::node {props: {v: {type: \"number\", default: \"wrong\"}}});",
        "+(bad::node {fields: {v: {default: range(1, 2)}}});",
        "+(bad::node {fields: {props: {}}});",
        "+(bad::node {fields: {v: {typo: true}}});",
    ] {
        assert!(compile(schema).is_err(), "{schema}");
    }
}

#[test]
fn feeds_references_input_destinations_without_reading_them() {
    let schema = "+(receiver::node {fields: {id: {type: \"string\", required: true}}});";
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
        "+=[choice::exclusive {}]=; (:node)=[:choice]=>[-[:next]->(:node), -[:next]->(:node)];",
        "+=[all::parallel {}]=; (:node)=[:all]=>[-[:next {when: true}]->(:node)];",
        "+>[some::quorum {}]>>; (:node)-[e:link]->(:node); >[:some {count: 2} [e]]>>(:node);",
        "+(values::vars {}); (:values {evaluate: \"sometimes\"});",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    compiled("+>[joined::accumulate {}]>>; (:node)-[e:link]->(:node); >[:joined [e]]>>(:node);");
}

#[test]
fn rejects_invalid_kind_placements_and_handles_truncated_declarations() {
    let sources = [
        "+(span::node {fields: {input: {type: \"string\"}}});",
        "+-[payload::feeds {props: {label: {type: \"string\"}}}]-;",
        "+=[split::parallel {}]=;",
        "+>[joined::accumulate {}]>>;",
        "(a:node) {id: <-};",
    ];
    for source in sources {
        for end in source.char_indices().map(|(index, _)| index) {
            let prefix = &source[..end];
            assert!(std::panic::catch_unwind(|| parse(prefix)).is_ok(), "{prefix}");
        }
    }
    for source in [
        "(a:node) {kind: +(span::node {})};",
        "+(span::node {}); (a:span {name: <-});",
        "+(span::node {fields: {input: {default: <-}}});",
        "+(span: :node {});",
        "+(span::node {}) {extra: 1};",
        "+(span::node {}); (a:span) {values: [|<-|]};",
        "+(span::node {}); (a:span) {input: <- + 1};",
        ">>[:accumulate [e]]->(a);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn upstream_requirements_suppress_defaults_and_allow_alternative_suppliers() {
    let model = compiled(
        r#"+(receiver::node {fields: {id: {type: "string", default: "fallback"}}});
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
    let schema = r#"+(record::node {props: {label: {type: "string"}}, fields: {id: {type: "string"}}});"#;
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
        r#"{schema} +(receiver::node {{fields: {{id: {{type: "number"}}}}}});
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
    let schema = r#"+(record::node {fields: {id: {type: "string"}}});"#;
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
        let source =
            format!("(a:node) {{guard: context.flag ? true : 1}} =[:conditional]=>[-[:next {{when: {guard}}}]->(b:node)];");
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
    for (open, close, base) in [
        ("+(", ")", "node"),
        ("+-[", "]-", "edge"),
        ("+=[", "]=", "fork"),
        ("+>[", "]>>", "junction"),
    ] {
        compiled(&format!("{open}patient_kind::{base} {{}}{close};"));
        for name in ["\"patient span\"", "\"patient_kind\""] {
            let errors = parse(&format!("{open}{name}::{base} {{}}{close};")).unwrap_err();
            assert!(errors.iter().all(|error| error.when == DiagPhase::Parsing));
        }
    }
}

#[test]
fn undirected_edges_match_in_reverse_without_changing_endpoint_identity() {
    use bts_lang::{Direction, ElementId};
    let source = r#"
        (a:node) {value: 2}; (b:node) {value: 3};
        (a)-[ab:link {sum: left.value + right.value}]-(b);
        (refs:node) {edge: ?<(b)-[@:link]-(a)>, l: [ab].left, r: [ab].right};
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
        +-[friendship::link {direction: "undirected"}]-;
        +-[close_friend::friendship {}]-;
        (a:node)-[:close_friend]-(b:node);
        (a)-[:custom]-(b); (a)-[:custom]->(b);
    "#,
    );
    assert_eq!(
        model.kinds[model.kind_names["close_friend"].0].direction,
        Some(Directions::Undirected)
    );
    for source in [
        "+-[bad::next {direction: \"either\"}]-;",
        "+-[base::link {direction: \"undirected\"}]-; +-[bad::base {direction: \"directed\"}]-;",
        "+(bad::node {direction: \"either\"});",
        "+-[bad::link {direction: \"sideways\"}]-;",
        "(a:node)-[:next]-(b:node);",
        "(a:node)-[:feeds]-(b:node);",
        "+-[friendship::link {direction: \"undirected\"}]-; (a:node)~[:friendship]~>(a);",
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
        +(conversion::relation {props: {label: {type: "string"}}});
        +-[participant::incidence {}]-;
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
        ("count: 1", "3", true),
        ("count: 2", "3", true),
        ("count: 3", "3", false),
        ("count: 0", "3", false),
        ("count: 1", "-1", false),
        ("count: 2", "0", false),
        ("count: 1", "\"heavy\"", false),
        ("count: context.count", "1", true),
    ] {
        let source = format!(
            "(a:node) =[:weighted {{{props}}}]=>[-[:next {{weight: {weights}}}]->(b:node), -[:next {{weight: 1}}]->(c:node)];"
        );
        assert_eq!(compile(&source).is_ok(), valid, "{source}");
    }
    compiled("(a:node) =[:weighted]=>[-[:link {weight: 2}, :link {weight: 2}]-(b:node)];");
    compiled("(a:node) =[:weighted {count: context.count}]=>[-[:next {weight: context.weight}]->(b:node)];");
    compiled("(a:node) =[:weighted]=>[-[:link {when: context.flag}, :link {when: true}]-(b:node)];");
    compiled("(a:node) =[:each {items: context.items, concurrency: context.limit}]=>[-[:next]->(b:node)];");
    assert!(compile("(a:node) =[:weighted]=>[-[:next {when: false}]->(b:node)];").is_err());
    assert!(compile("(a:node) =[:weighted]=>[-[:link {weight: 1}, :link {weight: 2}]-(b:node)];").is_err());
    for props in ["", "{items: 1}", "{items: [||], concurrency: 0}"] {
        assert!(compile(&format!("(a:node) =[:each {props}]=>[-[:next]->(b:node)];")).is_err());
    }
    let source = r#"
        +=[batch::each {}]=; +>[sum::reduce {}]>>;
        (a:node) =[work:batch {items: context.records}]=>[-[done:next]->(b:node) {item: context.item, index: context.index}]
            >[:sum {observe: "result", initial: 0, combine: context.accumulator + context.arrival.value, on_failure: "skip"} [done]]>>(c:node);
    "#;
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
    assert!(PROVIDER_SOURCE[span.range.start..span.range.end].starts_with("+(span::node"));
    assert!(COMPILATION_SOURCE[local.range.start..local.range.end].starts_with("+(patient_lookup::tool_span"));
    let metadata = local.fields.iter().find(|field| field.name == "metadata").unwrap();
    assert_eq!(metadata.range.source, 1);
}

#[test]
fn supplied_kinds_cover_every_construct_and_cross_source_forward_inheritance() {
    let definitions = [
        Source {
            name: "derived.bt",
            text: "+(tool_span::span {}); +-[delivery::feeds {}]-; +=[split::parallel {}]=; +>[join::accumulate {}]>>;",
        },
        Source {
            name: "base.bt",
            text: "+(span::node {fields: {id: {type: \"string\"}}});",
        },
    ];
    let source = Source {
        name: "user.bt",
        text: r#"
        +(special::tool_span {});
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
        ("+(span::node {});", "+(span::node {});", "", "duplicate kind", 0),
        ("(a:node);", "+(span::node {});", "+(span::node {});", "duplicate kind", 2),
        ("(a:node);", "+(node::node {});", "", "reserved", 1),
        ("(a:node);", "+(span::missing {});", "", "unknown base", 1),
        ("(a:node);", "+(span::tool_span {});", "+(tool_span::span {});", "circular", 1),
        ("(a:node);", "+(span::delivery {});", "+-[delivery::link {}]-;", "crosses", 1),
        (
            "+(local::span {props: {name: {required: false}}});",
            "+(span::node {props: {name: {type: \"string\", required: true}}});",
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
        ("# é\n+(span::node {", DiagPhase::Parsing),
        (
            "# é\n+(span::node {fields: {id: {type: \"unknown\"}}});",
            DiagPhase::Validation,
        ),
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
    assert_eq!(model.kinds.len(), 2);
    assert!(
        compile_module_with_definitions(
            source,
            &[Source {
                name: "invalid.bt",
                text: "+(span::missing {});",
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
            text: "(a:custom);",
        },
        &[],
    )
    .unwrap();
    assert_eq!(model.sources, ["user.bt"]);
    assert_eq!(model.nodes[0].range.source, 0);
}
