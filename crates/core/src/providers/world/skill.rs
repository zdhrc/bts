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

- A `sentence` is a walk whose steps are parts. A `part` is some of the grammar of a sentence, written with the same properties as a `statement` plus `form` (`statement` or `question`), all optional. A part sets only what it is written with, and a later part's keys win. `text` is the sentence, `sentence` is its grammar for functions such as `combine`, and `grammar` is the parts merged. Put a fork wherever there is more than one way to say it.
- A `list` is a walk with one item for each step. `items` is what the steps gave and `text` is the items composed into one string. A `statement` gives its text, so a list of statements is a list of sentences.
- A `policy` is a statement that states a rule. Define the rules an agent works under once, as the steps of a `list`, and read them wherever they are needed.

An agent reads a world by name: `policies.text`, `ask.text`, or the field of a step every walk visits, such as `fee.sentence`. A step behind a fork cannot be read by name, since a walk may not visit it. Read the walk it belongs to. Nothing in a world reads the agent, and no edge joins the two.

Each walk happens once for a trace, and every read in that trace sees the same walk. A draw depends only on the seed, the trace and the fork, so the same seed gives the same world in generated data and in a generated agent. Data generation takes `--seed`. A generated agent that reads a walk takes an optional `seed` in `run`, `run_details` and `run_traced`, and with the seed of a data run it builds the world of that run's first trace. In TypeScript a seed too large for a number is given as a bigint. Other random values, such as `choice` or `express`, are not shared this way.

Use a world graph for what is true of the setting whatever the agent does: rules, facts, and what the people the agent talks to say. Keep what the agent itself does and says on its path. To build something of your own, declare a kind that extends `walk` and computes its fields from `context.steps`, and step kinds that extend `vars` and compute `gives`. `merge` adds up steps that give objects.

"#;
