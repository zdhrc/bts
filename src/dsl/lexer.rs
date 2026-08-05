use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use std::{iter::Peekable, str::CharIndices};
use thiserror::Error as Err;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub range: SrcRange,
}

impl Token {
    fn new(kind: TokenKind, start: usize, end: usize) -> Self {
        Self {
            kind,
            range: SrcRange { start, end },
        }
    }
}

pub(super) type Tokens = Vec<Token>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum TokenKind {
    LBrace,
    RBrace,
    LBrack,
    RBrack,

    Comma,
    Dot,
    Equals,

    Ident(String),
    String(String),
    Number(String),

    Eof,
}

#[derive(Debug)]
struct Lexer<'src> {
    src: &'src str,
    chars: Peekable<CharIndices<'src>>,
}

impl<'src> Lexer<'src> {
    fn new(src: &'src str) -> Self {
        Self {
            src,
            chars: src.char_indices().peekable(),
        }
    }

    fn lex(mut self) -> Result<Tokens, Errors> {
        let mut tokens: Tokens = Vec::new();
        let mut errors: Errors = Vec::new();

        while let Some((idx, ch)) = self.next() {
            match ch {
                ch if ch.is_whitespace() => {}
                // parenbraceckets
                '{' => tokens.push(Token::new(TokenKind::LBrace, idx, idx + ch.len_utf8())),
                '}' => tokens.push(Token::new(TokenKind::RBrace, idx, idx + ch.len_utf8())),
                '[' => tokens.push(Token::new(TokenKind::LBrack, idx, idx + ch.len_utf8())),
                ']' => tokens.push(Token::new(TokenKind::RBrack, idx, idx + ch.len_utf8())),

                // punctuation
                ',' => tokens.push(Token::new(TokenKind::Comma, idx, idx + ch.len_utf8())),
                '.' => tokens.push(Token::new(TokenKind::Dot, idx, idx + ch.len_utf8())),
                '=' => tokens.push(Token::new(TokenKind::Equals, idx, idx + ch.len_utf8())),

                // ident:
                // - first char must be alphabetic
                // - ident chars must be alphanumeric or underscores
                // - whitespace or the start of another token breaks
                // - all other chars emit a diag and break
                ch if ch.is_ascii_alphabetic() => {
                    let start = idx;
                    let mut end = idx + ch.len_utf8();
                    let mut value = String::new();
                    value.push(ch);

                    while let Some((i_idx, i_ch)) = self.peek() {
                        match i_ch {
                            c if c.is_ascii_alphanumeric() || c == '_' => {
                                value.push(i_ch);
                                end = i_idx + i_ch.len_utf8();
                                self.next();
                            }
                            c if c.is_whitespace() => break,
                            '{' | '}' | '[' | ']' | ',' | '.' | '=' | '"' | '-' => break,
                            _ => {
                                errors.push(Error::new(
                                    ErrorKind::InvalidIdentToken,
                                    SrcRange::new(i_idx, i_idx + i_ch.len_utf8()),
                                ));
                                self.next();
                                break;
                            }
                        }
                    }
                    tokens.push(Token::new(TokenKind::Ident(value), start, end));
                }

                // strings
                '"' => {
                    let start = idx;
                    let mut end = idx + ch.len_utf8();
                    let mut value = String::new();

                    let terminated = loop {
                        match self.next() {
                            Some((s_idx, '"')) => {
                                end = s_idx + '"'.len_utf8();
                                break true;
                            }
                            Some((s_idx, c)) => {
                                value.push(c);
                                end = s_idx + c.len_utf8();
                            }
                            None => break false,
                        }
                    };

                    if !terminated {
                        errors.push(Error::new(ErrorKind::UnterminatedString, SrcRange::new(start, end)));
                    }

                    tokens.push(Token::new(TokenKind::String(value), start, end));
                }

                // numbers
                ch if ch.is_ascii_digit() => {
                    let token = self.lex_number(idx, ch, &mut errors);
                    tokens.push(token);
                }

                // a minus sign is only valid immediately before a digit
                '-' => match self.peek() {
                    Some((_, next)) if next.is_ascii_digit() => {
                        let token = self.lex_number(idx, ch, &mut errors);
                        tokens.push(token);
                    }
                    _ => {
                        errors.push(Error::new(ErrorKind::UnknownToken, SrcRange::new(idx, idx + ch.len_utf8())));
                    }
                },

                // errors
                _ => {
                    errors.push(Error::new(ErrorKind::UnknownToken, SrcRange::new(idx, idx + ch.len_utf8())));
                }
            }
        }
        tokens.push(Token::new(TokenKind::Eof, self.src.len(), self.src.len()));

        if errors.is_empty() { Ok(tokens) } else { Err(errors) }
    }

