//! What the world provider tells authors.

pub const DEFINITIONS: &str = r#"## World definitions

The world provider represents English as graph. A `statement` is one sentence written as its grammar, and its `text` field is the sentence. An `intent` is something a speaker wants, and its `text` is worded differently on each evaluation. A kind with the `speaker` trait gives the intents beneath it a way of talking. These nodes emit no spans. Place them on the agent's path before the steps that read them. The world functions do the same work inside an expression.

Write a subject in lowercase unless it is a name. A sentence starts with a capital when its first word is an ordinary lowercase word, so `subject: "refunds"` reads `Refunds return to the card.` as a statement and `Do refunds return to the card?` as a question. A first word with a capital, a digit or other marks is a name or an identifier and stays as written. A `verb` is a word the provider has forms for, or its five forms written out as `{base: "reconcile", third_person_singular: "reconciles", past: "reconciled", past_participle: "reconciled", present_participle: "reconciling"}`. The provider has forms for a few hundred common verbs, and this skill does not list them. To find out whether it has one, write the verb as a word and run `bts check syntax`. It reports a word without forms where it is written and prints the object to fill in. A verb that is computed, such as one read from a `fact`, is looked up when a trace reads the sentence, so a word without forms fails the run there and the error points at the sentence.

### Building a world beside the agent

A `walk` is something the provider builds by walking a graph. Write it as its own root statement, beside the agent and not on its path. The steps it leads to say how one instance is put together.

- The provider starts at the walk and takes what leaves each node in the order written, along `next` edges. Hang the steps under the walk on aligned lines, so the lines read top to bottom as the order. Reordering the lines changes what is built.
- A `weighted` fork takes one branch by a draw, and an `exclusive` fork takes the branch whose `when` guard holds. A fork needs no junction after it. The walk carries on with the next line. Write a nested fork as its own statement that starts from a named step, since a branch list holds single steps.
- Each visited step hands the walk one value, its `gives` field. The walk reads them in order as `context.steps`, and its kind says what they become.
- A `repeat` fork with a whole `count` walks its branches once for each round, and a fork inside a round draws for that round alone. An `either` is a step that gives nothing and holds a fork, so `=|:repeat {count: 3}|=>[-|:next|->(pick:either)]` with a weighted fork under `pick` gives three values, each drawn for itself. A walk is built once, so a walk cannot be what a repeat repeats.
- A walk can be a step of another walk. It gives the outer walk what it built, a sentence its text and an entity its attributes, and the lines indented under it are its own steps. This is how one item of a list varies and is still read by name: make it a `sentence` with a fork and read `fee.text`.
- A walk emits no span and so do its steps. Junctions, `calls`, `parallel`, `each` and cycles are rejected in a walk.

The walks the provider ships:

- A `sentence` is a walk whose steps are parts. A `part` is some of the grammar of a sentence, written with the same properties as a `statement` plus `form` (`statement` or `question`), all optional. A part sets only what it is written with, and a later part's keys win. `text` is the sentence, `clause` is its grammar for functions such as `combine`, and `grammar` is the parts merged over `defaults`. Put a fork wherever there is more than one way to say it. To start a sentence from something other than third person, singular, present tense and a statement, write the keys that change in its `defaults` field. It is a field, so it goes in the body after the node and not among the properties: `(ask:sentence) {defaults: {tense: "past"}}`.
- A `list` is a walk with one item for each step. `items` is what the steps gave and `text` is the items joined into one message by `paragraph`, which keeps a sentence written without its full stop from running into the next. A `statement` gives its text, so a list of statements is a list of sentences.
- A `policy` is a statement that states a rule. Define the rules an agent works under once, as the steps of a `list`, and read them wherever they are needed.
- A `fact` is one value in the world, the first its walk gives. A `given` is a step that gives the value written in `value`. `(fare:fact)` with a weighted fork of `given` steps is a fare that differs from trace to trace, read as `fare.value`.
- An `entity` is something in the world with attributes: a customer, a booking, a flight. Its steps give objects and `attributes` is those objects added up, so one `given` holds what never changes and a fork of `given` steps holds what does. Hand it to the step that returns it, as `output: flight.attributes`, so a tool result and the reply that quotes it agree.
- A `persona` is someone in the world. The sentences that are its steps are said its way, set by `style` as for `speaker`, and `said` is what they come to. Without `style` a persona talks as `conversational`, which contracts `I will` to `I'll`. A sentence no persona leads to is said plainly, and `plain` is always the sentence as its grammar reads. What a way of talking samples, such as typos, is drawn by the seed like a fork, so it is the same in data and in a generated agent. Nothing about this is built into the provider: a sentence reads `context.speaker`, which is null when nothing leads to it with one, and hands `vary` its `context.draws`.

