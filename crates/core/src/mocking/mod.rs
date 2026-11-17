//! Shared execution contracts for data generation and code emission.
pub mod code;
pub mod data;
pub mod functions;

pub use crate::providers::braintrust::{Automation, AutomationKind, Dataset, DatasetCase, DatasetSource, Experiment, Facet};
pub use bts_lang::{BinOp, Diag, DiagPhase, Diags, Number, SrcRange, UnaryOp, syntax};
pub use code::contracts::{JudgeOption, Scorer, ScorerArg, ScorerKind, ScorerLang, ScorerStep, When};
pub use data::contracts::{
    Accessor, Again, Agent, Binding, Child, Choice, CtxRef, Field, Maybe, Model, NodeId, RefId, Repeat, ResolvedRef,
    ScorerSpan, Selection, Span, SpanFields, SpanKind, Step, Trace,
};
pub use data::value::expr::*;
pub use functions::{Check, Consumer, Evaluate, EvaluateError, Evaluation, Function, Helper, Runtime};
