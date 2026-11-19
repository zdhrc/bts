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
    // it supplies the steps of the walk
    let supplied = world::Bundle.supply(&model).unwrap();
    let keys: Vec<_> = supplied.context.iter().map(|supply| supply.key.as_str()).collect();
    // with its steps a walk is given what it is related to and its draws, and null for the speaker nothing leads to it with
    assert_eq!(keys, ["steps", "related", "draws", "speaker"]);
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
            "{first: ask.text, again: ask.text, grammar: ask.grammar, clause: ask.clause}",
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
    let read = "{items: policies.items, text: policies.text, fee: fee.text, clause: fee.clause}";
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
    let world = r#"+(shout:node) {text: {value: upper(realize(self.clause))}}
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
    let read = "{said: ask.text, clause: ask.clause, rules: rules.text, items: rules.items, then: then.text, may: may.text}";
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
fn a_walk_is_a_step_of_another_walk() {
    // one policy of the list varies, and it is read by name and through the list alike
    let world = r#"(policies:list)
    -[:next]->(fee:sentence)
        -[:next]->(:part {subject: "a same-day change", verb: "cost"})
        =[:weighted]=>[
            -[:next {weight: 1}]->(:part {complement: "75 USD"}),
            -[:next {weight: 1}]->(:part {complement: "50 USD"}),
        ]
    -[:next]->(window:policy {subject: "the change window", verb: "close", complement: "at noon"});"#;
    let said = outputs(
        &shape(
            world,
            "{fee: fee.text, clause: fee.clause, all: policies.text, items: policies.items}",
        ),
        0..24,
    );
    let fees: BTreeSet<_> = said.iter().map(|trace| trace["fee"].as_str().unwrap().to_owned()).collect();
    assert_eq!(
        fees.into_iter().collect::<Vec<_>>(),
        ["A same-day change costs 50 USD.", "A same-day change costs 75 USD."]
    );
    for trace in &said {
        let fee = trace["fee"].as_str().unwrap();
        assert_eq!(trace["all"], format!("{fee} The change window closes at noon."));
        assert_eq!(trace["items"], json!([fee, "The change window closes at noon."]));
        assert!(fee.contains(trace["clause"]["clause"]["predicate"]["complement"].as_str().unwrap()));
    }
}