### Entities that belong together

A `relation` node joins the walks it has `incidence` edges to, written `(holds:relation); (holds)-|:incidence|-(traveler); (holds)-|:incidence|-(booking);`. Each entity in it reads the others through `related`, an object with one key for each relation the entity is part of, holding what the other walks give: `traveler.related.holds` is a list with the booking's attributes. Name relations of one entity apart. A relation with a direction is read from the other side: with `{role: "input"}` on the traveler's edge and `{role: "output"}` on the booking's, the traveler reads the outputs and the booking reads the inputs.

A `setting` gathers walks under one name. It is a network, so the walks go in `nodes`: `(trip:setting {nodes: [(traveler), (booking)], edges: []})`. `trip.members` holds what each of them gives, by name, which is a way to hand a step the whole situation at once.

A walk reads another walk by name, wherever a value is written: in a property, as `(customer:persona {style: tone.value})`, in a guard, as `when: plan.attributes.interval == "annual"`, and in what a step gives, as `(:given {value: {amount: plan.attributes.price}})`. This is how one part of a world depends on another.

A property of a `part` or a `statement` takes an expression like any other property, so a `complement` can be `invoice.attributes.id` or `format("invoice {} for {} dollars", invoice.attributes.id, invoice.attributes.amount)`. A subject or a complement is a piece of one sentence, so it holds no comma, question mark, exclamation mark or semicolon and does not end in a full stop. A value that does, such as the default `format_date`, passes `bts check syntax` and fails the run at the sentence. A guard can read a sentence too, such as one a persona says: `when: ask.plain == "I need a refund."` on one branch and the same with `!=` on the other. Read `plain` there, since `text` and `said` change with the way of talking. An exclusive fork needs guards it can tell apart. `==` and `!=` on one value are told apart, and `contains` beside its negation is not. With more than two ways to go, draw a `fact` and guard on its value.

An agent reads a world by name: `policies.text`, `ask.text`, or the field of a step, such as `fee.clause`. A step's own fields say what the step is, whether or not a walk visits it, so read the walk to know what was built. No edge joins the two.

A world reads the agent by name too. It reads the agent's input, as `(:given {value: {name: support.input.name}})` or in a guard such as `when: support.input.tier == "gold"` on a branch of an exclusive fork, which is how the world a caller meets depends on who is calling. It reads what a step returned, as `charge.output.amount`, which is how a receipt in the world quotes the charge the agent made. A step and the part of the world that quotes it cannot read each other in a circle.

Each walk happens once for a trace, and every read in that trace sees the same walk. To keep a walk the same across traces, say what it is drawn for with `per`, a string or a whole number. Traces that give the same value see the same walk: `{per: "run"}` draws it once for the whole run, and `{per: floor(context.trace_index / 3)}` draws it once for every three traces, such as the turns of one conversation. Without `per` a walk is drawn for each trace. A walk that is a step of another and does not state `per` is drawn for what that walk is drawn for, so a fork in a sentence of a persona drawn for a conversation is the same on every turn of it. A walk that is a step of walks not drawn for the same thing has to state `per` itself.

### Traces that belong together

A `series` is several traces in a row that belong together, such as the turns of one conversation, the sessions of one customer or the runs of one day. Write it beside the agent, as `(chat:series {turns: 3})`. It has no steps and emits no span. `turns` is how many traces one series has, and left out every trace is its own. With `up_to`, as in `(chat:series {turns: 2, up_to: 6})`, each series runs for its own number of turns from `turns` to `up_to`, so a run has short series and long ones. Both are whole numbers from 1 to 256. The lengths are equally likely unless `weights` gives one weight for each, the first for `turns` and the last for `up_to`: `{turns: 1, up_to: 4, weights: [8, 4, 2, 1]}` makes most series short.

