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
+(leg:node {to: {type: "string", required: true}}) {gives: {type: "string", value: self.props.to}};
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
fn an_author_changes_what_a_shipped_kind_computes_by_extending_it() {
    // the provider decides what its kinds compute, and an extension states a field again
    let world = r#"+(shout:node) {text: {value: upper(realize(self.sentence))}}
    -[:extends]->(sentence);
+(numbered:node) {text: {value: format("{} rules. {}", len(self.items), compose(self.items))}}
    -[:extends]->(list);
+(past:node) {defaults: {value: {tense: "past"}}}
    -[:extends]->(sentence);
+(asked:node {form: {default: "question"}})
    -[:extends]->(part);
+(house_rule:node {subject: {default: "the house"}})
    -[:extends]->(policy);
(ask:shout)
    -[:next]->(:part {subject: "I", person: "first", modal: "must"})
    -[:next]->(:part {verb: "change", complement: "my flight"});
(rules:numbered)
    -[:next]->(:policy {subject: "a change", verb: "cost", complement: "75 USD"})
    -[:next]->(:house_rule {verb: "close", complement: "at noon"});
(then:past)
    -[:next]->(:part {subject: "the desk", verb: "close"});
(may:sentence)
    -[:next]->(:asked {subject: "I", person: "first", modal: "can", verb: "go"});"#;
    let read = "{said: ask.text, clause: ask.sentence, rules: rules.text, items: rules.items, then: then.text, may: may.text}";
    let output = outputs(&shape(world, read), 1..2).remove(0);
    assert_eq!(output["said"], "I MUST CHANGE MY FLIGHT.");
    // what the extension did not state is still the shipped kind's
    assert_eq!(output["clause"]["clause"]["modal"], "must");
    assert_eq!(output["rules"], "2 rules. A change costs 75 USD. The house closes at noon.");
    assert_eq!(
        output["items"],
        json!(["A change costs 75 USD.", "The house closes at noon."])
    );
    assert_eq!(output["then"], "The desk closed.");
    assert_eq!(output["may"], "Can I go?");
    // an instance starts from defaults of its own too, and a part still wins over them
    let world = r#"(was:sentence) {defaults: {tense: "past", subject: "the desk"}}
    -[:next]->(:part {verb: "open"});
(is:sentence) {defaults: {tense: "past", subject: "the desk"}}
    -[:next]->(:part {verb: "open", tense: "present"});"#;
    assert_eq!(
        outputs(&shape(world, "[|was.text, is.text|]"), 1..2),
        [json!(["The desk opened.", "The desk opens."])]
    );
    // an instance states a field of its own in the same way
    let world = r#"(ask:sentence) {text: "whatever is written here"}
    -[:next]->(:part {subject: "I", person: "first", verb: "go"});"#;
    assert_eq!(
        outputs(&shape(world, "[|ask.text, ask.grammar.verb|]"), 1..2),
        [json!(["whatever is written here", "go"])]
    );
}

