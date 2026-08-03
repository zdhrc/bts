#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct Diag {
    pub(crate) when: DiagPhase,
    pub(crate) what: String,
    pub(crate) r#where: SrcRange,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DiagPhase {
    Lexing,
    Parsing,
    Validation,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct SrcRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl SrcRange {
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

pub(crate) type Diags = Vec<Diag>;
