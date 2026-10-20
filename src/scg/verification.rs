//! Cross-stage generation checks. Execution tests also live in tests/end_to_end.rs.

#[cfg(test)]
mod agents {
    use crate::dsl::ScorerLang;
    use crate::scg::assembly::agents as assemble;

    #[test]
    fn rejects_unsupported_behavior_and_filename_collisions() {
        for source in [
            r#"agent "a-b" { entrypoint = trace.t } agent "a_b" { entrypoint = trace.t } trace "t" { output = "ok" }"#,
            r#"trace "t" { output = "ok" }"#,
        ] {
            for lang in [ScorerLang::Python, ScorerLang::Typescript] {
                assert!(assemble(&crate::dsl::compile(source).unwrap(), lang).is_err(), "{source}");
            }
        }
    }

    #[test]
    fn typescript_rejects_integers_that_would_lose_precision() {
        let model =
            crate::dsl::compile(r#"agent "a" { entrypoint = trace.t } trace "t" { output = 9007199254740993 }"#).unwrap();
        assert!(assemble(&model, ScorerLang::Python).is_ok());
        assert!(
            assemble(&model, ScorerLang::Typescript)
                .err()
                .unwrap()
                .contains("safe integer")
        );
    }
}

#[cfg(test)]
mod scorers {
    use crate::dsl::ScorerLang;
    use crate::dsl::compile;
    use crate::scg::assembly::assemble;

    #[test]
    fn emits_sdk_registrations_for_code_and_judge() {
        let model = compile(include_str!("../../tests/fixtures/scorers.bt")).unwrap();
        let built = assemble(&model.scorers, None, "demo-project").unwrap();
        assert_eq!(built.files.len(), 3);
        let code = &built.files[1].contents;
        assert!(code.contains("project.scorers.create("));
        assert!(code.contains("parameters=scorer_answer_quality_params"));
        assert!(code.contains("handler=scorer_answer_quality"));
        assert!(code.contains("project = braintrust.projects.create(name=\"demo-project\")"));
        let judge = &built.files[2].contents;
        assert!(judge.contains("choice_scores={"));
        assert!(judge.contains("messages=[{"));
    }

    #[test]
    fn emits_typescript_sdk_registrations() {
        let model = compile(include_str!("../../tests/fixtures/scorers.bt")).unwrap();
        let built = assemble(&model.scorers, Some(ScorerLang::Typescript), "demo-project").unwrap();
        assert_eq!(built.files.len(), 3);
        assert!(built.files[1].contents.contains("parameters: z.object("));
        assert!(built.files[1].contents.contains("handler: scorer_answer_quality"));
        assert!(built.files[2].contents.contains("choiceScores:"));
    }
}

#[cfg(test)]
mod naming {
    use crate::scg::slugify;

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("response-quality").as_deref(), Some("response-quality"));
        assert_eq!(slugify("Answer Quality!").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("answer_quality").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("  spaced   out  ").as_deref(), Some("spaced-out"));
        assert_eq!(slugify("v2 scorer").as_deref(), Some("v2-scorer"));
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify(""), None);
    }
}

#[cfg(test)]
mod code {
    use crate::dsl::ScorerLang;
    use crate::scg::{
        emission,
        lowering::code::{Expr, Function, Parameter, Statement, Variable},
    };

    #[test]
    fn function_scopes_own_parameters_and_locals_without_leaking_nested_bindings() {
        let mut outer = Function::new("outer", vec![Parameter::object(&["input", "output"])]);
        assert!(outer.declare("input", "0").is_err());
        outer.declare("result", "output").unwrap();
        assert!(outer.declare("result", "1").is_err());
        let mut inner = Function::new("inner", vec![Parameter::new("input")]);
        inner.declare("result", "input").unwrap();
        inner.declare("private_value", "1").unwrap();
        outer.body.push(Statement::Function(inner));
        assert_eq!(outer.local_names(), ["input", "output", "result", "inner"]);
        assert!(outer.declare("inner", "0").is_err());
        outer.declare("private_value", "2").unwrap();
    }

    #[test]
    fn emits_nested_functions_mutable_declarations_and_reassignments() {
        let mut outer = Function::new("outer", vec![Parameter::new("input").with_default(Expr::number(1))]);
        outer.body.push(Statement::Variable(Variable::Declare {
            name: "result".into(),
            value: "input".into(),
            mutable: true,
            annotation: None,
        }));
        let mut inner = Function::new("inner", vec![Parameter::new("value")]);
        inner.body.push(Statement::Return("value".into()));
        outer.body.push(Statement::Function(inner));
        outer.body.push(Statement::Variable(Variable::Assign {
            name: "result".into(),
            value: Expr::call("inner", vec!["result".into()]),
        }));
        outer.body.push(Statement::Return("result".into()));
        assert_eq!(
            emission::function(&outer, ScorerLang::Python, 4),
            "def outer(input=1):\n    result = input\n    def inner(value):\n        return value\n    result = inner(result)\n    return result\n"
        );
        assert_eq!(
            emission::function(&outer, ScorerLang::Typescript, 2),
            "function outer(input = 1) {\n  let result = input;\n  function inner(value) {\n    return value;\n  }\n  result = inner(result);\n  return result;\n}\n"
        );
    }

