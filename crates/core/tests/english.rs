use bts_core::english::*;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;

fn subject(text: &str, person: Person, number: Number) -> Subject {
    Subject {
        text: text.into(),
        person,
        number,
    }
}

fn predicate(base: &str, third_person: &str, complement: Option<&str>) -> Predicate {
    Predicate {
        verb: {
            let verb = Verb::lookup(base).unwrap();
            assert_eq!(verb.third_person_singular, third_person);
            verb
        },
        complement: complement.map(str::to_owned),
    }
}

fn guest() -> Sentence {
    Sentence {
        clause: Clause {
            subject: subject("the guest", Person::Third, Number::Singular),
            modal: Some(Modal::Can),
            tense: Tense::Present,
            aspect: Aspect::Simple,
            polarity: Polarity::Positive,
            predicate: predicate("check", "checks", Some("in")),
            form: Form::Statement,
        },
        condition: None,
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn grammatical_features_control_meaning_independently_of_wording() {
    let mut sentence = guest();
    let rendering = RealizeOptions::default();
    assert_eq!(realize(&sentence, &rendering).unwrap(), "The guest can check in.");
    sentence.clause.form = Form::Question;
    assert_eq!(realize(&sentence, &rendering).unwrap(), "Can the guest check in?");
    sentence.clause.form = Form::Statement;
    sentence.clause.polarity = Polarity::Negative;
    assert_eq!(realize(&sentence, &rendering).unwrap(), "The guest cannot check in.");
    let contracted = apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Contracted)).unwrap();
    assert_eq!(realize(&sentence, &contracted).unwrap(), "The guest can't check in.");
    assert_eq!(sentence.clause.polarity, Polarity::Negative);
    assert_eq!(sentence.clause.modal, Some(Modal::Can));
    assert_eq!(sentence.clause.form, Form::Statement);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn conditional_order_keeps_supplied_references_and_agreement() {
    let mut sentence = guest();
    sentence.clause.subject = subject("they", Person::Third, Number::Plural);
    sentence.clause.form = Form::Question;
    sentence.condition = Some(Condition {
        subject: subject("the guest", Person::Third, Number::Singular),
        predicate: predicate("arrive", "arrives", Some("late")),
        polarity: Polarity::Positive,
        tense: Tense::Present,
        aspect: Aspect::Simple,
    });
    let rendering = RealizeOptions::default();
    assert_eq!(
        realize(&sentence, &rendering).unwrap(),
        "If the guest arrives late, can they check in?"
    );
    let after = apply_variation(&sentence, &rendering, Variation::ConditionPosition(ConditionPosition::After)).unwrap();
    assert_eq!(
        realize(&sentence, &after).unwrap(),
        "Can they check in if the guest arrives late?"
    );
    let condition = sentence.condition.as_mut().unwrap();
    condition.subject = subject("they", Person::Third, Number::Plural);
    assert_eq!(
        realize(&sentence, &rendering).unwrap(),
        "If they arrive late, can they check in?"
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn conditional_negation_uses_base_forms_and_explicit_irregulars() {
    let mut sentence = guest();
    sentence.condition = Some(Condition {
        subject: subject("the guest", Person::Third, Number::Singular),
        predicate: predicate("have", "has", Some("identification")),
        polarity: Polarity::Positive,
        tense: Tense::Present,
        aspect: Aspect::Simple,
    });
    let rendering = RealizeOptions::default();
    assert_eq!(
        realize(&sentence, &rendering).unwrap(),
        "If the guest has identification, the guest can check in."
    );
    sentence.condition.as_mut().unwrap().polarity = Polarity::Negative;
    assert_eq!(
        realize(&sentence, &rendering).unwrap(),
        "If the guest does not have identification, the guest can check in."
    );
    let contracted = apply_variation(&sentence, &rendering, Variation::Negation(NegationStyle::Contracted)).unwrap();
    assert_eq!(
        realize(&sentence, &contracted).unwrap(),
        "If the guest doesn't have identification, the guest can check in."
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn variation_does_not_invent_alternatives_for_unsupported_constructions() {
    let mut sentence = guest();
    let rendering = RealizeOptions::default();
    let options = VariationOptions {
        negation: true,
        condition_order: true,
    };
    assert!(applicable_variations(&sentence, &rendering, &options).unwrap().is_empty());
    assert!(apply_variation(&sentence, &rendering, Variation::ConditionPosition(ConditionPosition::After)).is_err());
    sentence.clause.polarity = Polarity::Negative;
    sentence.clause.modal = Some(Modal::May);
    assert!(applicable_variations(&sentence, &rendering, &options).unwrap().is_empty());
    assert!(
        realize(
            &sentence,
            &RealizeOptions {
                negation: NegationStyle::Contracted,
                ..rendering
            }
        )
        .is_err()
    );
    sentence.clause.modal = Some(Modal::Can);
    sentence.clause.form = Form::Question;
    assert!(applicable_variations(&sentence, &rendering, &options).unwrap().is_empty());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn structured_identifiers_and_unicode_complements_keep_their_spelling() {
    let mut sentence = guest();
    sentence.clause.subject = subject("guest_id", Person::Third, Number::Singular);
    sentence.clause.predicate = predicate("update", "updates", Some("résumé_ID-42"));
    sentence.clause.polarity = Polarity::Negative;
    assert_eq!(
        realize(&sentence, &RealizeOptions::default()).unwrap(),
        "guest_id cannot update résumé_ID-42."
    );
    let source = "  They cannot update résumé_ID-42.\n";
    let Analysis::Supported(analysis) = analyze(source) else {
        panic!("supported modal sentence")
    };
    assert_eq!(analysis.source(), source);
    let (range, replacement, contracted) = analysis.contraction().unwrap();
    assert!(contracted);
    let mut result = source.to_owned();
    result.replace_range(range, replacement);
    assert_eq!(result, "  They can't update résumé_ID-42.\n");
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn invalid_input_does_not_poison_later_calls() {
    let valid = guest();
    let rendering = RealizeOptions::default();
    let mut invalid = valid.clone();
    invalid.clause.subject.text.clear();
    assert!(realize(&invalid, &rendering).is_err());
    assert!(matches!(analyze("I don't have to cancel."), Analysis::Supported(_)));
    assert_eq!(analyze("I have to not cancel."), Analysis::Unsupported);
    assert_eq!(realize(&valid, &rendering).unwrap(), "The guest can check in.");
    assert!(matches!(analyze("The guest cannot check in."), Analysis::Supported(_)));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn packaged_noun_rules_and_unicode_boundaries_work_together() {
    let plural = text::inflect_noun("child", Number::Plural).unwrap();
    let mut sentence = guest();
    sentence.clause.subject = subject(&format!("the {plural}"), Person::Third, Number::Plural);
    let result = realize(&sentence, &RealizeOptions::default()).unwrap();
    assert_eq!(result, "The children can check in.");
    assert_eq!(
        text::words(&result).collect::<Vec<_>>(),
        ["The", "children", "can", "check", "in"]
    );
    assert_eq!(text::inflect_noun("children", Number::Singular).unwrap(), "child");
    assert_eq!(text::graphemes("e\u{301}👩‍💻").collect::<Vec<_>>(), ["e\u{301}", "👩‍💻"]);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn seeded_variation_matches_across_native_and_browser() {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    let options = VariationOptions {
        negation: true,
        condition_order: true,
    };
    let mut sentence = guest();
    sentence.clause.polarity = Polarity::Negative;
    let source = "The guest cannot check in.";
    let sequence = (0..12)
        .map(|_| {
            let structured = vary_sentence(&sentence, &RealizeOptions::default(), &options, &mut rng).unwrap();
            let prose = vary(source, &options, &mut rng).unwrap();
            assert!([source, "The guest can't check in."].contains(&structured.as_str()));
            assert!([source, "The guest can't check in."].contains(&prose.as_str()));
            (structured != source, prose != source)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        [
            (true, false),
            (true, true),
            (true, true),
            (false, true),
            (false, true),
            (true, true),
            (true, false),
            (false, true),
            (false, false),
            (true, false),
            (false, true),
            (true, true),
        ]
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn english_tenses_and_aspects_render_statements_questions_and_negation() {
    let mut sentence = guest();
    sentence.clause.subject = subject("the editor", Person::Third, Number::Singular);
    sentence.clause.modal = None;
    sentence.clause.predicate = predicate("write", "writes", Some("letters"));
    let options = RealizeOptions::default();
    for (tense, aspect, positive, negative, question, contracted) in [
        (
            Tense::Present,
            Aspect::Simple,
            "The editor writes letters.",
            "The editor does not write letters.",
            "Does the editor write letters?",
            "The editor doesn't write letters.",
        ),
        (
            Tense::Past,
            Aspect::Simple,
            "The editor wrote letters.",
            "The editor did not write letters.",
            "Did the editor write letters?",
            "The editor didn't write letters.",
        ),
        (
            Tense::Future,
            Aspect::Simple,
            "The editor will write letters.",
            "The editor will not write letters.",
            "Will the editor write letters?",
            "The editor won't write letters.",
        ),
        (
            Tense::Present,
            Aspect::Progressive,
            "The editor is writing letters.",
            "The editor is not writing letters.",
            "Is the editor writing letters?",
            "The editor isn't writing letters.",
        ),
        (
            Tense::Past,
            Aspect::Progressive,
            "The editor was writing letters.",
            "The editor was not writing letters.",
            "Was the editor writing letters?",
            "The editor wasn't writing letters.",
        ),
        (
            Tense::Future,
            Aspect::Progressive,
            "The editor will be writing letters.",
            "The editor will not be writing letters.",
            "Will the editor be writing letters?",
            "The editor won't be writing letters.",
        ),
        (
            Tense::Present,
            Aspect::Perfect,
            "The editor has written letters.",
            "The editor has not written letters.",
            "Has the editor written letters?",
            "The editor hasn't written letters.",
        ),
        (
            Tense::Past,
            Aspect::Perfect,
            "The editor had written letters.",
            "The editor had not written letters.",
            "Had the editor written letters?",
            "The editor hadn't written letters.",
        ),
        (
            Tense::Future,
            Aspect::Perfect,
            "The editor will have written letters.",
            "The editor will not have written letters.",
            "Will the editor have written letters?",
            "The editor won't have written letters.",
        ),
        (
            Tense::Present,
            Aspect::PerfectProgressive,
            "The editor has been writing letters.",
            "The editor has not been writing letters.",
            "Has the editor been writing letters?",
            "The editor hasn't been writing letters.",
        ),
        (
            Tense::Past,
            Aspect::PerfectProgressive,
            "The editor had been writing letters.",
            "The editor had not been writing letters.",
            "Had the editor been writing letters?",
            "The editor hadn't been writing letters.",
        ),
        (
            Tense::Future,
            Aspect::PerfectProgressive,
            "The editor will have been writing letters.",
            "The editor will not have been writing letters.",
            "Will the editor have been writing letters?",
            "The editor won't have been writing letters.",
        ),
    ] {
        sentence.clause.tense = tense;
        sentence.clause.aspect = aspect;
        sentence.clause.form = Form::Statement;
        sentence.clause.polarity = Polarity::Positive;
        assert_eq!(realize(&sentence, &options).unwrap(), positive);
        sentence.clause.polarity = Polarity::Negative;
        assert_eq!(realize(&sentence, &options).unwrap(), negative);
        let changed = apply_variation(&sentence, &options, Variation::Negation(NegationStyle::Contracted)).unwrap();
        assert_eq!(realize(&sentence, &changed).unwrap(), contracted);
        sentence.clause.polarity = Polarity::Positive;
        sentence.clause.form = Form::Question;
        assert_eq!(realize(&sentence, &options).unwrap(), question);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn english_agreement_covers_person_number_and_irregular_be() {
    let mut sentence = guest();
    sentence.clause.modal = None;
    sentence.clause.predicate = predicate("be", "is", Some("ready"));
    for (text, person, number, present, past) in [
        ("I", Person::First, Number::Singular, "I am ready.", "I was ready."),
        ("you", Person::Second, Number::Singular, "You are ready.", "You were ready."),
        ("she", Person::Third, Number::Singular, "She is ready.", "She was ready."),
        ("we", Person::First, Number::Plural, "We are ready.", "We were ready."),
        ("you", Person::Second, Number::Plural, "You are ready.", "You were ready."),
        ("they", Person::Third, Number::Plural, "They are ready.", "They were ready."),
    ] {
        sentence.clause.subject = subject(text, person, number);
        sentence.clause.tense = Tense::Present;
        assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), present);
        sentence.clause.tense = Tense::Past;
        assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), past);
    }
    sentence.clause.subject = subject("they", Person::Third, Number::Plural);
    sentence.clause.tense = Tense::Present;
    sentence.clause.aspect = Aspect::Progressive;
    sentence.clause.predicate = predicate("run", "runs", None);
    assert_eq!(realize(&sentence, &RealizeOptions::default()).unwrap(), "They are running.");
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn modal_aspect_and_past_conditions_preserve_explicit_time() {
    let mut sentence = guest();
    sentence.clause.modal = Some(Modal::Could);
    sentence.clause.aspect = Aspect::Perfect;
    sentence.clause.predicate = predicate("leave", "leaves", Some("earlier"));
    sentence.condition = Some(Condition {
        subject: subject("they", Person::Third, Number::Plural),
        predicate: predicate("finish", "finishes", Some("the work")),
        polarity: Polarity::Positive,
        tense: Tense::Past,
        aspect: Aspect::Perfect,
    });
    let options = RealizeOptions::default();
    assert_eq!(
        realize(&sentence, &options).unwrap(),
        "If they had finished the work, the guest could have left earlier."
    );
    let after = apply_variation(&sentence, &options, Variation::ConditionPosition(ConditionPosition::After)).unwrap();
    assert_eq!(
        realize(&sentence, &after).unwrap(),
        "The guest could have left earlier if they had finished the work."
    );
    sentence.clause.tense = Tense::Past;
    assert!(realize(&sentence, &options).is_err());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn english_inflection_combines_packaged_rules_with_explicit_forms() {
    for (base, third, past, participle, progressive) in [
        ("write", "writes", "wrote", "written", "writing"),
        ("run", "runs", "ran", "run", "running"),
        ("go", "goes", "went", "gone", "going"),
        ("have", "has", "had", "had", "having"),
    ] {
        let verb = Verb::lookup(base).unwrap();
        assert_eq!(
            (
                &*verb.third_person_singular,
                &*verb.past,
                &*verb.past_participle,
                &*verb.present_participle
            ),
            (third, past, participle, progressive)
        );
    }
    let verb = Verb::regular("carry").unwrap();
    assert_eq!(
        (&*verb.third_person_singular, &*verb.past, &*verb.present_participle),
        ("carries", "carried", "carrying")
    );
    let verb = Verb::from_forms("travel", "travels", "travelled", "travelled", "travelling").unwrap();
    let mut sentence = guest();
    sentence.clause.modal = None;
    sentence.clause.tense = Tense::Past;
    sentence.clause.predicate = Predicate { verb, complement: None };
    assert_eq!(
        realize(&sentence, &RealizeOptions::default()).unwrap(),
        "The guest travelled."
    );
    assert!(Verb::from_forms("", "goes", "went", "gone", "going").is_err());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn seeded_conditional_order_matches_across_native_and_browser() {
    let mut sentence = guest();
    sentence.clause.modal = None;
    sentence.clause.tense = Tense::Future;
    sentence.clause.predicate = predicate("leave", "leaves", None);
    sentence.condition = Some(Condition {
        subject: subject("they", Person::Third, Number::Plural),
        predicate: predicate("finish", "finishes", Some("the work")),
        polarity: Polarity::Positive,
        tense: Tense::Present,
        aspect: Aspect::Perfect,
    });
    let before = "If they have finished the work, the guest will leave.";
    let after = "The guest will leave if they have finished the work.";
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    let variations = VariationOptions {
        negation: false,
        condition_order: true,
    };
    let sequence = (0..12)
        .map(|_| {
            let output = vary_sentence(&sentence, &RealizeOptions::default(), &variations, &mut rng).unwrap();
            assert!([before, after].contains(&output.as_str()));
            output == after
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        [true, false, true, true, true, true, false, true, false, true, true, true]
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn english_prose_variation_preserves_negation_scope_and_identifiers() {
    let options = VariationOptions {
        negation: true,
        condition_order: false,
    };
    for (expanded, contracted) in [
        ("The editor does not write letters.", "The editor doesn't write letters."),
        ("Maya did not leave.", "Maya didn't leave."),
        ("They are not running.", "They aren't running."),
        ("The editor has not written letters.", "The editor hasn't written letters."),
        ("They had not been waiting.", "They hadn't been waiting."),
        ("I do not have to cancel.", "I don't have to cancel."),
        ("api_v2 did not update résumé_ID-42.", "api_v2 didn't update résumé_ID-42."),
    ] {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut saw_change = false;
        for _ in 0..16 {
            let result = vary(expanded, &options, &mut rng).unwrap();
            assert!([expanded, contracted].contains(&result.as_str()));
            saw_change |= result == contracted;
        }
        assert!(saw_change, "{expanded}");
        let Analysis::Supported(analysis) = analyze(contracted) else {
            panic!("{contracted}")
        };
        let (range, replacement, _) = analysis.contraction().unwrap();
        let mut restored = contracted.to_owned();
        restored.replace_range(range, replacement);
        assert_eq!(restored, expanded);
    }
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    for unsupported in [
        "I have to not cancel.",
        "She's written a letter.",
        "I'd leave.",
        "Le client ne peut pas annuler.",
        "Der Gast kann nicht einchecken.",
    ] {
        assert_eq!(analyze(unsupported), Analysis::Unsupported);
        assert_eq!(vary(unsupported, &options, &mut rng).unwrap(), unsupported);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn toolkit_materializes_through_the_dsl_with_explicit_conversation_scope() {
    use bts_core::mocking::data::{self, Distribution};
    use std::time::{Duration, UNIX_EPOCH};

    let source = include_str!("fixtures/english.bt");
    let outputs = |seed| {
        let batch = data::generate(
            bts_lang::semantics::compile(source).unwrap(),
            16,
            Duration::from_secs(600),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(1_000),
            seed,
            &(),
        )
        .unwrap();
        batch
            .events
            .iter()
            .filter_map(|event| event.output.as_ref())
            .map(|value| value.project(&()).unwrap())
            .collect::<Vec<_>>()
    };
    let first = outputs(42);
    assert_eq!(first, outputs(42));
    assert_ne!(first, outputs(43));
    for output in first {
        assert_eq!(output["action"], "confirm_check_in");
        assert_eq!(output["statement"], "The guest will arrive after midnight.");
        assert_eq!(
            output["combined"],
            "The guest will arrive after midnight, and the desk will be open."
        );
        assert_eq!(output["first_reference"], "Maya");
        assert_eq!(output["next_reference"], "she");
        assert_eq!(output["reset_reference"], "Maya");
        assert_eq!(
            output["details"],
            serde_json::json!([{"id":"desk","text":"The desk is open."}])
        );
        assert_eq!(output["amount"], "12,34,567.90");
        assert_eq!(output["date"], "10 November 2026");
        assert_eq!(output["time"], "12:05:09 AM");
        assert_eq!(output["structured"]["selection"]["message"]["layout"], "current_desired");
        assert_eq!(output["slot_answer"]["text"], "Friday");
        assert!(output["slot_answer"]["context"].get("requested").is_none());
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
        assert!(["The guest cannot check in.", "The guest can't check in."].contains(&output["wording"].as_str().unwrap()));
        assert!(
            [
                "If the guest arrives late, can they check in?",
                "Can they check in if the guest arrives late?"
            ]
            .contains(&output["conditional"].as_str().unwrap())
        );
        assert!(
            [
                "I won't leave. They cannot stay.",
                "I will not leave. They cannot stay.",
                "I won't leave. They can't stay."
            ]
            .contains(&output["register"].as_str().unwrap())
        );
        assert!(
            ["api_v2 does not update résumé_ID-42.", "api_v2 doesn't update résumé_ID-42."]
                .contains(&output["identifier"].as_str().unwrap())
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod parity {
    use bts_core::mocking::{
        code::{Emitter, Item, languages::Languages},
        data::{self, Distribution},
    };
    use bts_lang::{ScorerKind, ScorerLang, semantics::compile};
    use serde_json::{Value, json};
    use std::{collections::BTreeSet, fs, io::Write, process::Stdio, time::Duration};

    struct Case {
        name: String,
        operation: &'static str,
        args: Value,
        valid: bool,
        expected: Option<Value>,
        variable: bool,
    }

    impl Case {
        fn valid(name: impl Into<String>, operation: &'static str, args: Value) -> Self {
            Self {
                name: name.into(),
                operation,
                args,
                valid: true,
                expected: None,
                variable: false,
            }
        }

        fn invalid(name: impl Into<String>, operation: &'static str, args: Value) -> Self {
            Self {
                valid: false,
                ..Self::valid(name, operation, args)
            }
        }

        fn golden(mut self, expected: Value) -> Self {
            self.expected = Some(expected);
            self
        }

        fn variable(mut self) -> Self {
            self.variable = true;
            self
        }
    }

    fn sentence(subject: &str, verb: &str, complement: &str) -> Value {
        json!({"clause": {"subject": {"text": subject}, "predicate": {"verb": verb, "complement": complement}}})
    }

    fn corpus() -> Vec<Case> {
        let guest = sentence("the guest", "check", "in");
        let desk = sentence("the desk", "be", "open");
        let mut cases = vec![
        Case::valid("default sentence", "realize", json!([guest])).golden(json!("The guest checks in.")),
        Case::valid("authored Unicode and identifiers", "realize", json!([sentence("api_v2", "update", "résumé_ID-42")]))
            .golden(json!("api_v2 updates résumé_ID-42.")),
        Case::valid("authored lowercase name", "realize", json!([sentence("iPhone", "be", "ready")]))
            .golden(json!("iPhone is ready.")),
        Case::valid("authored verb forms", "realize", json!([{"clause":{"subject":{"text":"the service"},"predicate":{"verb":{"base":"frobnicate","third_person_singular":"frobnicates","past":"frobnicated","past_participle":"frobnicated","present_participle":"frobnicating"}}}}]))
            .golden(json!("The service frobnicates.")),
        Case::valid("explicit optional nulls", "realize", json!([{"clause":{"subject":{"text":"the guest"},"predicate":{"verb":"arrive","complement":null},"modal":null},"condition":null}]))
            .golden(json!("The guest arrives.")),
    ];
        for text in ["constructor", "__proto__", "toString", "Élodie", "\u{feff}Maya"] {
            cases.push(
                Case::valid(
                    format!("authored subject {text}"),
                    "realize",
                    json!([sentence(text, "be", "ready")]),
                )
                .golden(json!(format!("{text} is ready."))),
            );
        }
        for tense in ["present", "past", "future"] {
            for aspect in ["simple", "progressive", "perfect", "perfect_progressive"] {
                for polarity in ["positive", "negative"] {
                    for form in ["statement", "question"] {
                        for negation in ["expanded", "contracted"] {
                            let mut value = sentence("the guest", "write", "a note");
                            for (key, text) in [("tense", tense), ("aspect", aspect), ("polarity", polarity), ("form", form)] {
                                value["clause"][key] = json!(text);
                            }
                            cases.push(Case::valid(
                                format!("grammar {tense} {aspect} {polarity} {form} {negation}"),
                                "realize",
                                json!([value,{"negation":negation}]),
                            ));
                        }
                    }
                }
            }
        }
        for modal in ["can", "could", "may", "might", "must", "should", "will", "would"] {
            let mut value = guest.clone();
            value["clause"]["modal"] = json!(modal);
            value["clause"]["polarity"] = json!("negative");
            cases.push(Case::valid(format!("expanded modal {modal}"), "realize", json!([value])));
            cases.push(if matches!(modal, "may" | "might") {
                Case::invalid(
                    format!("unsupported contracted modal {modal}"),
                    "realize",
                    json!([value,{"negation":"contracted"}]),
                )
            } else {
                Case::valid(
                    format!("contracted modal {modal}"),
                    "realize",
                    json!([value,{"negation":"contracted"}]),
                )
            });
        }
        for (text, person, number) in [
            ("I", "first", "singular"),
            ("we", "first", "plural"),
            ("you", "second", "singular"),
            ("they", "third", "plural"),
            ("she", "third", "singular"),
        ] {
            let mut value = sentence(text, "be", "ready");
            value["clause"]["subject"] = json!({"text":text,"person":person,"number":number});
            cases.push(Case::valid(format!("agreement {text}"), "realize", json!([value])));
        }
        let mut conditional = guest.clone();
        conditional["clause"]["modal"] = json!("can");
        conditional["clause"]["form"] = json!("question");
        conditional["condition"] = json!({"subject":{"text":"the desk"},"predicate":{"verb":"be","complement":"open"}});
        cases.push(
            Case::valid("condition before", "realize", json!([conditional]))
                .golden(json!("If the desk is open, can the guest check in?")),
        );
        cases.push(
            Case::valid(
                "condition after",
                "realize",
                json!([conditional,{"condition_position":"after"}]),
            )
            .golden(json!("Can the guest check in if the desk is open?")),
        );
        for (name, pointer, value) in [
            ("unknown sentence field", "/extra", json!(true)),
            ("unknown clause field", "/clause/extra", json!(true)),
            ("unknown subject field", "/clause/subject/extra", json!(true)),
            ("unknown predicate field", "/clause/predicate/extra", json!(true)),
            ("unknown tense", "/clause/tense", json!("tomorrow")),
            ("null tense", "/clause/tense", Value::Null),
            ("unknown aspect", "/clause/aspect", json!("passive")),
            ("unknown polarity", "/clause/polarity", json!("neutral")),
            ("unknown form", "/clause/form", json!("imperative")),
            ("unknown person", "/clause/subject/person", json!(4)),
            ("unknown number", "/clause/subject/number", json!("dual")),
            ("unknown verb", "/clause/predicate/verb", json!("frobnicate")),
            ("empty subject", "/clause/subject/text", json!("")),
            ("trimmed subject", "/clause/subject/text", json!(" the guest")),
            (
                "subject clause punctuation",
                "/clause/subject/text",
                json!("the guest, who arrived"),
            ),
            ("subject control", "/clause/subject/text", json!("the\nguest")),
            (
                "Unicode sentence separator",
                "/clause/predicate/complement",
                json!("in\u{2028}later"),
            ),
            (
                "complement sentence punctuation",
                "/clause/predicate/complement",
                json!("in. Then leave"),
            ),
            ("pronoun agreement mismatch", "/clause/subject/text", json!("I")),
        ] {
            let mut malformed = guest.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            let object = if parent.is_empty() {
                &mut malformed
            } else {
                malformed.pointer_mut(parent).unwrap()
            };
            object[key] = value;
            cases.push(Case::invalid(name, "realize", json!([malformed])));
        }
        let mut bad_modal = guest.clone();
        bad_modal["clause"]["tense"] = json!("past");
        bad_modal["clause"]["modal"] = json!("can");
        cases.push(Case::invalid("modal with past tense", "realize", json!([bad_modal])));
        for options in [
            json!({"unknown":true}),
            json!({"negation":false}),
            json!({"condition_position":"middle"}),
            Value::Null,
        ] {
            cases.push(Case::invalid(
                format!("render options {options}"),
                "realize",
                json!([guest, options]),
            ));
        }

        for text in [
            "The guest cannot check in.",
            "If the guest arrives late, can they check in?",
            "I won't leave. They cannot stay.",
            "api_v2 does not update résumé_ID-42.",
        ] {
            cases.push(Case::valid(format!("prose variation {text}"), "vary", json!([text])).variable());
        }
        for text in [
            "She's written a letter.",
            "I'd leave.",
            "I have to not cancel.",
            "Le client ne peut pas annuler.",
            "The guest can\tcheck in.",
            "",
            "Élodie cannot update résumé_ID-42.",
        ] {
            cases.push(
                Case::valid(
                    format!("conservative prose {text}"),
                    "vary",
                    json!([text,{"negation":false,"condition_order":false}]),
                )
                .golden(json!(text)),
            );
        }
        cases.push(Case::valid("structured variation", "vary", json!([conditional])).variable());
        cases.push(
            Case::valid(
                "disabled variation",
                "vary",
                json!(["The guest cannot check in.",{"negation":false}]),
            )
            .golden(json!("The guest cannot check in.")),
        );
        for options in [
            json!({"negation":"yes"}),
            json!({"negation":null}),
            json!({"condition_order":0}),
            json!({"extra":true}),
            json!({"rendering":{"extra":true}}),
        ] {
            cases.push(Case::invalid(
                format!("variation options {options}"),
                "vary",
                json!(["The guest cannot check in.", options]),
            ));
        }

        for relation in ["and", "but", "or", "because"] {
            cases.push(Case::valid(
                format!("combine {relation}"),
                "combine",
                json!([[guest, desk], relation]),
            ));
        }
        let mut negative = guest.clone();
        negative["clause"]["polarity"] = json!("negative");
        cases.push(
            Case::valid(
                "because preserves negative effect",
                "combine",
                json!([[negative, desk], "because"]),
            )
            .golden(json!("Because the desk is open, the guest does not check in.")),
        );
        cases.push(Case::valid(
            "three propositions",
            "combine",
            json!([[guest, desk, sentence("the room", "be", "ready")], "and"]),
        ));
        for (name, values, relation) in [
            ("empty combination", json!([]), "and"),
            ("single proposition", json!([guest]), "and"),
            ("three causes", json!([guest, desk, guest]), "because"),
            ("unknown relation", json!([guest, desk]), "so"),
            ("conditional scope", json!([conditional, desk]), "and"),
        ] {
            cases.push(Case::invalid(name, "combine", json!([values, relation])));
        }

        let maya = json!({"id":"maya","label":"Maya","pronouns":{"subject":"she","object":"her","possessive_determiner":"her","possessive":"hers"}});
        let introduced = json!({"entities":[maya],"mentioned":["maya"]});
        cases.push(Case::valid("first mention", "refer", json!([maya, {}])).golden(json!("Maya")));
        for (form, expected) in [
            ("subject", "she"),
            ("object", "her"),
            ("possessive_determiner", "her"),
            ("possessive", "hers"),
        ] {
            cases.push(
                Case::valid(format!("reference {form}"), "refer", json!([maya,introduced,{"form":form}]))
                    .golden(json!(expected)),
            );
        }
        cases.push(
            Case::valid("name despite context", "refer", json!([maya,introduced,{"style":"name"}])).golden(json!("Maya")),
        );
        cases.push(
            Case::valid(
                "possessive proper name",
                "refer",
                json!([{"id":"j","label":"James"},{},{"form":"possessive"}]),
            )
            .golden(json!("James'")),
        );
        let mut other = maya.clone();
        other["id"] = json!("other");
        other["label"] = json!("Élodie");
        let ambiguous = json!({"entities":[maya,other],"mentioned":["maya","other"]});
        cases.push(Case::valid("ambiguous pronoun uses name", "refer", json!([maya, ambiguous])).golden(json!("Maya")));
        cases.push(Case::invalid(
            "forced ambiguous pronoun",
            "refer",
            json!([maya,ambiguous,{"style":"pronoun"}]),
        ));
        cases.push(Case::invalid(
            "forced first pronoun",
            "refer",
            json!([maya,{},{"style":"pronoun"}]),
        ));
        cases.push(Case::valid("remember deduplicates", "remember", json!([{},[maya,maya],["arrival","arrival"]]))
        .golden(json!({"entities":[{"id":"maya","label":"Maya","person":"third","number":"singular","pronouns":maya["pronouns"]}],"mentioned":["maya"],"facts":["arrival"]})));
        cases.push(
            Case::valid("empty context", "remember", json!([{}, []])).golden(json!({"entities":[],"mentioned":[],"facts":[]})),
        );
        cases.push(Case::valid("filter established details", "unmentioned", json!([[{"id":"arrival","text":"Arrival is confirmed."},{"id":"desk","text":"The desk is open."},{"id":"desk","text":"The desk is open."}],{"facts":["arrival"]}]))
        .golden(json!([{"id":"desk","text":"The desk is open."}])));
        for context in [
            json!({"extra":true}),
            json!({"entities":[maya,maya]}),
            json!({"mentioned":["missing"]}),
            json!({"facts":["arrival","arrival"]}),
            json!({"facts":null}),
            json!({"facts":[" untrimmed"]}),
        ] {
            cases.push(Case::invalid(
                format!("invalid context {context}"),
                "refer",
                json!([maya, context]),
            ));
        }
        for entity in [
            json!({"id":"empty"}),
            json!({"id":"e","label":""}),
            json!({"id":"e","label":"Maya","extra":true}),
            json!({"id":"e","label":"Maya","pronouns":{"subject":"she"}}),
        ] {
            cases.push(Case::invalid(
                format!("invalid entity {entity}"),
                "remember",
                json!([{}, [entity]]),
            ));
        }
        cases.push(Case::invalid(
            "conflicting entity",
            "remember",
            json!([introduced,[{"id":"maya","label":"Someone else"}]]),
        ));
        cases.push(Case::invalid("fact type", "remember", json!([{}, [], [true]])));
        cases.push(Case::invalid(
            "conflicting known detail",
            "unmentioned",
            json!([[{"id":"arrival","text":"yes"},{"id":"arrival","text":"no"}],{"facts":["arrival"]}]),
        ));
        cases.push(Case::invalid(
            "unknown detail field",
            "unmentioned",
            json!([[{"id":"x","text":"yes","extra":true}],{}]),
        ));

        for (value, options, expected) in [
            (json!(1234567.5), json!({}), "1,234,567.5"),
            (
                json!("1234567.895"),
                json!({"locale":"en-IN","fraction_digits":2}),
                "12,34,567.90",
            ),
            (json!("-0.005"), json!({"fraction_digits":2}), "-0.01"),
            (json!("-0.004"), json!({"fraction_digits":2}), "0.00"),
            (json!("999.995"), json!({"fraction_digits":2}), "1,000.00"),
            (json!("1.2300e2"), json!({}), "123"),
            (json!("1e-7"), json!({}), "0.0000001"),
            (json!(1.0), json!({"fraction_digits":1.0}), "1.0"),
            (
                json!("12345678901234567890.123456789"),
                json!({"grouping":false}),
                "12345678901234567890.123456789",
            ),
        ] {
            cases.push(
                Case::valid(format!("number {value} {options}"), "format_number", json!([value, options]))
                    .golden(json!(expected)),
            );
        }
        for locale in ["en-US", "en-GB", "en-CA", "en-AU", "en-IN"] {
            cases.push(Case::valid(
                format!("number locale {locale}"),
                "format_number",
                json!(["1234567.50",{"locale":locale}]),
            ));
            for style in ["short", "long"] {
                cases.push(Case::valid(
                    format!("date {locale} {style}"),
                    "format_date",
                    json!(["2026-11-10",{"locale":locale,"style":style}]),
                ));
            }
            for time in ["00:00", "12:00", "23:59:59"] {
                cases.push(Case::valid(
                    format!("time {locale} {time}"),
                    "format_time",
                    json!([time,{"locale":locale,"seconds":true}]),
                ));
            }
        }
        for (date, expected) in [
            ("0001-01-01", "January 1, 1"),
            ("2000-02-29", "February 29, 2000"),
            ("9999-12-31", "December 31, 9999"),
        ] {
            cases.push(Case::valid(format!("date bounds {date}"), "format_date", json!([date])).golden(json!(expected)));
        }
        for date in [
            "0000-01-01",
            "1900-02-29",
            "2026-02-29",
            "2026-13-01",
            "2026-01-00",
            "10000-01-01",
            "2026-1-01",
            "２０２６-11-10",
            "2026-11-10\n",
        ] {
            cases.push(Case::invalid(format!("invalid date {date}"), "format_date", json!([date])));
        }
        for time in ["24:00", "23:60", "23:59:60", "1:00", "00:0", "00:00Z", "００:00", "00:00\n"] {
            cases.push(Case::invalid(format!("invalid time {time}"), "format_time", json!([time])));
        }
        for value in [
            json!(true),
            Value::Null,
            json!("NaN"),
            json!("Infinity"),
            json!("1,000"),
            json!(".5"),
            json!("1."),
            json!("1e4097"),
            json!("0e4096"),
            json!("1e-4096"),
            json!(" 1"),
            json!("１２"),
        ] {
            cases.push(Case::invalid(
                format!("invalid number {value}"),
                "format_number",
                json!([value]),
            ));
        }
        for options in [
            json!({"locale":"fr-FR"}),
            json!({"grouping":1}),
            json!({"fraction_digits":10}),
            json!({"fraction_digits":-1}),
            json!({"fraction_digits":true}),
            json!({"fraction_digits":1.5}),
            json!({"extra":true}),
        ] {
            cases.push(Case::invalid(
                format!("number options {options}"),
                "format_number",
                json!([1, options]),
            ));
        }
        for (operation, value, options) in [
            ("format_date", json!("2026-11-10"), json!({"style":"medium"})),
            ("format_date", json!("2026-11-10"), json!({"locale":null})),
            ("format_time", json!("12:00"), json!({"seconds":1})),
            ("format_time", json!("12:00"), json!({"hour_cycle":"h23"})),
        ] {
            cases.push(Case::invalid(
                format!("{operation} options {options}"),
                operation,
                json!([value, options]),
            ));
        }
        for (text, options, expected) in [
            ("I will check the file.", json!({"contractions":true}), "I'll check the file."),
            (
                "We have received the file.",
                json!({"style":"conversational"}),
                "We've received the file.",
            ),
            ("By the way, it is ready.", json!({"shorthand":true}), "Btw, it is ready."),
            ("Thank you for the update.", json!({"casual":true}), "Thanks for the update."),
            ("See you later.", json!({"slang":true}), "Catch you later."),
            (
                "I am going to check the file.",
                json!({"reductions":true}),
                "I am gonna check the file.",
            ),
            (
                "I want to check the file.",
                json!({"reductions":true}),
                "I wanna check the file.",
            ),
            ("I am checking the file.", json!({"fragments":true}), "Checking the file."),
            ("Thanks.", json!({"warmth":true}), "Thanks so much."),
            (
                "Please share the file.",
                json!({"soften_requests":true}),
                "Could you please share the file?",
            ),
            ("Thanks.", json!({"punctuation":"omit_period"}), "Thanks"),
            ("Thanks.", json!({"punctuation":"exclamation"}), "Thanks!"),
            ("Thanks!", json!({"punctuation":"expressive"}), "Thanks!!"),
            (
                "I'll check with Maya.",
                json!({"casing":"lowercase"}),
                "i'll check with Maya.",
            ),
            ("please", json!({"typos":{"probability":1,"kinds":["common"]}}), "plese"),
            ("please", json!({"typos":{"probability":1,"kinds":["omission"]}}), "plase"),
            (
                "please",
                json!({"typos":{"probability":1,"kinds":["transposition"]}}),
                "pelase",
            ),
            ("please", json!({"typos":{"probability":1,"kinds":["repetition"]}}), "pllease"),
            ("please\u{301}", json!({"typos":{"probability":1}}), "please\u{301}"),
            ("Thank you.", json!({"style":"text_message","max_changes":0}), "Thank you."),
            (
                "Maya Élodie 李雷 👩‍💻 café e\u{301}",
                json!({"style":"text_message","typos":{"probability":1}}),
                "Maya Élodie 李雷 👩‍💻 café e\u{301}",
            ),
            (
                "I will thank you tomorrow.",
                json!({"casual":true}),
                "I will thank you tomorrow.",
            ),
            ("I am going to London.", json!({"reductions":true}), "I am going to London."),
            ("I have a file.", json!({"contractions":true}), "I have a file."),
            ("Thanks.", json!({"style":"enthusiastic","warmth":false}), "Thanks!"),
            ("please", json!({"protected":["please"],"typos":{"probability":1}}), "please"),
            ("Thank you.", json!({"casual":true,"probability":0}), "Thank you."),
            (
                "Thank you.",
                json!({"style":"neutral","alternatives":[{"from":"Thank you","to":["Much appreciated"]}]}),
                "Much appreciated.",
            ),
            (
                "I will check.",
                json!({"style":"neutral","contractions":null}),
                "I will check.",
            ),
            ("Thanks.", json!({"punctuation":"omit_period","max_changes":1.0}), "Thanks"),
        ] {
            cases.push(Case::valid(format!("styled {text} {options}"), "vary", json!([text, options])).golden(json!(expected)));
        }
        for text in ["You'm", "He'st", "please\u{301}", "Company Inc.", "."] {
            cases.push(
                Case::valid(
                    format!("style boundary {text}"),
                    "vary",
                    json!([text,{"casing":"lowercase","punctuation":"omit_period","typos":{"probability":1}}]),
                )
                .golden(json!(text)),
            );
        }
        let expression_source = json!({"intent":"test","constructions":[{"id":"change","predicate":{"verb":"change","complement":"my flight"}}],"families":[{"id":"need","kind":"need"}]});
        cases.push(
            Case::valid("procedural text", "express", json!([expression_source])).golden(json!("I need to change my flight.")),
        );
        cases.push(Case::valid(
            "procedural explanation",
            "expression",
            json!([expression_source]),
        ));
        cases.push(Case::valid(
            "procedural candidates",
            "expression_candidates",
            json!([expression_source]),
        ));
        for (name, path, value) in [
            ("empty intent", "/intent", json!("")),
            ("empty families", "/families", json!([])),
            ("unknown family", "/families/0/kind", json!("maybe")),
            ("negative weight", "/families/0/weight", json!(-1)),
            ("huge weight", "/families/0/weight", json!(1000001)),
            ("boolean weight", "/families/0/weight", json!(true)),
            ("unknown pattern", "/families/0/patterns", json!(["request_please"])),
            ("unknown construction", "/families/0/constructions", json!(["missing"])),
            ("incomplete followup", "/families/0/kind", json!("follow_up")),
            ("incomplete reason", "/families/0/kind", json!("reason_request")),
            (
                "duplicate families",
                "/families",
                json!([{"id":"x","kind":"need"},{"id":"x","kind":"need"}]),
            ),
            ("unknown detail", "/optional_details", json!(["missing"])),
            ("unknown field", "/surprise", json!(true)),
            ("sequence fact", "/facts", json!([["day", "Friday"]])),
            (
                "sequence verb",
                "/constructions/0/predicate/verb",
                json!(["change", "changes", "changed", "changed", "changing"]),
            ),
        ] {
            let mut source = expression_source.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            source
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), value);
            cases.push(Case::invalid(format!("expression {name}"), "express", json!([source])));
        }
        for options in [
            json!({"speaker":[]}),
            json!({"context":[[],[]]}),
            json!({"speaker":{"greetings":[["Hi!",1]]}}),
            json!({"speaker":{"wording":{"rendering":["expanded","before"]}}}),
            json!({"repeat_penalty":-1}),
            json!({"repeat_penalty":2}),
            json!({"context":{"established":["x","x"]}}),
            json!({"context":{"recent":[{}]}}),
            json!({"speaker":{"max_details":1.5}}),
            json!({"speaker":{"family_weights":{"need":true}}}),
            json!({"speaker":{"greeting_probability":2}}),
            json!({"speaker":{"greetings":[{"text":""}]}}),
            json!({"speaker":{"pattern_weights":[]}}),
            json!({"speaker":{"wording":{"style":"missing"}}}),
        ] {
            cases.push(Case::invalid(
                format!("expression options {options}"),
                "expression",
                json!([expression_source, options]),
            ));
        }
        let literal_message = json!({"intent":"literal","families":[{"id":"literal","kind":"authored","text":"Please keep {reason} unchanged.","message":{"layout":"request_reason","bindings":{"reason":"reason"}}}],"components":[{"id":"reason","role":"reason","forms":[{"id":"literal","text":"Élodie wrote {request} in the file."}]}]});
        cases.push(
            Case::valid("literal component braces", "express", json!([literal_message]))
                .golden(json!("Please keep {reason} unchanged. Élodie wrote {request} in the file.")),
        );
        let mut escaped = super::message_sources().remove(0);
        escaped.families.retain(|f| f.id == "current_desired");
        let plan = escaped.families[0].message.as_mut().unwrap();
        for (role, id, form) in [("current", "a/b", "c"), ("outcome", "a", "b/c")] {
            let c = escaped.components.iter_mut().find(|c| c.role == role).unwrap();
            c.id = id.into();
            c.forms.truncate(1);
            c.forms[0].id = form.into();
            plan.bindings.insert(role.into(), id.into());
        }
        let escaped_options = bts_core::english::ExpressionOptions::default();
        let escaped_result = bts_core::english::expression(
            &escaped,
            &escaped_options,
            &mut <rand_xoshiro::Xoshiro256PlusPlus as rand::SeedableRng>::seed_from_u64(1),
        )
        .unwrap();
        assert_eq!(escaped_result.selection.message.as_ref().unwrap().forms, ["a~1b/c", "a/b~1c"]);
        cases.push(Case::valid(
            "escaped form identifiers",
            "expression",
            json!([escaped, {"context":escaped_result.context}]),
        ));
        cases.push(Case::valid("nullable message history", "expression", json!([expression_source, {"context":{"recent":[{"intent":"other","family":"other","construction":null,"pattern":"authored","message":null}]}}])));
        let messages = serde_json::to_value(super::message_sources().remove(0)).unwrap();
        for (name, path, value) in [
            ("bad role", "/components/0/role", json!("invented")),
            ("overlapping correction facts", "/components/7/mentions", json!(["previous"])),
            ("bad form", "/components/0/forms/0/shape", json!("question")),
            ("bad layout", "/families/0/message/layout", json!("toString")),
            ("bad binding", "/families/1/message/bindings", json!({"reason":"outcome"})),
            ("invented relationship", "/families/0/message/relation", json!("cause")),
            ("unexpected apology", "/families/0/message/allow_apology", json!(true)),
            ("sequence component", "/components", json!([["x", "reason", []]])),
            ("sequence form", "/components/0/forms", json!([["x", "text"]])),
            ("sequence message", "/families/0/message", json!(["request_only"])),
            ("unguarded reference", "/components/0/forms/0/reference", json!("contextual")),
            ("unknown message field", "/families/0/message/unknown", json!(true)),
        ] {
            let mut source = messages.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            source
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), value);
            cases.push(Case::invalid(name, "expression_candidates", json!([source])));
        }
        for options in [
            json!({"speaker":{"organization":{"unknown":1}}}),
            json!({"speaker":{"sentence_shape":{"complete":true}}}),
            json!({"context":{"allowed_acts":["invent"]}}),
        ] {
            cases.push(Case::invalid(
                "invalid message options",
                "expression_candidates",
                json!([messages, options]),
            ));
        }
        let mut unavailable = expression_source.clone();
        unavailable["families"][0]["requires"] = json!(["missing"]);
        cases.push(Case::valid("no expression candidates", "expression_candidates", json!([unavailable])).golden(json!([])));
        cases.push(Case::invalid("no eligible expression", "express", json!([unavailable])));
        for options in [
            json!({"style":"unknown"}),
            json!({"style":"toString"}),
            json!({"style":4}),
            json!({"contractions":1}),
            json!({"probability":null}),
            json!({"probability":-0.1}),
            json!({"probability":true}),
            json!({"max_changes":1.5}),
            json!({"max_changes":17}),
            json!({"max_changes":false}),
            json!({"typos":null}),
            json!({"typos":{"probability":2}}),
            json!({"typos":{"max_errors":-1}}),
            json!({"typos":{"kinds":["random"]}}),
            json!({"typos":{"extra":true}}),
            json!({"protected":[""]}),
            json!({"protected":[1]}),
            json!({"alternatives":[{"from":"x","to":[]}]}),
            json!({"alternatives":[{"from":"x","to":[null]}]}),
            json!({"casing":"upper"}),
            json!({"punctuation":"random"}),
        ] {
            cases.push(Case::invalid(
                format!("style options {options}"),
                "vary",
                json!(["Thanks.", options]),
            ));
        }
        cases
    }

    fn input_value(value: &Value) -> bts_lang::Value {
        use bts_lang::{Array, ArrayElem, Number, Object, ObjectField, Value as ModelValue};
        match value {
            Value::Array(values) => ModelValue::Array(Array {
                elem: values.iter().map(|value| ArrayElem::Item(input_value(value))).collect(),
            }),
            Value::Object(fields) => ModelValue::Object(Object {
                elem: fields
                    .iter()
                    .map(|(key, value)| ObjectField {
                        key: key.clone(),
                        value: input_value(value),
                    })
                    .collect(),
            }),
            Value::String(value) => ModelValue::Str(value.clone()),
            Value::Bool(value) => ModelValue::Bool(*value),
            Value::Null => ModelValue::Null,
            Value::Number(value) => ModelValue::Num(
                value
                    .as_i64()
                    .map(Number::Int)
                    .unwrap_or_else(|| Number::Float(value.as_f64().unwrap())),
            ),
        }
    }

    fn native(case: &Case) -> BTreeSet<String> {
        let args = case.args.as_array().unwrap();
        let call = format!(
            "{}({})",
            case.operation,
            (0..args.len())
                .map(|index| format!("self.input.args[{index}]"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let source = format!("trace \"corpus\" {{ input = {{ args = [] }} output = {call} }}");
        let mut model = compile(&source).unwrap_or_else(|errors| panic!("{} did not compile: {errors:?}\n{source}", case.name));
        model.traces[0].fields.input = Some(input_value(&json!({"args":case.args})));
        let mut results = BTreeSet::new();
        for seed in 0..if case.variable { 64 } else { 1 } {
            let generated = data::generate(
                model.clone(),
                1,
                Duration::from_secs(1),
                Distribution::Linear,
                std::time::UNIX_EPOCH + Duration::from_secs(1_000),
                seed,
                &(),
            );
            let result = match generated {
                Ok(batch) => {
                    let value = batch
                        .events
                        .iter()
                        .find_map(|event| event.output.as_ref())
                        .unwrap()
                        .project(&())
                        .unwrap();
                    assert!(case.valid, "{} unexpectedly accepted {value}", case.name);
                    if let Some(expected) = &case.expected {
                        assert_eq!(&value, expected, "{}", case.name);
                    }
                    json!({"ok":true,"value":value})
                }
                Err(error) => {
                    assert!(!case.valid, "{} unexpectedly failed: {error}", case.name);
                    json!({"ok":false})
                }
            };
            results.insert(result.to_string());
        }
        results
    }

    fn style_replays() -> (Vec<Value>, Vec<Value>) {
        use bts_core::english::{Sentence, StyleOptions, vary_sentence_styled, vary_styled};
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;
        let mut inputs = Vec::new();
        let mut expected = Vec::new();
        let sentences = [
            json!("I am checking the message. Thank you."),
            json!("I want to check the file. By the way, please send the address as soon as possible."),
            json!("I will not leave. They cannot stay."),
            json!("Thanks. Hello. Please share the message."),
            json!("Maya said: \"please\". `thanks` https://example.com/please. hello 👩‍💻 please"),
            json!("If the guest arrives late, can they check in?"),
            json!({"clause":{"subject":{"text":"I","person":"first"},"predicate":{"verb":"check","complement":"the message"},"aspect":"progressive"}}),
            json!({"clause":{"subject":{"text":"Please"},"predicate":{"verb":"check","complement":"the message"}}}),
            json!({"clause":{"subject":{"text":"Élodie"},"predicate":{"verb":"check","complement":"the message"}}}),
        ];
        for style in [
            "neutral",
            "professional",
            "conversational",
            "warm",
            "terse",
            "text_message",
            "enthusiastic",
            "informal_chat",
        ] {
            for probability in [0.0, 0.5, 1.0] {
                for sentence in &sentences {
                    let value = json!({"style":style,"max_changes":8,"probability":probability,"typos":{"probability":0.5,"max_errors":2},"soften_requests":true,"protected":["address"]});
                    let options: StyleOptions = serde_json::from_value(value.clone()).unwrap();
                    for seed in 0..4 {
                        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
                        let mut tape = rng.clone();
                        let result = if let Some(text) = sentence.as_str() {
                            vary_styled(text, &options, &mut rng)
                        } else {
                            vary_sentence_styled(
                                &serde_json::from_value::<Sentence>(sentence.clone()).unwrap(),
                                &options,
                                &mut rng,
                            )
                        }
                        .unwrap();
                        inputs.push(
                            json!({"args":[sentence,value],"draws":(0..64).map(|_| tape.random::<f64>()).collect::<Vec<_>>()}),
                        );
                        expected.push(json!({"text":result.text,"next":rng.random::<f64>()}));
                    }
                }
            }
        }
        (inputs, expected)
    }

    fn expression_replays() -> (Vec<Value>, Vec<Value>) {
        use bts_core::english::*;
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;
        let source = super::flight_expression_source();
        let mut inputs = Vec::new();
        let mut expected = Vec::new();
        for style in ["neutral", "conversational", "text_message", "warm"] {
            for seed in 0..24 {
                let mut source = source.clone();
                source.optional_details = vec!["meeting".into()];
                let mut options:ExpressionOptions=serde_json::from_value(json!({"speaker":{"wording":{"style":style,"max_changes":8,"typos":{"probability":0.3}},"greetings":[{"text":"Hi!"},{"text":"Hello."}],"closings":[{"text":"Thanks."}],"closing_probability":0.4,"max_details":2,"family_weights":{"desire":2},"pattern_weights":{"desire_want":0}}})).unwrap();
                for turn in 0..3 {
                    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed + turn);
                    let mut tape = rng.clone();
                    let result = expression(&source, &options, &mut rng).unwrap();
                    inputs.push(
                        json!({"args":[source,options],"draws":(0..128).map(|_|tape.random::<f64>()).collect::<Vec<_>>()}),
                    );
                    options.context = result.context.clone();
                    expected.push(json!({"value":result,"next":rng.random::<f64>()}));
                }
            }
        }
        for source in super::message_sources() {
            for style in ["neutral", "text_message"] {
                let mut options = super::message_response_options(&source);
                options.speaker.wording.style = Some(if style == "neutral" {
                    Style::Neutral
                } else {
                    Style::TextMessage
                });
                options.speaker.organization.insert("circumstance_first".into(), 2.0);
                options.speaker.reference_style.insert("contextual".into(), 3.0);
                for seed in 0..24 {
                    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
                    let mut tape = rng.clone();
                    let result = expression(&source, &options, &mut rng).unwrap();
                    inputs.push(
                        json!({"args":[source,options],"draws":(0..128).map(|_|tape.random::<f64>()).collect::<Vec<_>>()}),
                    );
                    expected.push(json!({"value":result,"next":rng.random::<f64>()}));
                }
            }
        }
        // Unicode before edits must use the same UTF-8 byte ranges in every runtime.
        for text in [
            "Élodie, please send the message.",
            "👩‍💻 thanks for the update.",
            "李雷 please send the message.",
        ] {
            let source: ExpressionSource = serde_json::from_value(
                json!({"intent":"unicode","families":[{"id":"authored","kind":"authored","text":text}]}),
            )
            .unwrap();
            let options: ExpressionOptions =
                serde_json::from_value(json!({"speaker":{"wording":{"typos":{"probability":1},"punctuation":"omit_period"}}}))
                    .unwrap();
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
            let mut tape = rng.clone();
            let result = expression(&source, &options, &mut rng).unwrap();
            inputs.push(json!({"args":[source,options],"draws":(0..128).map(|_|tape.random::<f64>()).collect::<Vec<_>>()}));
            expected.push(json!({"value":result,"next":rng.random::<f64>()}));
        }
        (inputs, expected)
    }

    #[test]
    fn english_contract_matches_native_python_and_typescript() {
        let cases = corpus();
        let native: Vec<_> = cases.iter().map(native).collect();
        let payload: Vec<_> = cases
            .iter()
            .map(|case| json!({"operation":case.operation,"args":case.args,"variable":case.variable}))
            .collect();
        let (replays, expected_replays) = style_replays();
        let (expressions, expected_expressions) = expression_replays();
        let payload = json!({"cases":payload,"replays":replays,"expressions":expressions});
        let model = compile(r#"scorer "corpus" { code { score = len(realize(output)) } } scorer "styled" { code { score = len(vary(output, {style = "text_message", max_changes = 6})) } } scorer "procedural" { code { score = len(express(output)) + len(expression_candidates(output)) + len(expression(output)["text"]) } }"#).unwrap();
        let ScorerKind::Code { score, steps } = &model.scorers[0].kind else {
            panic!("code scorer")
        };
        let items = [Item {
            name: "corpus",
            slug: "corpus",
            score,
            steps,
        }];
        let ScorerKind::Code { score, steps } = &model.scorers[1].kind else {
            panic!("code scorer")
        };
        let [corpus] = items;
        let items = [
            corpus,
            Item {
                name: "styled",
                slug: "styled",
                score,
                steps,
            },
        ];
        let [corpus, styled] = items;
        let ScorerKind::Code { score, steps } = &model.scorers[2].kind else {
            panic!("code scorer")
        };
        let items = [
            corpus,
            styled,
            Item {
                name: "procedural",
                slug: "procedural",
                score,
                steps,
            },
        ];
        let root = std::env::temp_dir().join(format!("bts-english-parity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        for (lang, program, filename, script) in [
            (
                ScorerLang::Python,
                "python3",
                "corpus.py",
                r#"
import json, sys
assert scorer_corpus({'clause':{'subject':{'text':'the guest'},'predicate':{'verb':'check','complement':'in'}}})['score'] == len('The guest checks in.')
assert scorer_styled('Thank you.')['score'] == len('thanks')
assert scorer_procedural({'intent':'test','constructions':[{'id':'change','predicate':{'verb':'change','complement':'my flight'}}],'families':[{'id':'need','kind':'need'}]})['score'] == 2*len('I need to change my flight.')+1
results = []
payload = json.load(sys.stdin)
for case in payload["cases"]:
    values = []
    for draw in ([i / 32 for i in range(32)] if case['variable'] else [0]):
        try:
            values.append({'ok':True, 'value':_language_english(case['operation'], case['args'], lambda: draw)})
        except Exception:
            values.append({'ok':False})
    results.append(values)
replays = []
for case in payload['replays']:
    draws = iter(case['draws'])
    value = _language_english('vary', case['args'], lambda: next(draws))
    replays.append({'text':value,'next':next(draws)})
expressions = []
for case in payload['expressions']:
    draws = iter(case['draws'])
    value = _language_english('expression',case['args'],lambda:next(draws))
    expressions.append({'value':value,'next':next(draws)})
print(json.dumps({'cases':results,'replays':replays,'expressions':expressions}, ensure_ascii=False))
"#,
            ),
            (
                ScorerLang::Typescript,
                "node",
                "corpus.mts",
                r#"
import {readFileSync} from 'node:fs';
if (scorer_corpus({output:{clause:{subject:{text:'the guest'},predicate:{verb:'check',complement:'in'}}}}).score !== 'The guest checks in.'.length) throw new Error('English scorer execution failed');
if (scorer_styled({output:'Thank you.'}).score !== 'thanks'.length) throw new Error('Styled scorer execution failed');
if (scorer_procedural({output:{intent:'test',constructions:[{id:'change',predicate:{verb:'change',complement:'my flight'}}],families:[{id:'need',kind:'need'}]}}).score !== 2*'I need to change my flight.'.length+1) throw new Error('Procedural scorer execution failed');
const payload = JSON.parse(readFileSync(0, 'utf8'));
const results = payload.cases.map((test: any) => {
    const draws = test.variable ? Array.from({length:32}, (_, i) => i / 32) : [0];
    return draws.map((draw: number) => {
        try { return {ok:true,value:_language_english(test.operation,test.args,() => draw)}; }
        catch { return {ok:false}; }
    });
});
const replays = payload.replays.map((test: any) => {
    let position = 0;
    const value = _language_english('vary',test.args,()=>test.draws[position++]);
    return {text:value,next:test.draws[position]};
});
const expressions = payload.expressions.map((test: any) => {
    let position=0;
    const value=_language_english('expression',test.args,()=>test.draws[position++]);
    return {value,next:test.draws[position]};
});
console.log(JSON.stringify({cases:results,replays,expressions}));
"#,
            ),
        ] {
            let source = Languages.emit(lang, &items).unwrap().source;
            let path = root.join(filename);
            fs::write(&path, format!("{source}\n{script}")).unwrap();
            let mut child = std::process::Command::new(program)
                .arg(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(serde_json::to_string(&payload).unwrap().as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{lang:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let generated: Value = serde_json::from_slice(&output.stdout).unwrap();
            for (index, (actual, expected)) in generated["replays"]
                .as_array()
                .unwrap()
                .iter()
                .zip(&expected_replays)
                .enumerate()
            {
                assert_eq!(
                    actual["text"], expected["text"],
                    "{lang:?} replay {index}: {}",
                    payload["replays"][index]["args"]
                );
                assert!(
                    (actual["next"].as_f64().unwrap() - expected["next"].as_f64().unwrap()).abs() < 1e-15,
                    "{lang:?} random stream replay {index}: {actual} != {expected}"
                );
            }
            assert_eq!(generated["expressions"].as_array().unwrap().len(), expected_expressions.len());
            for (index, (actual, expected)) in generated["expressions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(&expected_expressions)
                .enumerate()
            {
                assert_eq!(
                    actual["value"], expected["value"],
                    "{lang:?} expression replay {index}: {}",
                    payload["expressions"][index]["args"]
                );
                assert!(
                    (actual["next"].as_f64().unwrap() - expected["next"].as_f64().unwrap()).abs() < 1e-15,
                    "{lang:?} expression random stream {index}"
                );
            }
            let generated: Vec<Vec<Value>> = serde_json::from_value(generated["cases"].clone()).unwrap();
            assert_eq!(generated.len(), cases.len());
            let mismatches: Vec<_> = generated
                .iter()
                .zip(&native)
                .zip(&cases)
                .filter_map(|((actual, expected), case)| {
                    let actual: BTreeSet<_> = actual.iter().map(Value::to_string).collect();
                    (actual != *expected).then(|| {
                        format!(
                            "{}: {} {}\nnative {expected:?}\n{lang:?} {actual:?}",
                            case.name, case.operation, case.args
                        )
                    })
                })
                .collect();
            assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
        }
    }
}

fn styled(text: &str, options: serde_json::Value) -> StyleResult {
    let options: StyleOptions = serde_json::from_value(options).unwrap();
    vary_styled(text, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn style_features_have_concrete_outputs_and_explanations() {
    use serde_json::json;
    for (source, controls, expected) in [
        ("I will check the file.", json!({"contractions":true}), "I'll check the file."),
        (
            "We have received the file.",
            json!({"contractions":true}),
            "We've received the file.",
        ),
        (
            "She has received the file.",
            json!({"contractions":true}),
            "She's received the file.",
        ),
        (
            "I had checked the file.",
            json!({"contractions":true}),
            "I'd checked the file.",
        ),
        (
            "By the way, the file is ready.",
            json!({"shorthand":true}),
            "Btw, the file is ready.",
        ),
        ("Thank you for the update.", json!({"casual":true}), "Thanks for the update."),
        ("Understood.", json!({"casual":true}), "Got it."),
        ("See you later.", json!({"slang":true}), "Catch you later."),
        (
            "I am going to check the file.",
            json!({"reductions":true}),
            "I am gonna check the file.",
        ),
        (
            "I want to check the file.",
            json!({"reductions":true}),
            "I wanna check the file.",
        ),
        ("I am checking the file.", json!({"fragments":true}), "Checking the file."),
        ("Thanks.", json!({"warmth":true}), "Thanks so much."),
        ("Send the file.", json!({"soften_requests":true}), "Could you send the file?"),
        (
            "Please share the file.",
            json!({"soften_requests":true}),
            "Could you please share the file?",
        ),
        ("Thanks.", json!({"punctuation":"omit_period"}), "Thanks"),
        ("Thanks.", json!({"punctuation":"exclamation"}), "Thanks!"),
        ("Thanks!", json!({"punctuation":"expressive"}), "Thanks!!"),
        (
            "I'll check with Maya.",
            json!({"casing":"lowercase"}),
            "i'll check with Maya.",
        ),
        ("please", json!({"typos":{"probability":1,"kinds":["common"]}}), "plese"),
        ("please", json!({"typos":{"probability":1,"kinds":["omission"]}}), "plase"),
        (
            "please",
            json!({"typos":{"probability":1,"kinds":["transposition"]}}),
            "pelase",
        ),
        ("please", json!({"typos":{"probability":1,"kinds":["repetition"]}}), "pllease"),
    ] {
        let result = styled(source, controls);
        assert_eq!(result.text, expected, "{source}");
        assert_eq!(result.edits.len(), 1, "{source}");
        let edit = &result.edits[0];
        let mut replay = source.to_owned();
        replay.replace_range(edit.range.clone(), &edit.replacement);
        assert_eq!(replay, expected);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn styles_preserve_ambiguous_grammar_protected_text_and_questions() {
    use serde_json::json;
    for (text, options) in [
        ("I am going to London.", json!({"reductions":true})),
        ("I have a file.", json!({"contractions":true})),
        ("I'd check the file.", json!({"contractions":true})),
        ("I am not checking the file.", json!({"fragments":true,"negation":false})),
        ("I am checking the file?", json!({"fragments":true})),
        ("I am checking if the file exists.", json!({"fragments":true})),
        ("I will thank you tomorrow.", json!({"casual":true})),
        ("I am.", json!({"contractions":true})),
        ("Yes?", json!({"punctuation":"expressive"})),
        ("Wait...", json!({"punctuation":"omit_period"})),
        ("Ask Dr.", json!({"punctuation":"omit_period"})),
        ("version_1.2", json!({"punctuation":"exclamation","casing":"lowercase"})),
        (
            "https://example.com/thanks.",
            json!({"punctuation":"omit_period","casual":true}),
        ),
        (
            "\"Thank you.\" `please` ‘thanks’",
            json!({"casual":true,"typos":{"probability":1}}),
        ),
        ("'Thank you.'", json!({"casual":true})),
        ("checking@example.com please_42 please-id", json!({"typos":{"probability":1}})),
        ("please", json!({"protected":["please"],"typos":{"probability":1}})),
        (
            "Maya Élodie 李雷 👩‍💻 café e\u{301}",
            json!({"style":"text_message","typos":{"probability":1}}),
        ),
        ("cannot must not never 12345", json!({"typos":{"probability":1}})),
        ("Thanks.", json!({"style":"enthusiastic","max_changes":0})),
    ] {
        assert_eq!(styled(text, options).text, text);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn style_presets_overrides_and_authored_alternatives_are_explicit() {
    use serde_json::json;
    for style in [
        Style::Neutral,
        Style::Professional,
        Style::Conversational,
        Style::Warm,
        Style::Terse,
        Style::TextMessage,
        Style::Enthusiastic,
        Style::InformalChat,
    ] {
        let resolved = StyleOptions {
            style: Some(style),
            ..StyleOptions::default()
        }
        .resolved()
        .unwrap();
        assert_eq!(resolved.typos.probability, 0.0);
        assert_eq!(resolved.soften_requests, Some(false));
    }
    assert_eq!(
        styled(
            "I will check the file.",
            json!({"style":"conversational","contractions":false})
        )
        .text,
        "I will check the file."
    );
    assert_eq!(
        styled("Thanks.", json!({"style":"enthusiastic","warmth":false})).text,
        "Thanks!"
    );
    assert_eq!(
        styled(
            "I will check.",
            json!({"style":"neutral","alternatives":[{"from":"I will check","to":["I will check right away"]}]})
        )
        .text,
        "I will check right away."
    );
    assert_eq!(
        styled("Thanks.", json!({"style":"warm","max_changes":8})).text,
        "Thanks so much."
    );
    let options: StyleOptions = serde_json::from_value(json!({"style":"text_message","max_changes":16})).unwrap();
    let candidates = style_edits("I am checking the file. Thank you.", &options).unwrap();
    assert!(candidates.iter().any(|edit| edit.rule.starts_with("fragments:")));
    assert!(candidates.iter().any(|edit| edit.rule.starts_with("contractions:")));
    for seed in 0..32 {
        let source = "I am checking the file. Thank you.";
        let result = vary_styled(source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(seed)).unwrap();
        assert!(!result.text.contains("I'm Checking"));
        let mut replay = source.to_owned();
        for edit in &result.edits {
            replay.replace_range(edit.range.clone(), &edit.replacement);
        }
        assert_eq!(replay, result.text);
        assert!(result.edits.len() <= 16);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn style_probabilities_budgets_and_rng_are_bounded() {
    use rand::Rng;
    use serde_json::json;
    let source = "please send the message tomorrow";
    let options: StyleOptions =
        serde_json::from_value(json!({"probability":0,"typos":{"probability":1,"max_errors":2},"max_changes":1})).unwrap();
    let result = vary_styled(source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(0)).unwrap();
    assert_eq!(result.edits.len(), 1);
    assert!(result.edits[0].rule.starts_with("typo:"));
    for options in [
        json!({"max_changes":0}),
        json!({"casual":true,"probability":0}),
        json!({"style":"neutral"}),
    ] {
        let options: StyleOptions = serde_json::from_value(options).unwrap();
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut original = rng.clone();
        let result = vary_styled("Thank you.", &options, &mut rng).unwrap();
        assert_eq!(result.text, "Thank you.");
        assert_eq!(rng.random::<u64>(), original.random::<u64>());
    }
    let options: StyleOptions = serde_json::from_value(json!({"casual":true,"probability":0.5})).unwrap();
    let outputs: Vec<_> = (0..128)
        .map(|seed| {
            vary_styled("Thank you.", &options, &mut Xoshiro256PlusPlus::seed_from_u64(seed))
                .unwrap()
                .text
        })
        .collect();
    assert!(outputs.iter().any(|s| s == "Thanks."));
    assert!(outputs.iter().any(|s| s == "Thank you."));
    for options in [
        json!({"probability":-1}),
        json!({"max_changes":17}),
        json!({"typos":{"probability":2}}),
        json!({"protected":[""]}),
        json!({"alternatives":[{"from":"x","to":[]}]}),
    ] {
        let parsed = serde_json::from_value::<StyleOptions>(options);
        assert!(parsed.is_err() || parsed.unwrap().resolved().is_err());
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn structured_styles_keep_authored_references_opaque() {
    use serde_json::json;
    let sentence: Sentence = serde_json::from_value(
        json!({"clause":{"subject":{"text":"Please"},"predicate":{"verb":"check","complement":"the file"}}}),
    )
    .unwrap();
    let options: StyleOptions =
        serde_json::from_value(json!({"style":"text_message","typos":{"probability":1},"max_changes":16})).unwrap();
    let result = vary_sentence_styled(&sentence, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap();
    assert!(result.text.starts_with("Please checks"));
    let mut unicode = sentence.clone();
    unicode.clause.subject.text = "Élodie".into();
    assert!(
        vary_sentence_styled(&unicode, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42))
            .unwrap()
            .text
            .starts_with("Élodie checks")
    );
    let sentence: Sentence = serde_json::from_value(json!({"clause":{"subject":{"text":"I","person":"first"},"predicate":{"verb":"check","complement":"the file"},"aspect":"progressive"}})).unwrap();
    assert_eq!(
        vary_sentence_styled(
            &sentence,
            &StyleOptions {
                fragments: Some(true),
                ..StyleOptions::default()
            },
            &mut Xoshiro256PlusPlus::seed_from_u64(42)
        )
        .unwrap()
        .text,
        "Checking the file."
    );
}

fn flight_expression_source() -> ExpressionSource {
    serde_json::from_value(serde_json::json!({
        "intent":"change_flight",
        "constructions":[
            {"id":"change","predicate":{"verb":"change","complement":"my flight"}},
            {"id":"move","predicate":{"verb":"move","complement":"my booking to Friday"},"mentions":["day"]},
            {"id":"switch","predicate":{"verb":"switch","complement":"to a different flight"}}
        ],
        "families":[
            {"id":"request","kind":"request","establishes":["flight_requested"]},
            {"id":"desire","kind":"desire","establishes":["flight_requested"]},
            {"id":"possibility","kind":"possibility","establishes":["flight_requested"]},
            {"id":"procedure","kind":"procedure","establishes":["flight_requested"]},
            {"id":"need","kind":"need","establishes":["flight_requested"]},
            {"id":"reason","kind":"reason_request","reason":"meeting","establishes":["flight_requested"]},
            {"id":"follow","kind":"follow_up","text":"What about Friday?","context_requires":["flight_requested"],"mentions":["day"]}
        ],
        "facts":[{"id":"day","text":"Friday"},{"id":"meeting","text":"My meeting was moved."}],
        "protected":["Friday"]
    })).unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn expression_families_generalize_across_domains_without_mixing_constructions() {
    use serde_json::json;
    for (intent, constructions) in [
        (
            "change_flight",
            json!([{"id":"change","predicate":{"verb":"change","complement":"my flight"}},{"id":"switch","predicate":{"verb":"switch","complement":"to a different flight"}}]),
        ),
        (
            "reschedule_appointment",
            json!([{"id":"move","predicate":{"verb":"move","complement":"my appointment to Monday"}},{"id":"change","predicate":{"verb":"change","complement":"the time of my appointment"}}]),
        ),
        (
            "damaged_delivery",
            json!([{"id":"report","predicate":{"verb":"report","complement":"a damaged delivery"}},{"id":"replace","predicate":{"verb":"replace","complement":"a broken item"}}]),
        ),
    ] {
        let source: ExpressionSource = serde_json::from_value(json!({"intent":intent,"constructions":constructions,"families":[{"id":"request","kind":"request"},{"id":"desire","kind":"desire"},{"id":"procedure","kind":"procedure"}]})).unwrap();
        let before = source.clone();
        let candidates = expression_candidates(&source, &ExpressionOptions::default()).unwrap();
        assert_eq!(candidates.len(), 18);
        assert!((candidates.iter().map(|c| c.probability).sum::<f64>() - 1.0).abs() < 1e-12);
        for c in &candidates {
            let construction = source
                .constructions
                .iter()
                .find(|k| Some(&k.id) == c.selection.construction.as_ref())
                .unwrap();
            assert!(
                c.text.contains(construction.predicate.complement.as_ref().unwrap()),
                "{}",
                c.text
            );
        }
        let mut distinct = std::collections::BTreeSet::new();
        for seed in 0..128 {
            let mut a = Xoshiro256PlusPlus::seed_from_u64(seed);
            let mut b = a.clone();
            let detailed = expression(&source, &ExpressionOptions::default(), &mut a).unwrap();
            assert_eq!(
                detailed.text,
                express(&source, &ExpressionOptions::default(), &mut b).unwrap()
            );
            assert!(candidates.iter().any(|c| c.text == detailed.base_text));
            distinct.insert(detailed.text);
        }
        assert_eq!(distinct.len(), 18);
        assert_eq!(source, before);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn expression_context_prevents_unfounded_reasons_and_followups() {
    let mut source = flight_expression_source();
    source.facts.clear();
    let options = ExpressionOptions::default();
    let candidates = expression_candidates(&source, &options).unwrap();
    assert!(!candidates.iter().any(|c| c.selection.family == "reason"
        || c.selection.family == "follow"
        || c.selection.construction.as_deref() == Some("move")));
    let first = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap();
    assert!(first.context.established.contains(&"flight_requested".into()));
    assert!(first.used_facts.is_empty());
    assert!(options.context.established.is_empty());
    let mut follow = flight_expression_source();
    follow.families.retain(|f| f.id == "follow");
    assert!(expression_candidates(&follow, &options).unwrap().is_empty());
    assert!(express(&follow, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).is_err());
    let next_options = ExpressionOptions {
        context: first.context,
        ..options
    };
    let next = expression(&follow, &next_options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap();
    assert_eq!(next.text, "What about Friday?");
    assert_eq!(next.used_facts, ["day"]);
    assert!(next.context.established.contains(&"day".into()));
    assert_eq!(next.context.recent.len(), 2);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn expression_weights_profiles_and_repetition_control_populations() {
    use serde_json::json;
    let source = flight_expression_source();
    let mut options: ExpressionOptions = serde_json::from_value(json!({"speaker":{"wording":{"style":"conversational"},"family_weights":{"desire":3},"pattern_weights":{"desire_want":0,"desire_looking_to":0}}})).unwrap();
    let candidates = expression_candidates(&source, &options).unwrap();
    let total = |family: &str| {
        candidates
            .iter()
            .filter(|c| c.selection.family == family)
            .map(|c| c.probability)
            .sum::<f64>()
    };
    assert!((total("desire") / total("request") - 3.0).abs() < 1e-12);
    assert!(
        candidates
            .iter()
            .filter(|c| c.selection.family == "desire")
            .all(|c| c.selection.pattern == "desire_would_like")
    );
    let first = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(4)).unwrap();
    options.context = first.context;
    options.repeat_penalty = 0.0;
    let alternatives = expression_candidates(&source, &options).unwrap();
    assert!(!alternatives.iter().any(|c| c.selection.family == first.selection.family));
    assert!(
        !alternatives
            .iter()
            .any(|c| c.selection.construction.is_some() && c.selection.construction == first.selection.construction)
    );
    let mut more = source.clone();
    more.constructions.push(
        serde_json::from_value(json!({"id":"different","predicate":{"verb":"change","complement":"the departure date"}}))
            .unwrap(),
    );
    options.context = ExpressionContext::default();
    let more = expression_candidates(&more, &options).unwrap();
    assert!(
        (more
            .iter()
            .filter(|c| c.selection.family == "desire")
            .map(|c| c.probability)
            .sum::<f64>()
            - total("desire"))
        .abs()
            < 1e-12
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn expression_optional_material_and_styles_preserve_explicit_state() {
    use serde_json::json;
    let mut source = flight_expression_source();
    source.families.retain(|f| f.id == "need");
    source.constructions.retain(|c| c.id == "change");
    source.optional_details = vec!["meeting".into()];
    let mut options: ExpressionOptions = serde_json::from_value(json!({"speaker":{"greetings":[{"text":"Hi!"}],"greeting_probability":1,"closings":[{"text":"Thanks."}],"closing_probability":1,"detail_probability":1,"wording":{"style":"text_message","max_changes":8}}})).unwrap();
    let original = options.clone();
    let first = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap();
    assert_eq!(
        first.base_text,
        "Hi! I need to change my flight. My meeting was moved. Thanks."
    );
    assert_eq!(first.used_facts, ["meeting"]);
    let mut replay = first.base_text.clone();
    for edit in &first.edits {
        replay.replace_range(edit.range.clone(), &edit.replacement);
    }
    assert_eq!(replay, first.text);
    assert_eq!(original, options);
    options.context = first.context;
    let second = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42)).unwrap();
    assert_eq!(second.base_text, "I need to change my flight. Thanks.");
    assert!(second.used_facts.is_empty());
    assert_eq!(options.speaker, original.speaker);
    options.context.recent = vec![first.selection; 32];
    assert_eq!(
        expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(42))
            .unwrap()
            .context
            .recent
            .len(),
        32
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn expression_custom_verbs_and_authored_families_do_not_require_domain_code() {
    use serde_json::json;
    let source:ExpressionSource=serde_json::from_value(json!({"intent":"custom","constructions":[{"id":"custom","predicate":{"verb":{"base":"rebook","third_person_singular":"rebooks","past":"rebooked","past_participle":"rebooked","present_participle":"rebooking"},"complement":"my flight"}}],"families":[{"id":"procedure","kind":"procedure","patterns":["procedure_go_about"]}]})).unwrap();
    assert_eq!(
        express(
            &source,
            &ExpressionOptions::default(),
            &mut Xoshiro256PlusPlus::seed_from_u64(42)
        )
        .unwrap(),
        "How do I go about rebooking my flight?"
    );
    let authored:ExpressionSource=serde_json::from_value(json!({"intent":"damaged_delivery","families":[{"id":"report","kind":"authored","text":"The package arrived with a cracked screen.","mentions":["damage"]}],"facts":[{"id":"damage","text":"cracked screen"}]})).unwrap();
    assert_eq!(
        express(
            &authored,
            &ExpressionOptions::default(),
            &mut Xoshiro256PlusPlus::seed_from_u64(42)
        )
        .unwrap(),
        "The package arrived with a cracked screen."
    );
    let mut missing = authored;
    missing.facts.clear();
    assert!(
        expression_candidates(&missing, &ExpressionOptions::default())
            .unwrap()
            .is_empty()
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn procedural_question_casing_and_modal_protection_are_independent() {
    assert_eq!(
        styled(
            "hey, How can I change my flight?",
            serde_json::json!({"casing":"lowercase","max_changes":8})
        )
        .text,
        "hey, how can i change my flight?"
    );
    assert_eq!(
        styled("What about Friday?", serde_json::json!({"casing":"lowercase"})).text,
        "what about Friday?"
    );
    assert_eq!(
        styled(
            "could would should must not",
            serde_json::json!({"typos":{"probability":1,"max_errors":16},"max_changes":16})
        )
        .text,
        "could would should must not"
    );
}

fn message_sources() -> Vec<ExpressionSource> {
    use serde_json::json;
    // The last two scenarios reuse the same layouts with new domain content.
    [
        ("flight", "change", "my flight", "My meeting has moved to Friday", "my meeting has moved to Friday", "My flight leaves on Thursday", "I need to arrive on Friday", "I cannot leave before noon", "My booking reference is AB42", "departure_day", "Friday", "I would like to leave on Friday", "Thursday"),
        ("delivery", "replace", "the damaged mug", "The handle broke during delivery", "the handle broke during delivery", "I have a mug with a broken handle", "I need a replacement mug", "I cannot return it until Monday", "My order number is ZX91", "delivery_day", "Monday", "I can accept the replacement on Monday", "Tuesday"),
        ("product", "check", "the charger specifications", "I am replacing a lost charger", "I am replacing a lost charger", "I have the laptop but no charger", "I need a compatible charger", "I need delivery before Saturday", "The model number is L42", "model", "L42", "My laptop is the L42", "L41"),
        ("charge", "review", "a duplicate charge", "The same payment appears twice", "the same payment appears twice", "I see two payments for one order", "I need the extra payment reversed", "I need the money before rent is due", "Both payments are for $42", "payment_day", "Tuesday", "The payment was on Tuesday", "Wednesday"),
        ("appointment", "move", "my appointment", "My work schedule has changed", "my work schedule has changed", "My appointment is on Monday", "I need an appointment on Wednesday", "I can only attend in the afternoon", "My appointment reference is AP42", "appointment_day", "Wednesday", "I would like an appointment on Wednesday", "Monday"),
    ].into_iter().map(|(topic,verb,object,reason,reason_clause,current,outcome,constraint,detail,slot,value,answer,previous)| {
        serde_json::from_value(json!({
            "intent":topic, "topic":topic,
            "facts":[{"id":"reason","text":reason},{"id":"current","text":current},{"id":"outcome","text":outcome},{"id":"constraint","text":constraint},{"id":"detail","text":detail},{"id":"answer","text":value},{"id":"previous","text":previous}],
            "protected":["AB42","ZX91","L42","L41","$42","AP42"],
            "constructions":[{"id":"action","predicate":{"verb":verb,"complement":object}}],
            "components":[
                {"id":"reason","role":"reason","mentions":["reason"],"forms":[{"id":"sentence","text":reason},{"id":"clause","text":reason_clause,"shape":"clause"}]},
                {"id":"current","role":"current","mentions":["current"],"forms":[{"id":"sentence","text":current}]},
                {"id":"outcome","role":"outcome","mentions":["outcome"],"forms":[{"id":"sentence","text":outcome},{"id":"clause","text":outcome,"shape":"clause"},{"id":"fragment","text":outcome.strip_prefix("I need ").unwrap_or(outcome),"shape":"fragments"}]},
                {"id":"constraint","role":"constraint","mentions":["constraint"],"forms":[{"id":"sentence","text":constraint}]},
                {"id":"detail","role":"detail","mentions":["detail"],"forms":[{"id":"sentence","text":detail}]},
                {"id":"answer","role":"answer","mentions":["answer"],"slot":slot,"option":"available","forms":[{"id":"explicit","text":answer},{"id":"short","text":value,"shape":"fragments","reference":"contextual","context_requires":["asked"]}]},
                {"id":"previous","role":"previous","mentions":["previous"],"slot":slot,"forms":[{"id":"value","text":previous,"shape":"value"}]},
                {"id":"replacement","role":"replacement","mentions":["answer"],"slot":slot,"forms":[{"id":"value","text":value,"shape":"value"},{"id":"sentence","text":answer}]},
                {"id":"ack","role":"acknowledgment","forms":[{"id":"sentence","text":"That works for me","reference":"contextual","context_requires":["offered"]}]},
                {"id":"clarify","role":"clarification","slot":slot,"forms":[{"id":"sentence","text":"Which date do you mean?","reference":"contextual","context_requires":["asked"]}]},
                {"id":"follow","role":"follow_up_request","forms":[{"id":"sentence","text":"Could you confirm that by email?","reference":"contextual","context_requires":["offered"]}]}
            ],
            "families":[
                {"id":"request","kind":"request","message":{"layout":"request_only"}},
                {"id":"request_reason","kind":"desire","message":{"layout":"request_reason","bindings":{"reason":"reason"},"relation":"cause"}},
                {"id":"reason_request","kind":"need","message":{"layout":"reason_request","bindings":{"reason":"reason"},"relation":"cause"}},
                {"id":"outcome_request","kind":"procedure","message":{"layout":"outcome_request","bindings":{"outcome":"outcome"}}},
                {"id":"current_desired","kind":"structured","message":{"layout":"current_desired","bindings":{"current":"current","outcome":"outcome"},"relation":"contrast"}},
                {"id":"constraint_request","kind":"possibility","message":{"layout":"constraint_request","bindings":{"constraint":"constraint"}}},
                {"id":"detailed","kind":"request","message":{"layout":"request_only","details":["detail"]}},
                {"id":"answer","kind":"structured","message":{"layout":"answer","bindings":{"answer":"answer"}}},
                {"id":"correction","kind":"structured","message":{"layout":"correction","bindings":{"previous":"previous","replacement":"replacement"}}},
                {"id":"ack","kind":"structured","message":{"layout":"acknowledgment","bindings":{"acknowledgment":"ack"}}},
                {"id":"clarify","kind":"structured","message":{"layout":"clarification","bindings":{"clarification":"clarify"}}},
                {"id":"follow","kind":"structured","message":{"layout":"follow_up_request","bindings":{"follow_up_request":"follow"}}}
            ]
        })).unwrap()
    }).collect()
}

fn message_response_options(source: &ExpressionSource) -> ExpressionOptions {
    serde_json::from_value(serde_json::json!({"context":{"topic":source.topic,"established":["asked","offered","previous"],"known":["previous"],"requested":[source.components.iter().find(|c| c.id == "answer").unwrap().slot],"offered":["available"],"allowed_acts":["answer","correction","acknowledgment","clarification","follow_up_request"]}})).unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn message_layouts_generalize_to_development_and_holdout_domains() {
    use std::collections::BTreeSet;
    for source in message_sources() {
        let initial = expression_candidates(&source, &ExpressionOptions::default()).unwrap();
        assert_eq!(
            initial
                .iter()
                .map(|c| c.selection.message.as_ref().unwrap().layout.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        assert!(
            initial
                .iter()
                .all(|c| c.selection.message.as_ref().unwrap().organization != "response")
        );
        let options = message_response_options(&source);
        let candidates = expression_candidates(&source, &options).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.selection.message.as_ref().unwrap().layout.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            11
        );
        assert!((candidates.iter().map(|c| c.probability).sum::<f64>() - 1.0).abs() < 1e-12);
        for candidate in &candidates {
            assert!(!candidate.text.contains('{'));
            assert!(candidate.used_facts.iter().all(|id| source.facts.iter().any(|f| f.id == *id)));
            assert!(!candidate.text.starts_with("Sorry"));
        }
        let mut layouts = BTreeSet::new();
        let mut openings = BTreeSet::new();
        for seed in 0..192 {
            let result = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(seed)).unwrap();
            layouts.insert(result.selection.message.unwrap().layout);
            openings.insert(result.base_text.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
        }
        assert_eq!(layouts.len(), 11);
        assert!(openings.len() >= 12);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn message_preferences_control_structure_without_inventing_facts() {
    let source = message_sources().remove(0);
    let options: ExpressionOptions = serde_json::from_value(serde_json::json!({"speaker":{"organization":{"request_first":0,"circumstance_first":0,"outcome_first":0,"current_then_desired":1,"response":0},"sentence_shape":{"complete":0,"short_sentences":1,"fragments":0}}})).unwrap();
    let candidates = expression_candidates(&source, &options).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].text,
        "My flight leaves on Thursday. I need to arrive on Friday."
    );
    let mut no_cause = source.clone();
    for f in &mut no_cause.families {
        f.message.as_mut().unwrap().relation = None;
    }
    let candidates = expression_candidates(&no_cause, &ExpressionOptions::default()).unwrap();
    assert!(
        candidates
            .iter()
            .all(|c| !c.text.contains("because") && !c.text.contains("Because") && !c.text.contains(", but"))
    );
    no_cause.facts.retain(|f| f.id != "reason");
    assert!(
        expression_candidates(&no_cause, &ExpressionOptions::default())
            .unwrap()
            .iter()
            .all(|c| !c.used_facts.iter().any(|id| id == "reason"))
    );
    let mut weighted = ExpressionOptions::default();
    weighted.speaker.organization.insert("circumstance_first".into(), 5.0);
    let candidates = expression_candidates(&source, &weighted).unwrap();
    let mass = |family: &str| {
        candidates
            .iter()
            .filter(|c| c.selection.family == family)
            .map(|c| c.probability)
            .sum::<f64>()
    };
    assert!((mass("reason_request") / mass("request") - 5.0).abs() < 1e-12);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn response_guards_and_state_updates_are_explicit() {
    let mut source = message_sources().remove(0);
    source.families.retain(|f| f.id == "answer");
    let options = message_response_options(&source);
    let before = options.clone();
    let result = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(4)).unwrap();
    assert!(result.context.requested.is_empty());
    assert!(result.context.allowed_acts.is_empty());
    assert!(result.context.known.contains(&"answer".into()));
    assert_eq!(options, before);
    for control in ["topic", "requested", "offered", "allowed_acts"] {
        let mut blocked = options.clone();
        match control {
            "topic" => blocked.context.topic = Some("different".into()),
            "requested" => blocked.context.requested.clear(),
            "offered" => blocked.context.offered.clear(),
            _ => blocked.context.allowed_acts.clear(),
        }
        assert!(expression_candidates(&source, &blocked).unwrap().is_empty(), "{control}");
    }
    let mut source = message_sources().remove(0);
    source.families.retain(|f| f.id == "correction");
    let result = expression(&source, &options, &mut Xoshiro256PlusPlus::seed_from_u64(2)).unwrap();
    assert!(!result.context.known.contains(&"previous".into()));
    assert!(result.context.established.contains(&"previous".into()));
    let mut blocked = options.clone();
    blocked.context.established.clear();
    assert!(expression_candidates(&source, &blocked).unwrap().is_empty());
    source.families[0].message.as_mut().unwrap().allow_apology = true;
    assert!(
        expression_candidates(&source, &options)
            .unwrap()
            .iter()
            .any(|c| c.text.starts_with("Sorry"))
    );
    source.components.iter_mut().find(|c| c.id == "replacement").unwrap().slot = Some("wrong".into());
    assert!(expression_candidates(&source, &options).is_err());
}
