use bts_lang::semantics::Source;

/// English as graph and the kinds a walk is built from, supplied explicitly by hosts during compilation.
pub const SOURCE: Source<'static> = Source {
    name: "world.bt",
    text: r#"
# a statement is one English sentence, written as its grammar
# clause is the grammar as an object, for the functions that relate sentences
+(statement:node {
    subject: {type: "string", required: true},
    person: {type: "string", default: "third"},
    number: {type: "string", default: "singular"},
    verb: {type: "string", required: true},
    complement: {type: "any", default: null},
    modal: {type: "any", default: null},
    tense: {type: "string", default: "present"},
}) {
    clause: {type: "object", value: {clause: {
        subject: {text: self.props.subject, person: self.props.person, number: self.props.number},
        predicate: {verb: self.props.verb, complement: self.props.complement},
        modal: self.props.modal,
        tense: self.props.tense,
    }}},
    text: {type: "string", value: realize(self.clause)},
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
# per says what the walk is drawn for: traces that give the same value see the same walk, and left out it is the trace
# speaker is who says the walk and spoken says whether a speaker leads to it, and related is what relations join it to
# a walk that is a step of another gives that walk what its own gives says
+(walk:node {per: {type: "any", default: null}})
    -[:requires {
        steps: {type: "array"},
        trace_index: {type: "number"},
        speaker: {type: "any"},
        spoken: {type: "boolean"},
        styled: {type: "any"},
        related: {type: "object"},
    }]->(context);

# a given is a value written down, for a walk to take as it is
+(given:node {value: {type: "any", required: true}}) {
    gives: {type: "any", value: self.props.value},
};

# an either gives nothing itself, so it is a place to hang a fork, such as inside a repeat
+(either:node) {
    gives: {type: "any", value: null},
};

# a fact is one value in the world, the first its walk gives, so a fork under it makes it vary
+(fact:node) {
    value: {type: "any", value: context.steps[0]},
    gives: {type: "any", value: self.value},
}
    -[:extends]->(walk);

# an entity is something in the world with attributes, which its steps add up to
# related holds, for each relation the entity is part of, the attributes of the others in it
+(entity:node) {
    attributes: {type: "object", value: merge({}, context.steps)},
    related: {type: "object", value: context.related},
    gives: {type: "object", value: self.attributes},
}
    -[:extends]->(walk);

# a setting gathers walks under one name, listed in nodes, and members holds what each of them gives
+(setting:node) {
    members: {type: "object", value: context.members},
}
    -[:extends]->(network)
    -[:requires {members: {type: "object"}}]->(context);

# a persona is someone in the world, and the sentences under it are said their way
+(persona:node) {
    said: {type: "array", value: context.steps},
    gives: {type: "array", value: self.said},
}
    -[:extends]->(walk)
    -[:has]->(speaker);

# a conversation is several traces in a row that belong together, and it stands beside an agent
# turns is how many traces one conversation has, and with up_to each conversation runs for turns to up_to of them
# weights says how likely each of those lengths is, the first for turns and the last for up_to
# number counts the conversations of a run, turn the traces within one, and length is how many turns this one has
# place is where the trace sits, worked out once, and the three read it
# id is the same for every trace of one conversation, and a walk drawn per number is too
+(conversation:node {
    turns: {type: "number", default: 1},
    up_to: {type: "any", default: null},
    weights: {type: "any", default: null},
}) {
    place: {type: "object", value: context.place},
    number: {type: "number", value: self.place.number},
    turn: {type: "number", value: self.place.turn},
    length: {type: "number", value: self.place.length},
    first: {type: "boolean", value: self.turn == 0},
    last: {type: "boolean", value: self.turn == self.length - 1},
    id: {type: "string", value: context.identity},
}
    -[:requires {trace_index: {type: "number"}, place: {type: "object"}, identity: {type: "string"}}]->(context);

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
# defaults is what the sentence starts from before its parts, and an instance writes only the keys it changes
# grammar is the parts merged over the defaults, and clause is that grammar as the object the sentence functions take
+(sentence:node) {
    defaults: {type: "object", value: {
        subject: null, person: "third", number: "singular", verb: null,
        complement: null, modal: null, tense: "present", form: "statement",
    }},
    grammar: {type: "object", value: merge(self.defaults, context.steps)},
    clause: {type: "object", value: {clause: {
        subject: {text: self.grammar.subject, person: self.grammar.person, number: self.grammar.number},
        predicate: {verb: self.grammar.verb, complement: self.grammar.complement},
        modal: self.grammar.modal,
        tense: self.grammar.tense,
        form: self.grammar.form,
    }}},
    # plain is the sentence as its grammar reads, and text is that or the way of the speaker that leads to it
    plain: {type: "string", value: realize(self.clause)},
    text: {type: "string", value: context.spoken ? context.styled : self.plain},
    gives: {type: "string", value: self.text},
}
    -[:extends]->(walk);

# a list is a walk with one item for each step
+(list:node) {
    items: {type: "array", value: context.steps},
    text: {type: "string", value: compose(context.steps)},
    gives: {type: "string", value: self.text},
}
    -[:extends]->(walk);
"#,
};
