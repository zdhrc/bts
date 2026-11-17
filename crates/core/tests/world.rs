//! A world graph stands beside an agent, and the world provider builds it by walking.

use bts_core::mocking::data;
use bts_core::providers::{Contributor, Provider, With, braintrust, conformance, world};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, UNIX_EPOCH};

const SELECTED: With<braintrust::Bundle, world::Bundle> = With(braintrust::Bundle, world::Bundle);

// an agent of one step whose output reads the world
fn shape(world: &str, read: &str) -> String {
    format!(
        "{world}\n(a:agent) {{input: \"hi\", output: t.output}};\n(a)-[:next]->(t:task {{name: \"t\"}}) {{input: a.input, output: {read}}};\n"
    )
}

// the output of the first trace of each seed
fn outputs(source: &str, seeds: std::ops::Range<u64>) -> Vec<Value> {
    let model = SELECTED.compile(source).unwrap_or_else(|errors| panic!("{errors}\n{source}"));
    seeds
        .map(|seed| {
            let batch = data::generate(
                model.clone(),
                1,
                Duration::from_secs(3600),
                data::Distribution::Linear,
                UNIX_EPOCH + Duration::from_secs(7200),
                seed,
                &(),
            )
            .unwrap();
            batch.events[0].output.as_ref().unwrap().project(&()).unwrap()
        })
        .collect()
}

fn rejected(source: &str) -> String {
    let errors = SELECTED.compile_module(source, false).unwrap_err();
    errors.into_iter().map(|error| error.what).collect::<Vec<_>>().join("\n")
}

const ASK: &str = r#"(ask:sentence)
    -[:next]->(:part {subject: "I", person: "first"})
    =[:weighted]=>[
        -[:next {weight: 2}]->(can:part {modal: "can", form: "question"}),
        -[:next {weight: 1}]->(:part {modal: "could", form: "question"}),
        -[:next {weight: 1}]->(:part {modal: "must"}),
    ]
    =[:weighted]=>[
        -[:next {weight: 1}]->(:part {verb: "change", complement: "my flight"}),
        -[:next {weight: 1}]->(:part {verb: "switch", complement: "to a later flight"}),
    ];"#;

#[test]
fn the_world_provider_walks_its_own_elements_and_conforms() {
    assert_eq!(conformance::check(&world::Bundle), Vec::<String>::new());
    assert_eq!(conformance::check(&SELECTED), Vec::<String>::new());
    let source = shape(ASK, "ask.text");
    let model = bts_lang::semantics::compile_module_with_provider(
        bts_lang::semantics::Source {
            name: "world.bt",
            text: &source,
        },
        &SELECTED.schema(),
        false,
    )
    .unwrap();
    // the walk, its six parts, its edges and its two forks belong to the world and to nothing else
    let walked = world::Bundle.elements(&model);
    let count = |pick: fn(&bts_lang::ElementId) -> bool| walked.iter().filter(|element| pick(element)).count();
    assert_eq!(count(|element| matches!(element, bts_lang::ElementId::Node(_))), 7);
    assert_eq!(count(|element| matches!(element, bts_lang::ElementId::Edge(_))), 6);
    assert_eq!(count(|element| matches!(element, bts_lang::ElementId::Fork(_))), 2);
    let agent = braintrust::Bundle.elements(&model);
    assert!(walked.iter().all(|element| !agent.contains(element)));
    assert_eq!(SELECTED.elements(&model).len(), walked.len() + agent.len());
    // it supplies the steps of the walk, and hides the steps a walk may not visit
    let supplied = world::Bundle.supply(&model).unwrap();
    assert_eq!(supplied.context.len(), 1);
    assert_eq!(supplied.context[0].key, "steps");
    assert_eq!(supplied.hidden.len(), 5);
}