    fn lex_number(&mut self, start: usize, first: char, errors: &mut Errors) -> Token {
        let mut end = start + first.len_utf8();
        let mut value = String::new();
        let mut decimals = 0;
        value.push(first);

        while let Some((n_idx, n_ch)) = self.peek() {
            match n_ch {
                c if c.is_ascii_digit() => {
                    value.push(c);
                    end = n_idx + c.len_utf8();
                    self.next();
                }
                '.' => {
                    decimals += 1;
                    value.push(n_ch);
                    end = n_idx + n_ch.len_utf8();
                    self.next();
                }
                _ => break,
            }
        }

        if decimals > 1 || value.ends_with('.') {
            errors.push(Error::new(ErrorKind::InvalidNumber, SrcRange::new(start, end)));
        }

        Token::new(TokenKind::Number(value), start, end)
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        self.chars.peek().copied()
    }
    fn next(&mut self) -> Option<(usize, char)> {
        self.chars.next()
    }
}

pub(super) fn lex(src: &str) -> Result<Tokens, Diags> {
    Lexer::new(src)
        .lex()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
#[error("{kind}")]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
pub(super) enum ErrorKind {
    #[error("unknown token")]
    UnknownToken,
    #[error("invalid ident token")]
    InvalidIdentToken,
    #[error("unterminated string")]
    UnterminatedString,
    #[error("invalid number")]
    InvalidNumber,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
    #[cfg(test)]
    fn kind(&self) -> ErrorKind {
        self.kind
    }
    #[cfg(test)]
    fn range(&self) -> SrcRange {
        self.range
    }
}

impl From<Error> for Diag {
    fn from(error: Error) -> Self {
        let Error { kind, range } = error;

        Diag {
            when: DiagPhase::Lexing,
            what: kind.to_string(),
            r#where: range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug() {
        let src = include_str!("../../tests/fixtures/simple.bt");
        let tokens = Lexer::new(src).lex();
        dbg!(&tokens);
    }

    #[track_caller]
    fn assert_error_kinds(src: &str, want: &[ErrorKind]) {
        let diags = Lexer::new(src).lex().unwrap_err();
        let got: Vec<_> = diags.iter().map(Error::kind).collect();

        assert_eq!(got, want);
    }

    #[test]
    fn rejects_string_without_termination() {
        assert_error_kinds("\"foo", &[ErrorKind::UnterminatedString]);
    }

    #[test]
    fn rejects_numbers_with_multiple_dec_points() {
        assert_error_kinds("5.6.4.3", &[ErrorKind::InvalidNumber]);
        assert_error_kinds("1..", &[ErrorKind::InvalidNumber]);
        assert_error_kinds("-5.6.4", &[ErrorKind::InvalidNumber]);
    }

    #[test]
    fn lexes_negative_numbers_as_single_tokens() {
        let tokens = Lexer::new("-5 -0.25").lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Number("-5".to_owned()));
        assert_eq!(tokens[0].range, SrcRange::new(0, 2));
        assert_eq!(tokens[1].kind, TokenKind::Number("-0.25".to_owned()));
        assert_eq!(tokens[1].range, SrcRange::new(3, 8));
    }

    #[test]
    fn breaks_idents_at_the_start_of_another_token() {
        let tokens = Lexer::new("[true,null]").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::LBrack,
                TokenKind::Ident("true".to_owned()),
                TokenKind::Comma,
                TokenKind::Ident("null".to_owned()),
                TokenKind::RBrack,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn rejects_a_minus_without_an_adjacent_digit() {
        assert_error_kinds("- 5", &[ErrorKind::UnknownToken]);
        assert_error_kinds("-x", &[ErrorKind::UnknownToken]);
        assert_error_kinds("-.5", &[ErrorKind::UnknownToken]);
    }

    #[test]
    fn points_invalid_ident_diagnostics_at_the_offending_character() {
        let errors = Lexer::new("foo$bar = 1").lex().unwrap_err();

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind(), ErrorKind::InvalidIdentToken);
        assert_eq!(errors[0].range(), SrcRange::new(3, 4));
    }
}

// - lexes_empty_input_as_eof
// - lexes_whitespace_as_eof
// - lexes_ident
// - lexes_ident_with_digits
// - lexes_ident_with_underscore
// - lexes_string_literal
// - lexes_empty_string_literal
// - lexes_number_literal
// - lexes_decimal_number_literal
// - lexes_lbrace
// - lexes_rbrace
// - lexes_lbrack
// - lexes_rbrack
// - lexes_equals
// - lexes_comma
// - lexes_simple_attr
// - lexes_simple_block
// - lexes_array
// - lexes_object_attr
// - lexes_fixture
// - tracks_single_char_token_ranges
// - tracks_ident_token_range
// - tracks_string_literal_range
// - tracks_number_literal_range
// - tracks_eof_range
// - rejects_unknown_token
// - rejects_unterminated_string_literal
// - rejects_number_with_multiple_decimal_points
// - stops_number_before_identifier
// - stops_ident_before_punctuation