#[test]
fn a_walk_that_is_a_step_is_drawn_for_what_its_walk_is_drawn_for() {
    // the sentence does not say, so its fork is drawn once for each conversation like the persona it is a step of
    let world = r#"(chat:series {turns: 4});
(customer:persona {per: chat.number})
    -[:next]->(:sentence)
        -[:next]->(:part {subject: "I", person: "first"})
        =[:weighted]=>[
            -[:next {weight: 1}]->(:part {verb: "need", complement: "a refund"}),
            -[:next {weight: 1}]->(:part {verb: "want", complement: "my money back"}),
            -[:next {weight: 1}]->(:part {verb: "have", complement: "a billing problem"}),
        ];"#;
    let said = |world: &str| -> Vec<String> {
        let source = shape(world, "customer.said[0]");
        let model = SELECTED
            .compile(&source)
            .unwrap_or_else(|errors| panic!("{errors}\n{source}"));
        let batch = data::generate(
            model,
            48,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            3,
            &(),
        )
        .unwrap();
        (batch.events.iter())
            .map(|event| {
                event
                    .output
                    .as_ref()
                    .unwrap()
                    .project(&())
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    let turns = said(world);
    assert_eq!(turns.len(), 48);
    for conversation in turns.chunks(4) {
        assert!(conversation.iter().all(|turn| *turn == conversation[0]), "{conversation:?}");
    }
    // conversations still differ from one another
    assert!(turns.iter().collect::<BTreeSet<_>>().len() > 1);
    // a step that says it is drawn for each trace is, whatever its walk is drawn for
    let each_trace = said(&world.replace("(:sentence)", "(:sentence {per: context.trace_index})"));
    assert!(
        each_trace
            .chunks(4)
            .any(|conversation| conversation.iter().any(|turn| *turn != conversation[0]))
    );
}

#[test]
fn a_fact_has_a_value_and_an_entity_has_attributes() {
    let world = r#"(fare:fact)
    =[:weighted]=>[
        -[:next {weight: 1}]->(:given {value: 1240.5}),
        -[:next {weight: 1}]->(:given {value: 980}),
    ];
(flight:entity)
    -[:next]->(:given {value: {date: "2026-11-20"}})
    =[:weighted]=>[
        -[:next {weight: 1}]->(:given {value: {departs: "17:30"}}),
        -[:next {weight: 1}]->(:given {value: {departs: "19:05"}}),
    ];
(traveler:entity)
    -[:next]->(:given {value: {name: a.input}});
(holds:relation);
(holds)-[:incidence]-(traveler);
(holds)-[:incidence]-(flight);"#;
    let read = "{fare: fare.value, flight: flight.attributes, departs: flight.attributes.departs, holds: traveler.related.holds, held_by: flight.related.holds, who: traveler.attributes}";
    let said = outputs(&shape(world, read), 0..24);
    let distinct = |key: &str| said.iter().map(|trace| trace[key].to_string()).collect::<BTreeSet<_>>();
    assert_eq!(distinct("fare"), BTreeSet::from(["1240.5".to_owned(), "980".to_owned()]));
    assert_eq!(distinct("departs").len(), 2);
    for trace in &said {
        // what never changes is there with what was drawn, and everyone who reads the flight reads the same one
        assert_eq!(trace["flight"]["date"], "2026-11-20");
        assert_eq!(trace["flight"]["departs"], trace["departs"]);
        assert_eq!(trace["holds"], json!([trace["flight"]]));
        // a world reads the agent's input, and a relation is read from both of its ends
        assert_eq!(trace["who"], json!({"name": "hi"}));
        assert_eq!(trace["held_by"], json!([{"name": "hi"}]));
    }
    // an entity that is part of no relation is related to nothing
    assert_eq!(
        outputs(
            &shape("(alone:entity)-[:next]->(:given {value: {a: 1}});", "alone.related"),
            1..2
        ),
        [json!({})]
    );
    // the world a caller meets depends on the input
    let world = r#"(perk:fact)
    =[:exclusive]=>[
        -[:next {when: a.input == "gold"}]->(:given {value: "lounge access"}),
        -[:next {when: a.input != "gold"}]->(:given {value: "a seat"}),
    ];"#;
    assert_eq!(outputs(&shape(world, "perk.value"), 1..2), [json!("a seat")]);
    assert_eq!(
        outputs(&shape(world, "perk.value").replace("input: \"hi\"", "input: \"gold\""), 1..2),
        [json!("lounge access")]
    );
}

#[test]
fn relations_have_a_direction_and_a_setting_gathers_walks() {
    let world = r#"(traveler:entity)-[:next]->(:given {value: {name: "Maya"}});
(booking:entity)-[:next]->(:given {value: {code: "AB42"}});
(spare:entity)-[:next]->(:given {value: {code: "ZZ99"}});
(friend:entity)-[:next]->(:given {value: {name: "Sam"}});
(holds:relation);
(holds)-[:incidence {role: "input"}]-(traveler);
(holds)-[:incidence {role: "output"}]-(booking);
(holds)-[:incidence {role: "output"}]-(spare);
(knows:relation);
(knows)-[:incidence]-(traveler);
(knows)-[:incidence]-(friend);
(trip:setting {nodes: [|(traveler), (booking)|], edges: [||]});"#;
    let read = "{has: traveler.related, held: booking.related.holds, spare: spare.related.holds, friend: friend.related, trip: trip.members}";
    let output = outputs(&shape(world, read), 1..2).remove(0);
    // an input reads the outputs and an output reads the inputs, never its own side
    assert_eq!(output["has"]["holds"], json!([{"code": "AB42"}, {"code": "ZZ99"}]));
    assert_eq!(output["held"], json!([{"name": "Maya"}]));
    assert_eq!(output["spare"], json!([{"name": "Maya"}]));
    // members of a relation with no direction read each other
    assert_eq!(output["has"]["knows"], json!([{"name": "Sam"}]));
    assert_eq!(output["friend"], json!({"knows": [{"name": "Maya"}]}));
    // a setting holds what each walk it gathers gives, by name
    assert_eq!(
        output["trip"],
        json!({"traveler": {"name": "Maya"}, "booking": {"code": "AB42"}})
    );
}

#[test]
fn a_world_reads_what_the_agent_was_given_and_what_its_steps_return() {
    let source = r#"(receipt:entity)
    -[:next]->(:given {value: {charged: charge.output.amount, to: desk.input.name}});
(note:sentence)
    -[:next]->(:part {subject: "the charge", verb: "be", complement: charge.output.status});
(desk:agent) {input: {name: "Maya"}, output: done.output};
(desk)-[:next]->(t:task {name: "t"}) {input: desk.input, output: done.output};
(t)-[:calls]->(charge:tool {name: "charge"}) {input: t.input, output: {amount: range(10, 20), status: choice("settled", "pending")}}
    -[:next]->(done:task {name: "done"}) {input: charge.output, output: {receipt: receipt.attributes, note: note.text, amount: charge.output.amount, status: charge.output.status}};
"#;
    for output in outputs(source, 0..12) {
        // the world quotes the one charge the step made, whatever it drew
        assert_eq!(output["receipt"], json!({"charged": output["amount"], "to": "Maya"}));
        assert_eq!(
            output["note"],
            format!("The charge is {}.", output["status"].as_str().unwrap())
        );
    }
    // a step's own fields are read by name whether or not the walk visits the step
    let world = r#"(w:list)
    =[:weighted]=>[
        -[:next {weight: 1}]->(p:policy {subject: "a", verb: "be"}),
        -[:next {weight: 1}]->(q:policy {subject: "b", verb: "be"}),
    ];"#;
    for output in outputs(&shape(world, "{p: p.text, q: q.text, built: w.items}"), 0..8) {
        assert_eq!(output["p"], "A is.");
        assert_eq!(output["q"], "B is.");
        assert!(
            output["built"] == json!(["A is."]) || output["built"] == json!(["B is."]),
            "{output}"
        );
    }
}

#[test]
fn a_repeat_fork_walks_its_branches_once_for_each_round() {
    let world = r#"(reasons:list)
    -[:next]->(:policy {subject: "the desk", verb: "open"})
    =[:repeat {count: 3}]=>[
        -[:next]->(reason:either),
    ];
(reason)
    =[:weighted]=>[
        -[:next {weight: 1}]->(:policy {subject: "the flight", verb: "leave", complement: "late"}),
        -[:next {weight: 1}]->(:policy {subject: "the flight", verb: "close", complement: "early"}),
        -[:next {weight: 1}]->(:policy {subject: "the flight", verb: "change"}),
    ];"#;
    let said = outputs(&shape(world, "reasons.items"), 0..24);
    let mut mixed = false;
    for items in &said {
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0], "The desk opens.");
        mixed |= items[1] != items[2] || items[2] != items[3];
    }
    // each round draws for itself, so the three are not always the same
    assert!(mixed);
    assert_eq!(said, outputs(&shape(world, "reasons.items"), 0..24));
    // no rounds give nothing
    let none = world.replace("count: 3", "count: 0");
    assert_eq!(outputs(&shape(&none, "reasons.items"), 1..2), [json!(["The desk opens."])]);
}

#[test]
fn a_persona_says_its_sentences_its_own_way() {
    let world = r#"(maya:persona {style: "text_message"})
    -[:next]->(ask:sentence)
        -[:next]->(:part {subject: "I", person: "first", modal: "can", form: "question", verb: "change", complement: "my flight"});
(plain:sentence)
    -[:next]->(:part {subject: "I", person: "first", modal: "can", form: "question", verb: "change", complement: "my flight"});"#;
    let output = outputs(&shape(world, "{ask: ask.text, said: maya.said, plain: plain.text}"), 1..2).remove(0);
    assert_eq!(output["plain"], "Can I change my flight?");
    assert_eq!(output["ask"], "can i change my flight?");
    assert_eq!(output["said"], json!(["can i change my flight?"]));
    // the sentence as its grammar reads is still there
    let output = outputs(&shape(world, "[|ask.plain, ask.text|]"), 1..2).remove(0);
    assert_eq!(output, json!(["Can I change my flight?", "can i change my flight?"]));
}

#[test]
fn a_series_is_several_traces_that_belong_together() {
    // three traces in a row are one conversation, and the traveler keeps one mood through it
    let world = r#"(chat:series {turns: 3});
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
        "+(pair:node) {{number: {{value: floor(context.trace_index / 2)}}, turn: {{value: context.trace_index % 2}}}}\n    -[:extends]->(series);\n{}",
        source.replace("(chat:series {turns: 3})", "(chat:pair)")
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
    // weights say how likely each length is, and a length with no weight never opens a conversation
    let weighted = source.replace("{turns: 3}", "{turns: 1, up_to: 4, weights: [|8, 0, 2, 1|]}");
    let said = traces(&weighted, 5, 255);
    let opened = |length: u64| {
        said.iter()
            .filter(|trace| trace["turn"] == 0 && trace["length"] == length)
            .count()
    };
    assert_eq!(opened(2), 0);
    assert!(
        opened(1) > 2 * opened(3) && opened(3) > 0 && opened(4) > 0,
        "{} {} {}",
        opened(1),
        opened(3),
        opened(4)
    );
    // the bounds are whole numbers of turns, the second no smaller than the first
    let weights = "a series takes one weight for each length from turns to up_to, none negative and one positive";
    for (written, message) in [
        ("{turns: 2, up_to: 4, weights: [|1, 2|]}", weights),
        ("{turns: 2, up_to: 4, weights: [|0, 0, 0|]}", weights),
        ("{turns: 2, up_to: 4, weights: [|1, -1, 1|]}", weights),
        ("{turns: 2, weights: [|1|]}", weights),
        (
            "{turns: 4, up_to: 2}",
            "a series runs up to at least as many turns as it starts with",
        ),
        ("{turns: 0}", "a series has a whole number of turns from 1 to 256"),
        ("{turns: 2, up_to: 2.5}", "a series has a whole number of turns from 1 to 256"),
    ] {
        // lengths that are written are checked where the series is written, before any trace
        let found = rejected(&source.replace("{turns: 3}", written));
        assert!(found.contains(message), "{written}: {found}");
    }
    // lengths that are computed are checked by the trace that reads them
    let computed = source.replace("{turns: 3}", "{turns: 2, up_to: 4, weights: [|1, range(1, 2)|]}");
    let model = SELECTED.compile(&computed).unwrap();
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
    assert!(error.to_string().contains(weights), "{error}");
    // a series has no steps
    let found = rejected(&shape(
        "(chat:series)-[:next]->(:policy {subject: \"a\", verb: \"be\"});",
        "chat.id",
    ));
    assert!(found.contains("a series has no steps, so nothing leaves it"), "{found}");
}

#[test]
fn dividing_two_integers_gives_an_integer() {
    // the remainder is dropped toward zero, written or computed, and a float on either side gives a float
    let said = outputs(
        &shape(
            "",
            "{whole: 7 / 2, negative: -7 / 2, half: 7 / 2.0, drawn: range(8, 8) / 100, fraction: range(8, 8) / 100.0}",
        ),
        0..1,
    );
    assert_eq!(
        said[0],
        json!({"whole": 3, "negative": -3, "half": 3.5, "drawn": 0, "fraction": 0.08})
    );
}

#[test]
fn a_list_keeps_its_sentences_apart() {
    // a text message drops the full stop of each sentence, and the list puts one back between them
    let world = r#"(customer:persona {style: "text_message"})
    -[:next]->(:list)
        -[:next]->(:sentence)
            -[:next]->(:part {subject: "someone", verb: "hit", tense: "past", complement: "my car"})
        -[:next]->(:sentence) {defaults: {form: "question", modal: "can"}}
            -[:next]->(:part {subject: "you", person: "second", verb: "help"})
        -[:next]->(:sentence)
            -[:next]->(:part {subject: "I", person: "first", verb: "need", complement: "a claim"});"#;
    let said = outputs(&shape(world, "customer.said[0]"), 0..1);
    assert_eq!(said[0], "someone hit my car. can you help? i need a claim");
    // sentences that end as written are joined as they are, and nothing is added after the last
    let joined = outputs(
        &shape("", r#"paragraph([|"It rained.", null, "", "Did it stop?", "Not yet"|])"#),
        0..1,
    );
    assert_eq!(joined[0], "It rained. Did it stop? Not yet");
}

#[test]
fn a_tag_is_written_or_computed() {
    // a tag reads the world, a condition or a function like any other string
    let source = r#"(line:fact)
    -[:next]->(:given {value: "home"});
(a:agent) {input: "hi", output: t.output};
(a)-[:next]->(t:task {name: "t"}) {
    input: a.input,
    output: "ok",
    local: {vip: true},
    tags: [|"claims", line.value, self.local.vip ? "vip" : "standard", lower("WEB")|],
};
"#;
    let model = SELECTED.compile(source).unwrap_or_else(|errors| panic!("{errors}"));
    let batch = data::generate(
        model,
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        1,
        &(),
    )
    .unwrap();
    assert_eq!(&*batch.events[0].tags, ["claims", "home", "vip", "web"]);
    // a number is not a tag
    let found = rejected(&source.replace("line.value", "3"));
    assert!(found.contains("a tag is a string, written or computed"), "{found}");
}

#[test]
fn the_world_example_in_the_skill_generates() {
    // the skill's complete example is the first world an author copies, so it compiles and runs as printed
    let example = bts_core::providers::world::skill::DEFINITIONS
        .rsplit_once("```bts\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let model = SELECTED.compile(example).unwrap_or_else(|errors| panic!("{errors}"));
    let batch = data::generate(
        model,
        40,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        3,
        &(),
    )
    .unwrap();
    let roots: Vec<_> = batch.events.iter().filter(|event| event.span_parents.is_empty()).collect();
    assert_eq!(roots.len(), 40);
    let mut turns = std::collections::BTreeMap::<String, Vec<u64>>::new();
    let mut replies = std::collections::BTreeMap::<String, Vec<String>>::new();
    let thread_of = |root: &data::Event| {
        let thread = root.metadata.as_ref().unwrap()["thread_id"].project(&()).unwrap();
        thread.as_str().unwrap().to_owned()
    };
    for root in &roots {
        let metadata = root.metadata.as_ref().unwrap();
        let thread = thread_of(root);
        let turn = metadata["turn"].project(&()).unwrap().as_u64().unwrap();
        turns.entry(thread).or_default().push(turn);
        // a turn carries the messages before it
        let input = root.input.as_ref().unwrap().project(&()).unwrap();
        assert_eq!(input["earlier"].as_array().unwrap().len() as u64, turn);
        // the llm step is handed the rules, each earlier turn with its reply, and this turn's message
        let step = (batch.events.iter())
            .find(|event| event.root_span_id == root.root_span_id && event.name == "chat_completion")
            .unwrap();
        let messages = step.input.as_ref().unwrap().project(&()).unwrap();
        let messages = messages.as_array().unwrap();
        assert_eq!(messages.len() as u64, 2 * turn + 2);
        assert_eq!(messages.last().unwrap()["content"], input["message"]);
        // the reply answers what the customer asked, and each turn has its own
        let reply = root
            .output
            .as_ref()
            .unwrap()
            .project(&())
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let topic = metadata["topic"].project(&()).unwrap().as_str().unwrap().to_owned();
        if turn == 0 {
            let asked = input["message"].as_str().unwrap().to_lowercase();
            let (said, answered) = match topic.as_str() {
                "charged_twice" => ("twice", "second charge"),
                _ => ("after i canceled", "after you canceled"),
            };
            assert!(asked.contains(said) && reply.contains(answered), "{topic}: {asked} / {reply}");
        }
        replies.entry(thread_of(root)).or_default().push(reply);
    }
    for (thread, replies) in &replies {
        let distinct: std::collections::BTreeSet<_> = replies.iter().collect();
        assert_eq!(distinct.len(), replies.len(), "{thread}: {replies:?}");
    }
    assert!(replies.values().any(|replies| replies.len() > 2));
    // every thread counts its turns from zero without a gap
    assert!(turns.len() > 8, "{turns:?}");
    for (thread, turns) in &turns {
        assert_eq!(*turns, (0..turns.len() as u64).collect::<Vec<_>>(), "{thread}");
    }
}

#[test]
fn a_verb_without_forms_is_rejected_where_it_is_written() {
    // a sentence is put into words when a trace reads it, on the branch a draw takes
    // a verb written as a word is looked up when the shape compiles, so no branch hides it
    let unknown = "unknown English verb \"reconcile\", write its forms as {base: \"reconcile\"";
    for world in [
        "(w:list)\n    -[:next]->(:policy {subject: \"we\", person: \"first\", number: \"plural\", verb: \"reconcile\"});",
        "(w:sentence)\n    =[:weighted]=>[\n        -[:next {weight: 99}]->(:part {subject: \"I\", person: \"first\", verb: \"wait\"}),\n        -[:next {weight: 1}]->(:part {subject: \"I\", person: \"first\", verb: \"reconcile\"}),\n    ];",
        "(w:sentence) {defaults: {verb: \"reconcile\"}}\n    -[:next]->(:part {subject: \"I\", person: \"first\"});",
    ] {
        let source = shape(world, "w.text");
        let errors = SELECTED.compile_module(&source, false).unwrap_err();
        assert!(errors[0].what.starts_with(unknown), "{}\n{world}", errors[0].what);
        // the error is at the verb, in the author's own source
        assert_eq!(errors[0].r#where.source, 0);
        assert_eq!(&source[errors[0].r#where.start..errors[0].r#where.end], "\"reconcile\"");
    }
    // a verb written with its forms, and a verb the provider knows, compile
    for world in [
        "(w:sentence)\n    -[:next]->(:part {subject: \"I\", person: \"first\", verb: {base: \"reconcile\", third_person_singular: \"reconciles\", past: \"reconciled\", past_participle: \"reconciled\", present_participle: \"reconciling\"}});",
        "(w:sentence)\n    -[:next]->(:part {subject: \"I\", person: \"first\", verb: \"wait\"});",
    ] {
        SELECTED.compile_module(&shape(world, "w.text"), false).unwrap();
    }
    // a statement and a policy take the forms the error asks for, as a part does
    let forms = "{base: \"reconcile\", third_person_singular: \"reconciles\", past: \"reconciled\", past_participle: \"reconciled\", present_participle: \"reconciling\"}";
    for kind in ["statement", "policy"] {
        let world =
            format!("(w:list)\n    -[:next]->(:{kind} {{subject: \"the team\", verb: {forms}, complement: \"the report\"}});");
        let said = outputs(&shape(&world, "w.text"), 0..1);
        assert_eq!(said[0], "The team reconciles the report.");
    }
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
            "(w:list)-[:next]->(s:list)-[:next]->(w);".to_owned(),
            "w.text",
            "this walk is a step of a walk that is a step of it, and a walk cannot build itself",
        ),
        (
            format!(
                "(w:list)\n    =[:repeat {{count: other.value}}]=>[\n        -[:next]->{policy},\n    ];\n(other:fact)-[:next]->(:given {{value: 2}});"
            ),
            "w.text",
            "a repeat fork in a walk is written with a whole count from 0 to 64",
        ),
        (
            format!(
                "(w:entity)-[:next]->(:given {{value: {{a: 1}}}});\n(r:relation);\n(r)-[:incidence]-(w);\n(r)-[:incidence]-{policy};"
            ),
            "w.related",
            "a relation in a world joins walks, and this is not one",
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
        // one walk is built once, so it cannot be what a repeat repeats
        (
            "(w:list)\n    =[:repeat {count: 2}]=>[\n        -[:next]->(s:sentence),\n    ];\n(s)-[:next]->(:part {subject: \"a\", verb: \"be\"});".to_owned(),
            "w.text",
            "a walk inside a repeat is built once, so every round would give the same. Repeat an `either` with a fork under it",
        ),
        (
            "(w:entity)-[:next]->(:given {value: {a: 1}});\n(all:setting {nodes: [|(w), (p)|], edges: [||]});\n(p:vars) {x: 1};".to_owned(),
            "all.members",
            "a network in a world gathers named walks, and this is not one",
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
    assert_eq!(
        alone[0].what,
        "`w` is read here and is on no agent's path, put it on the path before the step that reads it or under a walk beside the agent"
    );
}
