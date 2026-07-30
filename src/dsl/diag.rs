#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Diag {
    pub when: Phase,
    pub what: String,
    pub r#where: Range,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum Phase {
    Lexing,
    Parsing,
    Validation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Range {
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) type Diags = Vec<Diag>;
