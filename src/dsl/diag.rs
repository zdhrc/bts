#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Diag {
    pub when: DiagPhase,
    pub what: String,
    pub r#where: SrcRange,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum DiagPhase {
    Lexing,
    Parsing,
    Validation,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct SrcRange {
    pub(super) start: usize,
    pub(super) end: usize,
}

impl SrcRange {
    pub(super) fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

pub(super) type Diags = Vec<Diag>;