- `id` is an identifier that is the same for every trace of one series and different for the next. Put it wherever the traces should carry it, such as `metadata: {conversation_id: chat.id}`.
- `number` tells the series of a run apart and `turn` counts the traces within one from zero. `length` is how many turns this series has, and `first` and `last` say whether a trace opens or closes it. Use them to make turns differ, such as a guard on `chat.last` for the step that closes a series.
- A walk drawn `per: chat.number` is the same for every trace of the series, so the people in it keep their wording and the rules stay the rules.
- What a persona says over a series is one list, so a turn reads its own message as `customer.said[chat.turn]` and the messages before it as `customer.said[0:chat.turn]`.
- `conversation` makes the chat messages of a turn for an llm step. `input: conversation({system: policies.text, user: customer.said, assistant: helper.said, turn: chat.turn})` is the system message, every earlier message with its reply, and the message of this turn. The lists can be of any length, which a comprehension cannot take. Without `assistant` it is the user's messages alone, and without `system` it starts with the first of them. With `between`, the tool call and the tool result of an earlier turn sit between that turn's message and its reply. It is a list with one entry for each turn. Write it as a `list` drawn `per` the series whose steps are `given` lists of messages, with an empty list for a turn that called nothing, and pass `between: lookups.items`.
- A trace cannot read another trace, so what the agent replied on an earlier turn has to be in the world. Write the agent's replies as a second persona drawn `per` the series, one sentence for each turn, and return `helper.said[chat.turn]` from the llm step. Every turn then sees the same replies. A reply that quotes something quotes a walk drawn `per` the series, since a value sampled on the agent's path is new on every trace.

With `up_to`, a trace finds its place by counting series from the start of its block of 256 traces, so the last series of each block ends with the block and may be shorter than `turns`. Its `length` is the turns it has.

To end series somewhere else, declare a kind that extends `series` and states `number`, `turn` and `length` again from `context.trace_index`. A generated agent is one trace. Given `seed` and `trace` it is that trace of the run, so it can be any turn of any series, and without `trace` it is the first. A draw depends only on the seed, the trace and the fork, so the same seed gives the same world in generated data and in a generated agent. Data generation takes `--seed`. A generated agent that reads a walk takes an optional `seed` and `trace` in `run`, `run_details` and `run_traced`. With the seed of a data run it builds the world of that run's first trace, and with `trace` as well the world of that trace, counted from zero. In Python they are keywords, as `run_details(seed=7, trace=3)`. In TypeScript they follow the input and the random source in order, as `runDetails(undefined, Math.random, 7, 3)`, where `undefined` takes the authored input, and a seed too large for a number is given as a bigint. An agent that reads no walk takes neither. Python raises a `TypeError` on the keyword and `tsc` rejects the extra arguments, while Node runs the file without checking its types and ignores them.

Only forks and `context.draws` are drawn this way. A function that samples, such as `hex`, `choice` or `express`, is drawn again for every trace, and a generated agent draws its own. In a walk drawn for each trace that is a new value on each trace, which is what such a walk asks for. In a walk that states `per` it would break what `per` promises, so `bts check syntax` rejects it there. For a value that stays the same across a series, write a fork of `given` steps or compute it from `context.draws` in a kind of your own.

Use a world graph for what is true of the setting whatever the agent does: rules, facts, and what the people the agent talks to say. Keep what the agent itself does and says on its path. The provider decides what its kinds compute, and an extension changes it. Declare a kind that extends `sentence`, `list`, `part` or `policy` and state the field or the default again, such as `+(shout:node) {text: {value: upper(realize(self.clause))}}` with `-|:extends|->(sentence)` on the next line. A kind or an instance that states an object field again, such as `defaults`, writes only the keys it changes.

To build something of your own, declare a kind that extends `walk` and computes its fields from `context.steps`. A step is a node of any kind that computes `gives`, and a step whose `gives` is null adds nothing. `merge` adds up steps that give objects. Every walk reads `context.draws`, a list of 32 numbers from zero up to one that are the same in data and in an agent given the seed. Hand them to `vary`, or index into them, when a kind of your own samples. A key a walk's kind requires that nothing leads to it with is null, so a kind asks whether it is there with `context.key == null`. Comparing with null works for a value whose type is known only on evaluation, such as an input, a context key or what a walk gives. A literal, or any value whose type is known before then, is rejected when compared with null.

### A complete world

One shape with each piece in place: a series, a fact that sets what a thread is about, two entities that belong together, rules of which one varies, a persona whose way of writing is drawn once for the series, a second persona for the agent's replies, and an agent that reads them all. The customer's first sentence and the agent's first reply are guarded on the same fact, so the reply answers what was asked, and each turn has its own reply. Steal the structure and replace the content.