#[test]
fn a_walk_says_what_it_is_drawn_for() {
    // three traces in a row are one conversation, the house is drawn once for the run, the weather for each trace
    let fork = |verbs: [&str; 4]| {
        let branches: Vec<_> = (verbs.iter())
            .map(|verb| format!("        -[:next {{weight: 1}}]->(:part {{verb: \"{verb}\"}}),"))
            .collect();
        format!("    =[:weighted]=>[\n{}\n    ];", branches.join("\n"))
    };
    let world = format!(
        "(mood:sentence {{per: floor(context.trace_index / 3)}})\n    -[:next]->(:part {{subject: \"I\", person: \"first\"}})\n{}\n(house:sentence {{per: \"run\"}})\n    -[:next]->(:part {{subject: \"the desk\"}})\n{}\n(each:sentence)\n    -[:next]->(:part {{subject: \"it\"}})\n{}",
        fork(["wait", "leave", "stay", "go"]),
        fork(["open", "close", "change", "go"]),
        fork(["open", "close", "change", "go"]),
    );
    let source = shape(&world, "[|mood.text, house.text, each.text|]");
    let model = SELECTED
        .compile(&source)
        .unwrap_or_else(|errors| panic!("{errors}\n{source}"));
    let batch = data::generate(
        model,
        30,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        5,
        &(),
    )
    .unwrap();
    let said: Vec<_> = (batch.events.iter())
        .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
        .collect();
    assert_eq!(said.len(), 30);
    for conversation in said.chunks(3) {
        assert!(
            conversation.iter().all(|turn| turn[0] == conversation[0][0]),
            "{conversation:?}"
        );
    }
    let distinct = |index: usize| said.iter().map(|turn| turn[index].to_string()).collect::<BTreeSet<_>>().len();
    assert!(distinct(0) > 1 && distinct(2) > 1);
    assert_eq!(distinct(1), 1);
    // conversations differ from one another, and so do the traces within one
    assert!(
        said.chunks(3)
            .any(|conversation| conversation.iter().any(|turn| turn[2] != conversation[0][2]))
    );
    // what a walk is drawn for is a string or a whole number
    let half = source.replace("floor(context.trace_index / 3)", "0.5");
    let model = SELECTED.compile(&half).unwrap();
    let error = data::generate(
        model,
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        5,
        &(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("a walk is drawn per a string or a whole number"),
        "{error}"
    );
}

#[test]
fn a_conversation_is_several_traces_that_belong_together() {
    // three traces in a row are one conversation, and the traveler keeps one mood through it
    let world = r#"(chat:conversation {turns: 3});
(mood:sentence {per: chat.number})
    -[:next]->(:part {subject: "I", person: "first"})
    =[:weighted]=>[
        -[:next {weight: 1}]->(:part {verb: "wait"}),
        -[:next {weight: 1}]->(:part {verb: "leave"}),
        -[:next {weight: 1}]->(:part {verb: "stay"}),
        -[:next {weight: 1}]->(:part {verb: "go"}),
    ];"#;
    let read = "{id: chat.id, number: chat.number, turn: chat.turn, length: chat.length, first: chat.first, last: chat.last, mood: mood.text}";
    let source = shape(world, read);
    let traces = |source: &str, seed: u64, count: usize| -> Vec<Value> {
        let model = SELECTED.compile(source).unwrap_or_else(|errors| panic!("{errors}\n{source}"));
        let batch = data::generate(
            model,
            count,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        (batch.events.iter())
            .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
            .collect()
    };
    let turns = |source: &str, seed: u64| traces(source, seed, 30);
    let said = turns(&source, 5);
    let mut ids = BTreeSet::new();
    for (number, conversation) in said.chunks(3).enumerate() {
        for (turn, trace) in conversation.iter().enumerate() {
            // every trace of a conversation carries its identifier and sees the same world
            assert_eq!(trace["id"], conversation[0]["id"]);
            assert_eq!(trace["mood"], conversation[0]["mood"]);
            assert_eq!(trace["number"], number);
            assert_eq!(trace["turn"], turn);
            assert_eq!(trace["length"], 3);
            assert_eq!(trace["first"], turn == 0);
            assert_eq!(trace["last"], turn == 2);
        }
        let id = conversation[0]["id"].as_str().unwrap();
        assert!(id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        ids.insert(id.to_owned());
    }
    // each conversation has its own identifier, and they do not all share a mood
    assert_eq!(ids.len(), 10);
    assert!(said.iter().any(|trace| trace["mood"] != said[0]["mood"]));
    // the same seed gives the same conversations, and another seed gives others
    assert_eq!(said, turns(&source, 5));
    assert_ne!(said[0]["id"], turns(&source, 6)[0]["id"]);
    // without turns every trace is its own conversation
    let single = turns(&source.replace("{turns: 3}", ""), 5);
    assert!(single.iter().all(|trace| trace["first"] == true && trace["last"] == true));
    assert_eq!(
        single
            .iter()
            .map(|trace| trace["id"].to_string())
            .collect::<BTreeSet<_>>()
            .len(),
        30
    );
    // an extension says where a conversation ends in its own way
    let paired = format!(
        "+(pair:node) {{number: {{value: floor(context.trace_index / 2)}}, turn: {{value: context.trace_index % 2}}}}\n    -[:extends]->(conversation);\n{}",
        source.replace("(chat:conversation {turns: 3})", "(chat:pair)")
    );
    let paired = turns(&paired, 5);
    assert_eq!(paired[0]["id"], paired[1]["id"]);
    assert_ne!(paired[1]["id"], paired[2]["id"]);
    assert_eq!(paired[3]["turn"], 1);
    // with up_to each conversation runs for its own number of turns, and the traces still line up
    let varied = source.replace("{turns: 3}", "{turns: 2, up_to: 6}");
    let said = traces(&varied, 5, 600);
    let mut lengths = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut at = 0;
    while at < said.len() {
        let opening = &said[at];
        let length = opening["length"].as_u64().unwrap() as usize;
        assert_eq!(opening["turn"], 0);
        assert_eq!(opening["first"], true);
        // only a conversation that a block of 256 traces cuts short has fewer turns than the fewest
        assert!((2..=6).contains(&length) || (at + length).is_multiple_of(256), "{opening}");
        for (turn, trace) in said[at..].iter().take(length).enumerate() {
            assert_eq!(trace["id"], opening["id"]);
            assert_eq!(trace["mood"], opening["mood"]);
            assert_eq!(trace["turn"], turn);
            assert_eq!(trace["length"], length);
            assert_eq!(trace["last"], turn == length - 1);
        }
        assert!(ids.insert(opening["id"].to_string()), "{opening}");
        lengths.insert(length);
        at += length;
    }
    assert!((2..=6).all(|length| lengths.contains(&length)), "{lengths:?}");
    assert_eq!(said, traces(&varied, 5, 600));
    // the bounds are whole numbers of turns, the second no smaller than the first
    for (written, message) in [
        (
            "{turns: 4, up_to: 2}",
            "a conversation runs up to at least as many turns as it starts with",
        ),
        ("{turns: 0}", "a conversation has a whole number of turns from 1 to 256"),
        (
            "{turns: 2, up_to: 2.5}",
            "a conversation has a whole number of turns from 1 to 256",
        ),
    ] {
        let model = SELECTED.compile(&source.replace("{turns: 3}", written)).unwrap();
        let error = data::generate(
            model,
            1,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            5,
            &(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    // a conversation has no steps
    let found = rejected(&shape(
        "(chat:conversation)-[:next]->(:policy {subject: \"a\", verb: \"be\"});",
        "chat.id",
    ));
    assert!(found.contains("a conversation has no steps, so nothing leaves it"), "{found}");
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
            "+(stop:node {at: {type: \"string\"}});\n(w:list)-[:next]->(p:stop {at: \"noon\"});".to_owned(),
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
