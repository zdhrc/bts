use bts_lang::semantics::Source;

/// English as graph and the kinds a walk is built from, supplied explicitly by hosts during compilation.
pub const SOURCE: Source<'static> = Source {
    name: "world.bt",
    text: r#"
# a statement is one English sentence, written as its grammar
# sentence is the grammar as an object, for the functions that relate sentences
+(statement:node {
    subject: {type: "string", required: true},
    person: {type: "string", default: "third"},
    number: {type: "string", default: "singular"},
    verb: {type: "string", required: true},
    complement: {type: "any", default: null},
    modal: {type: "any", default: null},
    tense: {type: "string", default: "present"},
}) {
    sentence: {type: "object", value: {clause: {
        subject: {text: self.props.subject, person: self.props.person, number: self.props.number},
        predicate: {verb: self.props.verb, complement: self.props.complement},
        modal: self.props.modal,
        tense: self.props.tense,
    }}},
    text: {type: "string", value: realize(self.sentence)},
    # as a step of a walk, a statement gives its text
    gives: {type: "string", value: self.text},
}
    -[:extends]->(vars);

# a policy is a statement that states a rule
+(policy:node)-[:extends]->(statement);

# a speaker gives everything said beneath it a way of talking
+(speaker:trait {style: {type: "string", default: "conversational"}})
    -[:on]->(node)
    -[:provides {speaker: {type: "object", value: {wording: {style: self.props.style}}}}]->(context);

# an intent is something a speaker wants, said a different way each time
+(intent:node {name: {type: "string", required: true}}) {
    constructions: {type: "array", required: true},
    families: {type: "array", required: true},
    text: {type: "string", value: express(
        {intent: self.props.name, constructions: self.constructions, families: self.families},
        {speaker: context.speaker})},
}
    -[:extends]->(vars)
    -[:requires {speaker: {type: "object"}}]->(context);

# a walk is something built by the steps it leads to, and it stands beside an agent
# the provider visits what leaves each node in the order written, and takes one branch at each fork
# steps is what each visited step gives, in order
+(walk:node)
    -[:requires {steps: {type: "array"}}]->(context);

# a part is some of a sentence's grammar, and gives what it was written with
+(part:node {
    subject: {type: "any", default: null},
    person: {type: "any", default: null},
    number: {type: "any", default: null},
    verb: {type: "any", default: null},
    complement: {type: "any", default: null},
    modal: {type: "any", default: null},
    tense: {type: "any", default: null},
    form: {type: "any", default: null},
}) {
    gives: {type: "object", value: {
        subject: self.props.subject,
        person: self.props.person,
        number: self.props.number,
        verb: self.props.verb,
        complement: self.props.complement,
        modal: self.props.modal,
        tense: self.props.tense,
        form: self.props.form,
    }},
};

# a sentence is a walk whose parts add up to one clause, and a later part's keys win
# grammar is the parts merged, sentence is the clause for the functions that relate sentences
+(sentence:node) {
    grammar: {type: "object", value: merge(
        {subject: null, person: "third", number: "singular", verb: null, complement: null, modal: null, tense: "present", form: "statement"},
        context.steps)},
    sentence: {type: "object", value: {clause: {
        subject: {text: self.grammar.subject, person: self.grammar.person, number: self.grammar.number},
        predicate: {verb: self.grammar.verb, complement: self.grammar.complement},
        modal: self.grammar.modal,
        tense: self.grammar.tense,
        form: self.grammar.form,
    }}},
    text: {type: "string", value: realize(self.sentence)},
}
    -[:extends]->(walk);

# a list is a walk with one item for each step
+(list:node) {
    items: {type: "array", value: context.steps},
    text: {type: "string", value: compose(context.steps)},
}
    -[:extends]->(walk);
"#,
};
