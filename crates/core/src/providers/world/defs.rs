use bts_lang::semantics::Source;

/// English as graph, supplied explicitly by hosts during compilation.
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
}
    -[:extends]->(vars);

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
"#,
};
