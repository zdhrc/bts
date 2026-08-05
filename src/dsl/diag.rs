use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct Diag {
    pub(crate) when: DiagPhase,
    pub(crate) what: String,
    pub(crate) r#where: SrcRange,
}

impl Diag {
    pub(crate) fn render(&self, source_name: &str, src: &str) -> String {
        let (line, col) = line_col(src, self.r#where.start);
        format!("{source_name}:{line}:{col}: {} error: {}", self.when, self.what)
    }
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let line = src[..offset].matches('\n').count() + 1;
    let col = src[..offset].chars().rev().take_while(|&ch| ch != '\n').count() + 1;

    (line, col)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DiagPhase {
    Lexing,
    Parsing,
    Validation,
    Generation,
}

impl fmt::Display for DiagPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Lexing => "lexing",
            Self::Parsing => "parsing",
            Self::Validation => "validation",
            Self::Generation => "generation",
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_one_based_line_and_column_positions() {
        let src = "trace \"a\" {\n    input = }\n}";
        let diag = Diag {
            when: DiagPhase::Parsing,
            what: "expected expression assignment".to_owned(),
            r#where: SrcRange::new(24, 25),
        };

        assert_eq!(
            diag.render("simple.bt", src),
            "simple.bt:2:13: parsing error: expected expression assignment"
        );
    }

    #[test]
    fn renders_the_start_of_source_as_line_one_column_one() {
        let diag = Diag {
            when: DiagPhase::Validation,
            what: "shape declares no traces".to_owned(),
            r#where: SrcRange::new(0, 0),
        };

        assert!(diag.render("<src>", "").starts_with("<src>:1:1: validation error:"));
    }

    #[test]
    fn clamps_offsets_past_the_end_of_source() {
        let diag = Diag {
            when: DiagPhase::Lexing,
            what: "unterminated string".to_owned(),
            r#where: SrcRange::new(99, 99),
        };

        assert!(diag.render("<src>", "x").starts_with("<src>:1:2:"));
    }
}
