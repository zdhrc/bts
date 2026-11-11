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
        cases.push(Case::valid(
            "style contraction",
            "vary",
            json!(["I will check the file.", {"style":"conversational"}]),
        ));
        cases.push(Case::valid(
            "style protected phrase",
            "vary",
            json!(["Élodie, please send the message.", {"style":"text_message","protected":["Élodie"]}]),
        ));
        cases.push(Case::valid(
            "controlled typo",
            "vary",
            json!(["please", {"typos":{"probability":1,"kinds":["common"]}}]),
        ));
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

    #[test]
    fn english_contract_matches_native_python_and_typescript() {
        let cases = corpus();
        let native: Vec<_> = cases.iter().map(native).collect();
        let payload: Vec<_> = cases
            .iter()
            .map(|case| json!({"operation":case.operation,"args":case.args,"variable":case.variable}))
            .collect();
        let model = compile(r#"scorer "corpus" { code { score = len(realize(output)) } }"#).unwrap();
        let ScorerKind::Code { score, steps } = &model.scorers[0].kind else {
            panic!("code scorer")
        };
        let items = [Item {
            name: "corpus",
            slug: "corpus",
            score,
            steps,
        }];
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
results = []
for case in json.load(sys.stdin):
    values = []
    for draw in ([i / 32 for i in range(32)] if case['variable'] else [0]):
        try:
            values.append({'ok':True, 'value':_language_english(case['operation'], case['args'], lambda: draw)})
        except Exception:
            values.append({'ok':False})
    results.append(values)
print(json.dumps(results, ensure_ascii=False))
"#,
            ),
            (
                ScorerLang::Typescript,
                "node",
                "corpus.mts",
                r#"
import {readFileSync} from 'node:fs';
if (scorer_corpus({output:{clause:{subject:{text:'the guest'},predicate:{verb:'check',complement:'in'}}}}).score !== 'The guest checks in.'.length) throw new Error('English scorer execution failed');
const results = JSON.parse(readFileSync(0, 'utf8')).map((test: any) => {
    const draws = test.variable ? Array.from({length:32}, (_, i) => i / 32) : [0];
    return draws.map((draw: number) => {
        try { return {ok:true,value:_language_english(test.operation,test.args,() => draw)}; }
        catch { return {ok:false}; }
    });
});
console.log(JSON.stringify(results));
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
            let generated: Vec<Vec<Value>> = serde_json::from_slice(&output.stdout).unwrap();
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
