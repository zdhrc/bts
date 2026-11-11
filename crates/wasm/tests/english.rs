#![cfg(target_arch = "wasm32")]

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[path = "../../core/tests/english.rs"]
mod shared;

fn generate(source: &str, count: usize) -> serde_json::Value {
    let options = serde_json::json!({"count":count,"over_ms":600000,"until_ms":1000000,"seed":"42"});
    let options = serde::Serialize::serialize(&options, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap();
    serde_wasm_bindgen::from_value(bts_wasm::generate(source, options).unwrap()).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn public_browser_api_materializes_the_english_toolkit() {
    let result = generate(include_str!("../../core/tests/fixtures/english.bt"), 8);
    assert_eq!(result["ok"], true, "{result}");
    let events = result["value"]["payload"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 8);
    for event in events {
        let output = &event["output"];
        assert_eq!(output["action"], "confirm_check_in");
        assert_eq!(output["statement"], "The guest will arrive after midnight.");
        assert_eq!(
            output["combined"],
            "The guest will arrive after midnight, and the desk will be open."
        );
        assert_eq!(output["next_reference"], "she");
        assert_eq!(output["reset_reference"], "Maya");
        assert_eq!(output["amount"], "12,34,567.90");
        assert_eq!(output["date"], "10 November 2026");
        assert_eq!(output["time"], "12:05:09 AM");
        assert_eq!(output["structured"]["selection"]["message"]["layout"], "current_desired");
        assert_eq!(output["slot_answer"]["text"], "Friday");
        assert_eq!(output["candidate_count"], 18);
        assert_eq!(output["procedural"]["selection"]["intent"], "change_flight");
        assert!(["What about Friday?", "what about Friday?"].contains(&output["followup"]["text"].as_str().unwrap()));
        assert_eq!(output["plain_expression"], "What about Friday?");
        assert_eq!(output["followup"]["context"]["recent"].as_array().unwrap().len(), 2);
        assert_eq!(output["styled"], "I'll check the file.");
        assert_eq!(output["fragment"], "Checking the file.");
        assert_eq!(output["softened"], "Could you send the file?");
        assert_eq!(output["shorthand"], "Btw, the file is ready.");
        assert_eq!(output["typo"], "plese");
        assert_eq!(output["wording"], output["reused"]);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn invalid_english_arguments_have_browser_source_diagnostics() {
    let expression = "format_date(self.input)";
    let source = format!("trace \"t\" {{ input = \"2026-02-30\" output = {expression} }}");
    let result = generate(&source, 1);
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["stage"], "generate");
    let diagnostic = &result["error"]["diagnostics"][0];
    assert_eq!(diagnostic["phase"], "generation");
    assert!(diagnostic["message"].as_str().unwrap().contains("format_date"));
    let start = source.find(expression).unwrap();
    assert_eq!(
        diagnostic["range"],
        serde_json::json!({"start": start,"end":start+expression.len()})
    );
}
