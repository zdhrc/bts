//! What the world provider tells authors.

pub const DEFINITIONS: &str = r#"## World definitions

The world provider represents English as graph. A `statement` is one sentence written as its grammar, and its `text` field is the sentence. An `intent` is something a speaker wants, and its `text` is worded differently on each evaluation. A kind with the `speaker` trait gives the intents beneath it a way of talking. These nodes emit no spans. Place them on the agent's path before the steps that read them. The world functions do the same work inside an expression.

"#;
