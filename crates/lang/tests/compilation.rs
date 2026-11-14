use bts_lang::SrcRange;
use bts_lang::syntax::{
    BinOp, Expr, ExprKind, GraphNode, GraphNodeDecl, GraphOrigin, GraphReferenceKind, GraphStep, ObjectItem, TemplatePart,
    parse,
};
use bts_lang::{DiagPhase, Value, semantics::compile, syntax::parse_traversal};

const COMPILATION_SOURCE: &str = include_str!("fixtures/compilation.bt");

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
    let source = "(a)=[:parallel]=>[\n    -[to_b:next]->(b),\n    -[to_c:next]->(c),\n    -[to_d:next]->(d),\n]\n    >>[[to_b], [to_c]]->(e)\n    >>[[to_c], [to_d]]->(f);";
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
    let source = ">>[[checks].[coverage], [records],]->(ready:task) {input: [records].scope}\n    -[:next]->(finish);";
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
    let source = "(a)\n  =[checks:parallel]=>[-[lookup:next]->(:tool), -[note:next]->(b)]\n    ->([lookup].target)~[:again]~>(a)\n    >>[[lookup], [note]]->(done);";
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
        "(a)=[:parallel]=>[-[work:next]->(b)] >>[[work]]->(c);",
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
        "(a)=[:parallel]=>[-[work:next]->(b)] ->(b) >>[[work]]->(c);",
        "(a)=[:parallel]=>[-[work:next]->(b)] >>[[work]]->(c) ->(b);",
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
        ">>[(a), (b)]->(c);",
        ">>[[a:next], [b]]->(c);",
        ">>[[a].target, [b]]->(c);",
        ">>[[a], [b]];",
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
    assert_eq!(graph.paths.len(), 30);
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
    let source =
        "(a)=[work:parallel]=>[-[one:next]->(b), -[two:next]->(c)]\n    ->(b)-[:next]->(d)\n    >>[[one], [two]]->(finish);";
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
    let source = "(a)-[:next]->(?<(b)-[:next]->(@:tool)>); >>[?<(a)-[@:next]->(b)>, [other]]->(done); (values:vars) {name: ?<(a)-[:next]->(@:tool)>.props.name, text: \"Found ${?<(@:tool)>.props.name}\"};";
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
    let model = compile(COMPILATION_SOURCE).unwrap_or_else(|errors| {
        panic!(
            "{}",
            errors
                .iter()
                .map(|e| e.render("compilation.bt", COMPILATION_SOURCE))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    assert_eq!(model.paths.len(), 30);
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
        "(a:x)\n    =[f:parallel]=>[-[eb:next]->(b:x), -[ec:next]->(c:x), -[ed:next]->(d:x)]\n        >>[[eb], [ec]]->(e:x)\n        >>[[ec], [ed]]->(g:x);",
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
        "(a:x) =[f:parallel]=>[-[eb:next]->(b:x)] >>[[ec]]->(d:x); (c:x)-[ec:next]->(b);",
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
