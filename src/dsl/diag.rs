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

        format!(
            "{source_name}:{line}:{col}: {} error: {}\n{}",
            self.when,
            self.what,
            snippet(src, self.r#where, line, col)
        )
    }
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let line = src[..offset].matches('\n').count() + 1;
    let col = src[..offset].chars().rev().take_while(|&ch| ch != '\n').count() + 1;

    (line, col)
}

// the offending line with a caret underline covering the range, clamped to the
// line the range starts on
fn snippet(src: &str, range: SrcRange, line: usize, col: usize) -> String {
    let start = range.start.min(src.len());
    let line_start = src[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = src[start..].find('\n').map_or(src.len(), |index| start + index);
    let text = &src[line_start..line_end];
    let gutter = " ".repeat(line.to_string().len());
    // pad with the line's own tabs so the caret stays aligned at any tab width
    let pad: String = text
        .chars()
        .take(col - 1)
        .map(|ch| if ch == '\t' { '\t' } else { ' ' })
        .collect();
    let width = src[start..range.end.clamp(start, line_end)].chars().count().max(1);

    format!(
        "{gutter} |\n{line} | {text}\n{gutter} | {pad}{carets} (byte {start})",
        carets = "^".repeat(width),
    )
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
            what: "expected expression".to_owned(),
            r#where: SrcRange::new(24, 25),
        };

        assert_eq!(
            diag.render("simple.bt", src),
            "simple.bt:2:13: parsing error: expected expression\n  \
             |\n\
             2 |     input = }\n  \
             |             ^ (byte 24)"
        );
    }

    #[test]
    fn underlines_the_full_range() {
        let src = "trace \"a\" {}";
        let diag = Diag {
            when: DiagPhase::Validation,
            what: "shape declares no traces".to_owned(),
            r#where: SrcRange::new(6, 9),
        };

        assert!(
            diag.render("<src>", src)
                .ends_with("1 | trace \"a\" {}\n  |       ^^^ (byte 6)")
        );
    }

    #[test]
    fn clamps_the_underline_to_the_starting_line() {
        let src = "input = \"\"\"\nnever closed";
        let diag = Diag {
            when: DiagPhase::Lexing,
            what: "unterminated string".to_owned(),
            r#where: SrcRange::new(8, src.len()),
        };

        assert!(
            diag.render("<src>", src)
                .ends_with("1 | input = \"\"\"\n  |         ^^^ (byte 8)")
        );
    }

    #[test]
    fn pads_the_underline_with_tabs_to_stay_aligned() {
        let src = "\ttags = [}]";
        let diag = Diag {
            when: DiagPhase::Parsing,
            what: "unknown token".to_owned(),
            r#where: SrcRange::new(9, 10),
        };

        assert!(
            diag.render("<src>", src)
                .ends_with("1 | \ttags = [}]\n  | \t        ^ (byte 9)")
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