#[test]
fn a_sentence_is_built_from_the_parts_a_walk_visits() {
    let said = outputs(&shape(ASK, "ask.text"), 0..64);
    let wordings: BTreeSet<_> = said.iter().map(|text| text.as_str().unwrap()).collect();
    assert_eq!(
        wordings.into_iter().collect::<Vec<_>>(),
        [
            "Can I change my flight?",
            "Can I switch to a later flight?",
            "Could I change my flight?",
            "Could I switch to a later flight?",
            "I must change my flight.",
            "I must switch to a later flight.",
        ]
    );
    // the heavier branch is taken more often, and the same seed gives the same sentence
    let can = said.iter().filter(|text| text.as_str().unwrap().starts_with("Can ")).count();
    assert!((20..=44).contains(&can), "{can}");
    assert_eq!(said, outputs(&shape(ASK, "ask.text"), 0..64));
    // every read in a trace sees the same walk
    for output in outputs(
        &shape(
            ASK,
            "{first: ask.text, again: ask.text, grammar: ask.grammar, clause: ask.sentence}",
        ),
        0..16,
    ) {
        assert_eq!(output["first"], output["again"]);
        assert_eq!(output["grammar"]["verb"], output["clause"]["clause"]["predicate"]["verb"]);
        assert_eq!(output["grammar"]["person"], "first");
        assert_eq!(output["grammar"]["tense"], "present");
    }
}

#[test]
fn a_list_has_one_item_for_each_step() {
    let world = r#"(policies:list)
    -[:next]->(fee:policy {subject: "a same-day change", verb: "cost", complement: "75 USD"})
    -[:next]->(window:policy {subject: "the change window", verb: "close", complement: "two hours before departure"});"#;
    let read = "{items: policies.items, text: policies.text, fee: fee.text, clause: fee.sentence}";
    let output = outputs(&shape(world, read), 1..2).remove(0);
    assert_eq!(
        output["items"],
        json!([
            "A same-day change costs 75 USD.",
            "The change window closes two hours before departure."
        ])
    );
    assert_eq!(
        output["text"],
        "A same-day change costs 75 USD. The change window closes two hours before departure."
    );
    // a step every walk visits is read by name
    assert_eq!(output["fee"], "A same-day change costs 75 USD.");
    assert_eq!(output["clause"]["clause"]["predicate"]["verb"], "cost");
}

