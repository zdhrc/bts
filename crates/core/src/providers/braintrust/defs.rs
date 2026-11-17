use bts_lang::semantics::Source;

/// Agent and span schemas supplied explicitly by hosts during compilation.
pub const SOURCE: Source<'static> = Source {
    name: "braintrust.bt",
    text: r#"
# agents describe behavior for mock data and mock code without emitting a span
# one next edge enters the first step, and every step it leads to belongs to the agent
# input supplies authored fallback data or caller input, output describes the result
# agent output does not overwrite any emitted span output
# each and repeat keep values scoped to their current occurrence
# context.history.<node_name> contains earlier completed input/output/expected/metadata records
# accumulate targets receive context.arrivals in listed edge order and fail on missing inputs
# neutral node and vars steps carry values and descendants without emitting spans
+(agent:node {
    name: {type: "string"},
    description: {type: "string"},
}) {
    input: {type: "any", role: "input"},
    output: {type: "any", role: "output"},
    expected: {type: "any", role: "input"},
    metadata: {type: "object", role: "input"},
}
    -[:requires {trace_index: {type: "number"}}]->(context);

# provider schemas describe span data, they do not execute or emit spans
# names are optional so anonymous graph nodes remain usable
# value ranges, tag element types, and emission rules need provider validation
# a span takes these fields only, local holds values a shape computes for its own expressions and is never emitted
+(span:node {name: {type: "string"}}) {
    input: {type: "any", role: "input"},
    output: {type: "any", role: "output"},
    expected: {type: "any", role: "input"},
    error: {type: "any"},
    metadata: {type: "object", role: "input"},
    metrics: {type: "object"},
    scores: {type: "object"},
    tags: {type: "array"},
    duration: {type: "number"},
    local: {type: "object"},
}
    -[:requires {trace_index: {type: "number"}}]->(context);

+(tool:node)-[:extends]->(span);
+(task:node)-[:extends]->(span);
# an llm span counts its own tokens, and an object written in metrics is merged over these keys
# a step without input or output counts none for it
+(llm:node) {
    input: {default: null},
    output: {default: null},
    metrics: {value: {
        prompt_tokens: tokens(self.input),
        completion_tokens: tokens(self.output),
        tokens: self.metrics.prompt_tokens + self.metrics.completion_tokens,
    }},
}
    -[:extends]->(span);
+(function:node)-[:extends]->(span);

# calls schedules its target like next and goes down a level
# its target and the steps that follow along next edges are children of its source
# next stays on the level it is on, and consumers keep the parent open through its children
+(calls:edge {direction: "directed"})
    -[:extends]->(next)
    -[:provides {parent: {type: "object"}}]->(context);

# one scorer expression supports emitted spans and callable scorer functions
# context reads the parent span during emission and caller arguments in a function
+(scorer:node {
    lang: {type: "string"},
    file: {type: "string"},
}) {
    score: {type: "number"},
    steps: {type: "array"},
    judge: {type: "object"},
    reason: {type: "string"},
}
    -[:extends]->(span)
    -[:requires {
        input: {type: "any"},
        output: {type: "any"},
        expected: {type: "any"},
        metadata: {type: "object"},
        index: {type: "number"},
        count: {type: "number"},
    }]->(context);

# datasets and evaluations connect authored cases to modeled behavior
+(dataset:node {name: {type: "string"}}) {description: {type: "string"}, cases: {type: "array", required: true}};
+(case:node {name: {type: "string"}}) {
    input: {type: "any"}, source: {type: "any"}, traces: {type: "array"},
    expected: {type: "any"}, metadata: {type: "object"}, tags: {type: "array"},
};
+(experiment:node {name: {type: "string"}}) {
    description: {type: "string"}, dataset: {type: "any", required: true},
    task: {type: "any", required: true}, scorers: {type: "array", required: true}, baseline: {type: "any"},
};
# facets and automation rules are resources rather than emitted spans
+(facet:node {name: {type: "string"}}) {
    prompt: {type: "string", required: true}, description: {type: "string"},
    no_match_pattern: {type: "string"},
};
+(automation:node {name: {type: "string"}}) {
    type: {type: "string", required: true}, scorers: {type: "array"}, facets: {type: "array"},
    scope: {type: "string"}, root: {type: "boolean"}, span_names: {type: "array"},
    sampling_rate: {type: "number"}, enabled: {type: "boolean"},
};
"#,
};
