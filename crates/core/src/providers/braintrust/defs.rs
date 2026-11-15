use bts_lang::semantics::Source;

/// Agent and span schemas supplied explicitly by hosts during compilation.
pub const SOURCE: Source<'static> = Source {
    name: "braintrust.bt",
    text: r#"
# agents describe behavior for mock data and mock code without emitting a span
# contains declares membership, next selects entry and progression
# neither relationship establishes span parenting
# input supplies authored fallback data or caller input, output describes the result
# agent output does not overwrite any emitted span output
# each and repeat keep values scoped to their current occurrence
# context.history.<node_name> contains earlier completed input/output/expected/metadata records
# accumulate targets receive context.arrivals in listed edge order and fail on missing inputs
# neutral node and vars steps carry values and descendants without emitting spans
+(agent::node {
    props: {
        name: {type: "string"},
        description: {type: "string"},
    },
    fields: {
        input: {type: "any", role: "input"},
        output: {type: "any", role: "output"},
        expected: {type: "any", role: "input"},
        metadata: {type: "object", role: "input"},
    },
});

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
        expected: {type: "any", role: "input"},
        error: {type: "any"},
        metadata: {type: "object", role: "input"},
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

# child_of points from an emitted child span to its parent span
# it does not schedule either endpoint or transfer values
# consumers keep the parent open through its children
# endpoint and hierarchy validation still require provider integration
+-[child_of::link {direction: "directed"}]-;

# one scorer expression supports emitted spans and callable scorer functions
# context reads the parent span during emission and caller arguments in a function
+(scorer::span {
    props: {
        lang: {type: "string"},
        file: {type: "string"},
    },
    fields: {
        score: {type: "number"},
        steps: {type: "array"},
        judge: {type: "object"},
        reason: {type: "string"},
    },
});

# datasets and evaluations connect authored cases to modeled behavior
+(dataset::node {props: {name: {type: "string"}}, fields: {description: {type: "string"}, cases: {type: "array", required: true}}});
+(case::node {props: {name: {type: "string"}}, fields: {
    input: {type: "any"}, source: {type: "any"}, traces: {type: "array"},
    expected: {type: "any"}, metadata: {type: "object"}, tags: {type: "array"},
}});
+(experiment::node {props: {name: {type: "string"}}, fields: {
    description: {type: "string"}, dataset: {type: "any", required: true},
    task: {type: "any", required: true}, scorers: {type: "array", required: true}, baseline: {type: "any"},
}});
# facets and automation rules are resources rather than emitted spans
+(facet::node {props: {name: {type: "string"}}, fields: {
    prompt: {type: "string", required: true}, description: {type: "string"},
    no_match_pattern: {type: "string"},
}});
+(automation::node {props: {name: {type: "string"}}, fields: {
    type: {type: "string", required: true}, scorers: {type: "array"}, facets: {type: "array"},
    scope: {type: "string"}, root: {type: "boolean"}, span_names: {type: "array"},
    sampling_rate: {type: "number"}, enabled: {type: "boolean"},
}});
"#,
};