#[test]
fn a_walk_takes_the_branch_whose_guard_holds() {
    let world = r#"(said:sentence)
    -[:next]->(mood:part {subject: "I", person: "first", tense: "past"})
    =[:exclusive]=>[
        -[:next {when: mood.props.tense == "past"}]->(:part {verb: "go"}),
        -[:next {when: mood.props.tense != "past"}]->(:part {verb: "stay"}),
    ];"#;
    assert_eq!(outputs(&shape(world, "said.text"), 1..2), [json!("I went.")]);
    let present = world.replace(r#"tense: "past"}"#, r#"tense: "present"}"#);
    assert_eq!(outputs(&shape(&present, "said.text"), 1..2), [json!("I stay.")]);
}

#[test]
fn steps_in_a_chain_are_walked_like_steps_in_a_fan() {
    // each step leads to the next, and the branches rejoin by hand
    let world = r#"(said:sentence)-[:next]->(who:part {subject: "I", person: "first"});
(who)
    =[:weighted]=>[
        -[:next {weight: 1}]->(can:part {modal: "can"}),
        -[:next {weight: 1}]->(must:part {modal: "must"}),
    ];
(can)-[:next]->(what);
(must)-[:next]->(what);
(what:part {verb: "go"});"#;
    let said: BTreeSet<_> = (outputs(&shape(world, "said.text"), 0..32).into_iter())
        .map(|text| text.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(said.into_iter().collect::<Vec<_>>(), ["I can go.", "I must go."]);
    // a branch of several steps gives several values, and a fork that is skipped shifts no other draw
    let world = r#"(said:sentence)
    -[:next]->(:part {subject: "I", person: "first"})
    =[:weighted]=>[
        -[:next {weight: 1}]->(polite:part {form: "question"}),
        -[:next {weight: 1}]->(:part {modal: "must"}),
    ]
    =[:weighted]=>[
        -[:next {weight: 1}]->(:part {verb: "go"}),
        -[:next {weight: 1}]->(:part {verb: "stay"}),
    ];
(polite)
    =[:weighted]=>[
        -[:next {weight: 1}]->(:part {modal: "can"}),
        -[:next {weight: 1}]->(:part {modal: "could"}),
    ];"#;
    let said: BTreeSet<_> = (outputs(&shape(world, "said.text"), 0..96).into_iter())
        .map(|text| text.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        said.into_iter().collect::<Vec<_>>(),
        [
            "Can I go?",
            "Can I stay?",
            "Could I go?",
            "Could I stay?",
            "I must go.",
            "I must stay."
        ]
    );
}

#[test]
fn an_author_declares_a_walk_and_its_steps() {
    let world = r#"+(itinerary:node) {legs: {type: "array", value: context.steps}}-[:extends]->(walk);
+(leg:node {to: {type: "string", required: true}}) {gives: {type: "string", value: self.props.to}}-[:extends]->(vars);
(trip:itinerary)
    -[:next]->(:leg {to: "Lisbon"})
    -[:next]->(:leg {to: "Porto"});"#;
    assert_eq!(outputs(&shape(world, "trip.legs"), 1..2), [json!(["Lisbon", "Porto"])]);
    // merge adds up what steps give: later keys win and null changes nothing
    let merged = r#"merge({tense: "present", modal: null}, [|{modal: "can"}, {tense: null}, null, {modal: "must"}|])"#;
    assert_eq!(
        outputs(&shape("", merged), 1..2),
        [json!({"tense": "present", "modal": "must"})]
    );
}

#[test]
fn a_world_graph_rejects_what_a_walk_cannot_take() {
    let policy = r#"(p:policy {subject: "a", verb: "be"})"#;
    for (world, read, message) in [
        (
            format!(
                "(w:list)\n    =[:weighted]=>[\n        -[x:next {{weight: 1}}]->{policy},\n        -[y:next {{weight: 1}}]->(q:policy {{subject: \"b\", verb: \"be\"}}),\n    ]\n        >[:first [x], [y]]>>(r:policy {{subject: \"c\", verb: \"be\"}});"
            ),
            "w.text",
            "junction kind `first` is not supported by World",
        ),
        (
            format!("(w:list)\n    =[:parallel]=>[\n        -[:next]->{policy},\n    ];"),
            "w.text",
            "fork kind `parallel` is not supported by World",
        ),
        (
            format!("(w:list)-[:calls]->{policy};"),
            "w.text",
            "a walk follows `next` edges, not `calls`",
        ),
        (
            "(w:list)-[:next]->(s:sentence)-[:next]->(:part {subject: \"a\", verb: \"be\"});".to_owned(),
            "w.text",
            "a walk cannot be a step of another walk yet",
        ),
        (
            "(w:list)-[:next]->(p:vars) {x: 1};".to_owned(),
            "w.text",
            "this step gives nothing to `w`, its kind needs a `gives` field",
        ),
        (
            format!("(w:list)-[:next {{when: true}}]->{policy};"),
            "w.text",
            "a step outside a fork is always taken, so its edge takes no properties",
        ),
        (
            format!("(w:list)\n    =[:weighted]=>[\n        -[:next {{weight: 1, when: true}}]->{policy},\n    ];"),
            "w.text",
            "a weighted fork in a walk chooses by weight alone",
        ),
        // a step behind a fork may not be visited, so nothing outside reads what it computes
        (
            format!(
                "(w:list)\n    =[:weighted]=>[\n        -[:next {{weight: 1}}]->{policy},\n        -[:next {{weight: 1}}]->(q:policy {{subject: \"b\", verb: \"be\"}}),\n    ];"
            ),
            "p.text",
            "`p` is behind a fork, so a walk may not visit it. Read `w`, the walk it belongs to",
        ),
        // the steps of a walk are not a key an agent's step can read
        (
            format!("(w:list)-[:next]->{policy};"),
            "context.steps",
            "context key `steps` is not available here",
        ),
    ] {
        let found = rejected(&shape(&world, read));
        assert!(found.contains(message), "{message}\n{found}\n{world}");
    }
    // a walk stands beside the agent, never on its path
    let on_path = format!(
        "(a:agent) {{input: \"hi\", output: t.output}};\n(a)-[:next]->(t:task {{name: \"t\"}}) {{input: a.input, output: w.text}};\n(t)-[:calls]->(w:list)-[:next]->{policy};"
    );
    let found = rejected(&on_path);
    assert!(
        found.contains("this belongs to Braintrust mocking and to World, and an element belongs to one of them"),
        "{found}"
    );
    // without the world provider nothing walks the graph, so the read is still refused
    let alone = braintrust::Bundle
        .compile_module(&shape("(w:vars) {text: hex(4)};", "w.text"), false)
        .unwrap_err();
    assert_eq!(alone[0].what, "external value references need one owning agent");
}