    #[test]
    fn preserves_signature_features_and_empty_bodies() {
        let mut function = Function::new(
            "task",
            vec![
                Parameter::new("input"),
                Parameter::new("limit").with_default(Expr::number(1)).keyword(),
            ],
        );
        function.asynchronous = true;
        assert_eq!(
            emission::function(&function, ScorerLang::Python, 4),
            "async def task(input, *, limit=1):\n    pass\n"
        );
        function.parameters = vec![Parameter::object(&["input", "output"]).typed("{ input: number; output: number }")];
        function.exported = true;
        function.returns = Some("Promise<void>".into());
        assert_eq!(
            emission::function(&function, ScorerLang::Typescript, 2),
            "export async function task({ input, output }: { input: number; output: number }): Promise<void> {\n}\n"
        );
    }
    #[test]
    fn expression_precedence_preserves_grouping_and_short_circuiting() {
        use crate::dsl::{BinOp, UnaryOp};
        let subtraction = Expr::from("a").binary(BinOp::Sub, Expr::from("b").binary(BinOp::Sub, "c".into()));
        let comparison = Expr::from("a").binary(BinOp::Lt, "b".into()).binary(BinOp::Eq, "c".into());
        let conditional = Expr::Conditional {
            condition: Box::new("enabled".into()),
            yes: Box::new(Expr::call("work", vec![])),
            no: Box::new(Expr::number(0)),
        };
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            assert_eq!(emission::expression(&subtraction, lang), "a - (b - c)");
            assert_eq!(
                emission::expression(
                    &Expr::Unary {
                        op: UnaryOp::Neg,
                        value: Box::new(Expr::number(-1))
                    },
                    lang
                ),
                "-(-1)"
            );
            let grouped = emission::expression(&Expr::number(2).binary(BinOp::Mul, conditional.clone()), lang);
            assert!(grouped.starts_with("2 * ("));
        }
        assert_eq!(emission::expression(&comparison, ScorerLang::Python), "(a < b) == c");
        assert_eq!(emission::expression(&comparison, ScorerLang::Typescript), "(a < b) === c");
        assert_eq!(
            emission::expression(
                &Expr::Bool(false).binary(BinOp::And, Expr::call("fail", vec![])),
                ScorerLang::Python
            ),
            "False and fail()"
        );
        assert_eq!(
            emission::expression(
                &Expr::Bool(false).binary(BinOp::And, Expr::call("fail", vec![])),
                ScorerLang::Typescript
            ),
            "false && fail()"
        );
    }

    #[test]
    fn dependency_reads_ignore_literals_and_respect_closure_scopes() {
        use crate::scg::lowering::code::{Dependency, references};
        use std::collections::HashSet;
        let literal = Expr::string("_read(root, \"output\") captured()");
        assert!(literal.references().is_empty());
        let local_read = Expr::from("local")
            .member("output")
            .read(Dependency::Field("local".into(), "output".into()));
        let expression = Expr::Array(vec![local_read, Expr::from("captured"), literal]).lambda(vec![Parameter::new("local")]);
        assert_eq!(
            expression.references(),
            HashSet::from([Dependency::Variable("captured".into())])
        );
        let mut nested = Function::new("nested", vec![Parameter::new("argument").with_default("default_value")]);
        nested.declare("local", "argument").unwrap();
        nested
            .body
            .push(Statement::Return(Expr::Array(vec!["local".into(), "captured".into()])));
        assert_eq!(
            references(&[Statement::Function(nested)]),
            HashSet::from([
                Dependency::Variable("captured".into()),
                Dependency::Variable("default_value".into())
            ])
        );
    }

    #[test]
    fn dependency_order_uses_semantic_reads_and_rejects_cycles() {
        use crate::scg::{
            lowering::code::Dependency,
            planning::{Operation, order},
        };
        use std::collections::HashSet;
        let input = Dependency::Field("root".into(), "input".into());
        let output = Dependency::Field("root".into(), "output".into());
        let operation = |key, reads, value| Operation {
            key,
            reads,
            code: vec![Statement::Evaluate(Expr::string(value))],
        };
        let body = order(
            vec![
                operation(output.clone(), HashSet::from([input.clone()]), "output"),
                operation(input.clone(), HashSet::new(), "input"),
            ],
            "root",
        )
        .unwrap();
        assert!(matches!(&body[0],Statement::Evaluate(Expr::String(value)) if value=="input"));
        assert!(
            order(
                vec![
                    operation(output.clone(), HashSet::from([input.clone()]), "output"),
                    operation(input, HashSet::from([output]), "input")
                ],
                "root"
            )
            .is_err()
        );
    }
}
