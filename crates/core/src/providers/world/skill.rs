//! What the world provider tells authors.

pub const DEFINITIONS: &str = r#"## World definitions

The world provider represents English as graph. A `statement` is one sentence written as its grammar, and its `text` field is the sentence. An `intent` is something a speaker wants, and its `text` is worded differently on each evaluation. A kind with the `speaker` trait gives the intents beneath it a way of talking. These nodes emit no spans. Place them on the agent's path before the steps that read them. The world functions do the same work inside an expression.

### Building a world beside the agent

A `walk` is something the provider builds by walking a graph. Write it as its own root statement, beside the agent and not on its path. The steps it leads to say how one instance is put together.

- The provider starts at the walk and takes what leaves each node in the order written, along `next` edges. Hang the steps under the walk on aligned lines, so the lines read top to bottom as the order. Reordering the lines changes what is built.
- A `weighted` fork takes one branch by a draw, and an `exclusive` fork takes the branch whose `when` guard holds. A fork needs no junction after it. The walk carries on with the next line. Write a nested fork as its own statement that starts from a named step, since a branch list holds single steps.
- Each visited step hands the walk one value, its `gives` field. The walk reads them in order as `context.steps`, and its kind says what they become.
- A walk emits no span and so do its steps. Junctions, `calls`, `parallel`, `each`, `repeat` and cycles are rejected in a walk, and a walk cannot be a step of another walk.

The walks the provider ships:

- A `sentence` is a walk whose steps are parts. A `part` is some of the grammar of a sentence, written with the same properties as a `statement` plus `form` (`statement` or `question`), all optional. A part sets only what it is written with, and a later part's keys win. `text` is the sentence, `sentence` is its grammar for functions such as `combine`, and `grammar` is the parts merged over `defaults`. Put a fork wherever there is more than one way to say it. To start a sentence from something other than third person, singular, present tense and a statement, write the keys that change in `defaults`, such as `{defaults: {tense: "past"}}`.
- A `list` is a walk with one item for each step. `items` is what the steps gave and `text` is the items composed into one string. A `statement` gives its text, so a list of statements is a list of sentences.
- A `policy` is a statement that states a rule. Define the rules an agent works under once, as the steps of a `list`, and read them wherever they are needed.

An agent reads a world by name: `policies.text`, `ask.text`, or the field of a step every walk visits, such as `fee.sentence`. A step behind a fork cannot be read by name, since a walk may not visit it. Read the walk it belongs to. Nothing in a world reads the agent, and no edge joins the two.

Each walk happens once for a trace, and every read in that trace sees the same walk. To keep a walk the same across traces, say what it is drawn for with `per`, a string or a whole number. Traces that give the same value see the same walk: `{per: "run"}` draws it once for the whole run, and `{per: floor(context.trace_index / 3)}` draws it once for every three traces, such as the turns of one conversation. Without `per` a walk is drawn for each trace.

### Traces that belong together

A `conversation` is several traces in a row that belong together, such as the turns of one chat or the sessions of one customer. Write it beside the agent, as `(chat:conversation {turns: 3})`. It has no steps and emits no span. `turns` is how many traces one conversation has, and left out every trace is its own. With `up_to`, as in `(chat:conversation {turns: 2, up_to: 6})`, each conversation runs for its own number of turns from `turns` to `up_to`, so a run has short conversations and long ones. Both are whole numbers from 1 to 256. The lengths are equally likely unless `weights` gives one weight for each, the first for `turns` and the last for `up_to`: `{turns: 1, up_to: 4, weights: [|8, 4, 2, 1|]}` makes most conversations short.

- `id` is an identifier that is the same for every trace of one conversation and different for the next. Put it wherever the traces should carry it, such as `metadata: {conversation_id: chat.id}`.
- `number` tells the conversations of a run apart and `turn` counts the traces within one from zero. `length` is how many turns this conversation has, and `first` and `last` say whether a trace opens or closes it. Use them to make turns differ, such as a guard on `chat.last` for the step that closes a conversation.
- A walk drawn `per: chat.number` is the same for every trace of the conversation, so the people in it keep their wording and the rules stay the rules.

With `up_to`, a trace finds its place by counting conversations from the start of its block of 256 traces, so the last conversation of each block ends with the block and may be shorter than `turns`. Its `length` is the turns it has.

To end conversations somewhere else, declare a kind that extends `conversation` and states `number`, `turn` and `length` again from `context.trace_index`. A generated agent is one trace. Given `seed` and `trace` it is that trace of the run, so it can be any turn of any conversation, and without `trace` it is the first. A draw depends only on the seed, the trace and the fork, so the same seed gives the same world in generated data and in a generated agent. Data generation takes `--seed`. A generated agent that reads a walk takes an optional `seed` and `trace` in `run`, `run_details` and `run_traced`. With the seed of a data run it builds the world of that run's first trace, and with `trace` as well the world of that trace, counted from zero. In TypeScript a seed too large for a number is given as a bigint. Other random values, such as `choice` or `express`, are not shared this way.

Use a world graph for what is true of the setting whatever the agent does: rules, facts, and what the people the agent talks to say. Keep what the agent itself does and says on its path. The provider decides what its kinds compute, and an extension changes it. Declare a kind that extends `sentence`, `list`, `part` or `policy` and state the field or the default again, such as `+(shout:node) {text: {value: upper(realize(self.sentence))}}` with `-[:extends]->(sentence)` on the next line. A kind or an instance that states an object field again, such as `defaults`, writes only the keys it changes.

To build something of your own, declare a kind that extends `walk` and computes its fields from `context.steps`. A step is a node of any kind that computes `gives`. `merge` adds up steps that give objects.

"#;