```bts
# a thread is one to four traces, one for each message a customer sends
(thread:series {turns: 1, up_to: 4});

# how the customer writes, the same for the whole thread
(tone:fact {per: thread.number})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:given {value: "professional"}),
        -|:next {weight: 2}|->(:given {value: "text_message"}),
    ];

# what the thread is about, which the customer and the agent both speak to
(topic:fact {per: thread.number})
    =|:weighted|=>[
        -|:next {weight: 2}|->(:given {value: "charged_twice"}),
        -|:next {weight: 1}|->(:given {value: "charged_after_canceling"}),
    ];

(plan:entity {per: thread.number})
    =|:weighted|=>[
        -|:next {weight: 3}|->(:given {value: {name: "Team", interval: "monthly", price: 49}}),
        -|:next {weight: 1}|->(:given {value: {name: "Team", interval: "annual", price: 490}}),
    ];

# the invoice reads the plan it bills
(invoice:entity {per: thread.number})
    -|:next|->(:given {value: {amount: plan.attributes.price}})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:given {value: {id: "INV-20418", issued: "2026-10-02"}}),
        -|:next {weight: 1}|->(:given {value: {id: "INV-20733", issued: "2026-10-17"}}),
    ];

(billed:relation);
(billed)-|:incidence|-(plan);
(billed)-|:incidence|-(invoice);

# two rules are fixed and the refund window follows the plan
(policies:list {per: thread.number})
    -|:next|->(:policy {subject: "agents", number: "plural", modal: "must", verb: "check", complement: "the invoice first"})
    -|:next|->(refund:sentence)
        -|:next|->(:part {subject: "customers", number: "plural", modal: "can", verb: "request"})
        =|:exclusive|=>[
            -|:next {when: plan.attributes.interval == "annual"}|->(:part {complement: "a refund within 30 days"}),
            -|:next {when: plan.attributes.interval == "monthly"}|->(:part {complement: "a refund within 14 days"}),
        ]
    -|:next|->(:policy {subject: "refunds", number: "plural", verb: "return", complement: "to the original card"});

# what the customer says, one sentence for each turn of the thread
(customer:persona {style: tone.value, per: thread.number})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "you", person: "second", verb: "charge", tense: "past"})
        =|:exclusive|=>[
            -|:next {when: topic.value == "charged_twice"}|->(:part {complement: "me twice this month"}),
            -|:next {when: topic.value == "charged_after_canceling"}|->(:part {complement: "me after I canceled"}),
        ]
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "the invoice number", verb: "be", complement: invoice.attributes.id})
    -|:next|->(:sentence) {defaults: {form: "question", modal: "can"}}
        -|:next|->(:part {subject: "you", person: "second", verb: "refund", complement: "the charge"})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "that", verb: "work", complement: "for me"});

# what the agent replies, one sentence for each turn, on the same topic
(helper:persona {per: thread.number})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "I", person: "first", modal: "can", verb: "look"})
        =|:exclusive|=>[
            -|:next {when: topic.value == "charged_twice"}|->(:part {complement: "into the second charge"}),
            -|:next {when: topic.value == "charged_after_canceling"}|->(:part {complement: "into the charge after you canceled"}),
        ]
    -|:next|->(:sentence)
        -|:next|->(:part {
            subject: "I",
            person: "first",
            verb: "find",
            tense: "past",
            complement: format("invoice {} for {} dollars", invoice.attributes.id, invoice.attributes.amount),
        })
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "I", person: "first", modal: "will", verb: "refund", complement: "the charge to the original card"})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "you", person: "second", verb: "be", complement: "welcome"});

(support:agent) {
    input: {message: customer.said[thread.turn], earlier: customer.said[0:thread.turn]},
    output: handle.output,
};
(support)-|:next|->(handle:task {name: "handle_message"}) {
    input: support.input,
    output: reply.output.content,
    metadata: {thread_id: thread.id, turn: thread.turn, topic: topic.value},
}
    -|:calls|->(lookup:tool {name: "lookup_invoice"}) {
        input: {message: handle.input.message},
        output: {invoice: invoice.attributes, plan: invoice.related.billed[0]},
    }
        -|:next|->(reply:llm {name: "chat_completion"}) {
            # the rules, every earlier turn with its reply, and the message of this turn
            input: conversation({system: policies.text, user: customer.said, assistant: helper.said, turn: thread.turn}),
            output: {role: "assistant", content: helper.said[thread.turn]},
        };
```

"#;
