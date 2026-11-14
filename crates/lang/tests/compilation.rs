use bts_lang::SrcRange;
use bts_lang::syntax::{
    BinOp, Expr, ExprKind, GraphNode, GraphNodeDecl, GraphOrigin, GraphReferenceKind, GraphStep, ObjectItem, TemplatePart,
    parse,
};
use bts_lang::{Accessor, Child, DiagPhase, Field, ScorerKind, Value, semantics::compile, syntax::parse_traversal};

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
fn distinguishes_parent_and_self_reference_scopes() {
    let model = compile(
        r#"trace "run" {
        input = "question"
        task "answer" { input = trace.input output = self.input }
    }"#,
    )
    .unwrap();
    let Child::Span(task) = &model.traces[0].children[0] else {
        panic!("expected task");
    };
    for (field, up) in [(&task.fields.input, 1), (&task.fields.output, 0)] {
        let Some(Value::BlockRef { ref_id, .. }) = field else {
            panic!("expected reference");
        };
        let reference = &model.refs[ref_id.0 as usize];
        assert_eq!(reference.up, up);
        assert_eq!(reference.module, None);
        assert!(reference.steps.is_empty());
        assert!(matches!(reference.accessor, Accessor::Field(Field::Input)));
    }
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
    assert!(bts_lang::semantics::compile_module("", true).unwrap().traces.is_empty());
}

#[test]
fn keeps_dynamic_dataset_source_restrictions_in_validation() {
    let source = r#"
        trace "run" { repeat "turns" { count = 2 task "answer" {} } }
        dataset "cases" { case "answer" { span = trace.run.repeat.turns[0].task.answer } }
    "#;
    let errors = compile(source).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.when == DiagPhase::Validation && error.what.contains("static path")),
        "{errors:?}"
    );
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
fn reports_source_locations_for_invalid_source() {
    let source = "trace \"broken\" {\n    input = }\n}";
    let errors = compile(source).unwrap_err();
    let error = errors.iter().find(|error| error.when == DiagPhase::Parsing).unwrap();
    let rendered = error.render("broken.bt", source);

    assert_eq!(&source[error.r#where.start..error.r#where.end], "}");
    assert!(rendered.contains("broken.bt:2:13: parsing error:"));
    assert!(rendered.contains("2 |     input = }"));
    assert!(rendered.contains("^ (byte 29)"));
}

#[test]
fn rejects_a_reference_to_a_missing_span() {
    let source = "trace \"run\" { output = task.missing.output }";
    let errors = compile(source).unwrap_err();

    assert!(errors.iter().any(|error| error.when == DiagPhase::Validation));
    assert!(errors.iter().any(|error| error.what.contains("missing")));
}

#[test]
fn parses_public_traversal_paths() {
    assert_eq!(
        parse_traversal("trace[\"run\"].task[\"answer\"].output").unwrap(),
        ["trace", "run", "task", "answer", "output"]
    );
    assert!(parse_traversal("trace[0].output").is_err());
}

#[test]
fn consumers_can_name_nested_model_types() {
    use bts_lang::{Facet, Func, JudgeOption, ScorerSpan, ScorerStep, Span, WeightedOption, When};
    let model = compile(
        r#"
        trace "run" {
            input = weighted(["question", 2], ["followup", 1])
            task "answer" {}
            scorer "quality" { score = 1 }
        }
        scorer "quality" { code { score = 1 when { cond = output == null score = 0 } } }
        scorer "judge" { judge { model = "gpt-4o-mini" prompt = "Evaluate ${output}." options = { good = 1, bad = 0 } } }
        facet "Topic" { prompt = "Identify the topic." }
    "#,
    )
    .unwrap();
    let [Child::Span(span), Child::Scorer(score)] = model.traces[0].children.as_slice() else {
        panic!("expected span and score");
    };
    let span: &Span = span;
    let score: &ScorerSpan = score;
    assert_eq!(span.name, "answer");
    assert_eq!(score.name, "quality");
    let facet: &Facet = &model.facets[0];
    assert_eq!(facet.prompt, "Identify the topic.");
    let Some(Value::Func {
        func: Func::Weighted(options),
        ..
    }) = &model.traces[0].fields.input
    else {
        panic!("expected weighted input");
    };
    let option: &WeightedOption = &options[0];
    assert_eq!(option.weight, 2.0);
    let ScorerKind::Code { steps, .. } = &model.scorers[0].kind else {
        panic!("expected code scorer");
    };
    let ScorerStep::When(when) = &steps[0] else {
        panic!("expected condition");
    };
    let when: &When = when;
    assert!(when.score.is_some());
    let ScorerKind::Judge { options, .. } = &model.scorers[1].kind else {
        panic!("expected judge");
    };
    let option: &JudgeOption = &options[0];
    assert_eq!(option.label, "good");
}

#[test]
fn exposes_agents_with_resolved_trace_entrypoints() {
    let model = compile(
        r#"
        agent "support" { entrypoint = trace.request }
        trace "request" { output = "hello" }
        agent "preview" { entrypoint = trace.request }
        "#,
    )
    .unwrap();
    assert_eq!(model.agents.len(), 2);
    for (agent, name) in model.agents.iter().zip(["support", "preview"]) {
        let agent: &bts_lang::Agent = agent;
        assert_eq!(agent.name, name);
        let reference = &model.refs[agent.entrypoint.0 as usize];
        assert_eq!(reference.module, Some(model.traces[0].node));
        assert!(matches!(reference.accessor, Accessor::Block { node, kind: "trace" }
            if node == model.traces[0].node));
        assert!(reference.steps.is_empty());
        assert!(reference.path.is_empty());
    }
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
