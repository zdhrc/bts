//! Shared execution contracts for data generation and code emission.
pub mod code;
pub mod data;

pub use crate::providers::braintrust::{Automation, AutomationKind, Dataset, DatasetCase, DatasetSource, Experiment, Facet};
pub use bts_lang::{BinOp, Diag, DiagPhase, Diags, EnglishOperation, Number, SrcRange, UnaryOp, syntax};
pub use code::contracts::{JudgeOption, Scorer, ScorerArg, ScorerKind, ScorerLang, ScorerStep, When};
pub use data::contracts::{
    Accessor, Agent, Binding, Child, Choice, CtxRef, Field, Flow, FlowEdge, FlowFork, FlowJunction, FlowNode, Maybe, Model,
    NodeId, RefId, Repeat, ResolvedRef, ScorerSpan, Selection, Span, SpanFields, SpanKind, Step, Trace,
};
pub use data::value::expr::*;
