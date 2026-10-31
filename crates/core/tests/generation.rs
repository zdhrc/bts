use bts_core::mocking::data::{self, Distribution, Value};
use bts_lang::semantics::compile;
use std::collections::HashSet;
use std::time::{Duration, UNIX_EPOCH};

const SOURCE: &str = include_str!("fixtures/generation.bt");

#[test]
fn generates_correlated_session_events() {
    let model = compile(SOURCE).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(86_400);
    let batch = data::generate(model, 20, Duration::from_secs(3_600), Distribution::Linear, now, 42, &()).unwrap();

    assert_eq!(batch.trace_count, 20);
    let roots: Vec<_> = batch.events.iter().filter(|event| event.span_parents.is_empty()).collect();
    assert_eq!(roots.len(), 20);
    let mut routes = HashSet::new();
    let mut turn_counts = HashSet::new();

    for root in roots {
        let events: Vec<_> = batch
            .events
            .iter()
            .filter(|event| event.root_span_id == root.span_id)
            .collect();
        let turns: Vec<_> = events.iter().copied().filter(|event| event.name == "turn").collect();
        let drafts: Vec<_> = events.iter().copied().filter(|event| event.name == "draft").collect();
        let route: Vec<_> = events
            .iter()
            .copied()
            .filter(|event| matches!(event.name.as_str(), "lookup" | "fallback"))
            .collect();
        let handoff = events.iter().any(|event| event.name == "handoff");

        assert_eq!(root.name, "support-session");
        assert_eq!(root.input.as_ref().unwrap()["customer"], Value::from("acct-42"));
        assert_eq!(root.metadata.as_ref().unwrap()["product"], Value::from("billing"));
        assert_eq!(root.metadata.as_ref().unwrap()["escalated"], Value::from(handoff));
        assert_eq!(root.tags.as_ref(), ["support", "orders"]);
        assert!((2..=4).contains(&turns.len()));
        assert_eq!(drafts.len(), turns.len());
        assert_eq!(route.len(), 1);
        routes.insert(route[0].name.as_str());
        turn_counts.insert(turns.len());

        for (index, turn) in turns.iter().enumerate() {
            assert_eq!(turn.input.as_ref().unwrap()["index"], Value::from(index));
            assert_eq!(turn.input.as_ref().unwrap()["request"], Value::from("Where is my order?"));
            assert_eq!(turn.output.as_ref().unwrap(), &Value::from(format!("answer {index}")));
            assert_eq!(turn.span_parents.as_ref(), std::slice::from_ref(&root.span_id));
            let draft = drafts
                .iter()
                .find(|draft| draft.span_parents.as_ref() == std::slice::from_ref(&turn.span_id))
                .unwrap();
            assert_eq!(draft.input, turn.input);
            assert_eq!(&draft.output.as_ref().unwrap()["content"], turn.output.as_ref().unwrap());
            assert_eq!(draft.metrics["tokens"], Value::from(16));
        }
        assert_eq!(root.output, turns.last().unwrap().output);
    }

    assert_eq!(routes, HashSet::from(["lookup", "fallback"]));
    assert!(turn_counts.len() > 1);
}

#[test]
fn reproduces_the_same_events_with_a_fixed_seed() {
    let now = UNIX_EPOCH + Duration::from_secs(86_400);
    let generate = || {
        let mut batch = data::generate(
            compile(SOURCE).unwrap(),
            3,
            Duration::from_secs(600),
            Distribution::Sine,
            now,
            42,
            &(),
        )
        .unwrap();
        batch.assign_stable_ids("generation-test");
        batch
    };

    assert_eq!(generate(), generate());
}
