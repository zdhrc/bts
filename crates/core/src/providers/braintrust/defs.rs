use bts_lang::semantics::Source;

/// Span schemas supplied explicitly by hosts during compilation.
pub const SOURCE: Source<'static> = Source {
    name: "braintrust.bt",
    text: r#"
# provider schemas describe span data, they do not execute or emit spans
# names are optional so anonymous graph nodes remain usable
# value ranges, tag element types, and emission rules need provider validation
+(span::node {
    props: {
        name: {type: "string"},
    },
    fields: {
        input: {type: "any", role: "input"},
        output: {type: "any", role: "output"},
        expected: {type: "any"},
        error: {type: "any"},
        metadata: {type: "object"},
        metrics: {type: "object"},
        scores: {type: "object"},
        tags: {type: "array"},
        duration: {type: "number"},
    },
});

+(tool::span {});
+(task::span {});
+(llm::span {});
+(function::span {});

# scorer spans are separate from deployable scorer implementations
+(scorer::span {
    fields: {
        score: {type: "number", required: true},
        reason: {type: "string"},
    },
});
"#,
};
