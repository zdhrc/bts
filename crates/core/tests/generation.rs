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

#[test]
fn preserves_native_seeded_samples() {
    let source = r#"
        trace "sampling" {
            output = {
                integer = range(-10, 10)
                choice = choice("alpha", "beta", "gamma")
                weighted = weighted(["steady", 3], ["burst", 1])
                chance = chance(0.5)
                uuid = uuid()
                hex = hex(8)
                alphanum = alphanum(8)
            }
        }
    "#;
    // captured from the original 64-bit native generator
    let expected = [
        (
            0,
            serde_json::json!([
                {
                    "integer": 10,
                    "choice": "alpha",
                    "weighted": "burst",
                    "chance": true,
                    "uuid": "0db107f7-53c0-422c-b3fd-475fe0114ae9",
                    "hex": "9daf119a",
                    "alphanum": "MjIhecBT"
                },
                {
                    "integer": -5,
                    "choice": "alpha",
                    "weighted": "steady",
                    "chance": true,
                    "uuid": "1e007435-896d-4761-874d-0067e9312c69",
                    "hex": "6b559921",
                    "alphanum": "qhDh7XAq"
                }
            ]),
        ),
        (
            42,
            serde_json::json!([
                {
                    "integer": -6,
                    "choice": "gamma",
                    "weighted": "steady",
                    "chance": false,
                    "uuid": "fa0552f9-657f-45da-8cf7-06f5fed89d5b",
                    "hex": "2a9e71e2",
                    "alphanum": "qVjkXWW7"
                },
                {
                    "integer": -6,
                    "choice": "alpha",
                    "weighted": "burst",
                    "chance": false,
                    "uuid": "35ef89eb-f8d8-48b7-8586-fda39ba3b61f",
                    "hex": "4cb85c37",
                    "alphanum": "Rl10wJDE"
                }
            ]),
        ),
        (
            u64::MAX,
            serde_json::json!([
                {
                    "integer": -8,
                    "choice": "beta",
                    "weighted": "burst",
                    "chance": true,
                    "uuid": "b09f5e2d-909c-43a1-a275-f3e3d182ea20",
                    "hex": "e544e8b6",
                    "alphanum": "GEdnZET4"
                },
                {
                    "integer": -10,
                    "choice": "beta",
                    "weighted": "steady",
                    "chance": false,
                    "uuid": "55bef189-0e4b-485f-8cbb-e060614354c9",
                    "hex": "6b1e2c38",
                    "alphanum": "NMGGYH8B"
                }
            ]),
        ),
    ];
    for (seed, expected) in expected {
        let batch = data::generate(
            compile(source).unwrap(),
            2,
            Duration::from_secs(60),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(1_735_689_660),
            seed,
            &(),
        )
        .unwrap();
        let outputs: Vec<_> = batch
            .events
            .iter()
            .filter(|event| event.span_parents.is_empty())
            .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
            .collect();
        assert_eq!(serde_json::Value::Array(outputs), expected, "seed {seed}");
    }
}
