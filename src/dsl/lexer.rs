use std::{iter::Peekable, str::CharIndices};

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: TokenRange,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TokenKind {
    Ident(String),

    StrLit(String),
    NumLit(String),

    LBrace,
    RBrace,
    LBrack,
    RBrack,

    Equals,
    Comma,

    Eof,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct Lexer<'src> {
    src: &'src str,
    chars: Peekable<CharIndices<'src>>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self {
            src: src,
            chars: src.char_indices().peekable(),
        }
    }

    pub fn lex(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();

        while let Some((idx, ch)) = self.next() {
            match ch {
                ch if ch.is_whitespace() => {}

                // braces
                '{' => tokens.push(Token {
                    kind: TokenKind::LBrace,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),
                '}' => tokens.push(Token {
                    kind: TokenKind::RBrace,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),
                '[' => tokens.push(Token {
                    kind: TokenKind::LBrack,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),
                ']' => tokens.push(Token {
                    kind: TokenKind::RBrack,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),

                // ident
                ch if ch.is_ascii_alphabetic() => {
                    let start = idx;
                    let mut end = idx + ch.len_utf8();
                    let mut val = String::new();
                    val.push(ch);

                    while let Some((i_idx, i_ch)) = self.peek() {
                        if i_ch.is_ascii_alphanumeric() || i_ch == '_' {
                            val.push(i_ch);
                            end = i_idx + i_ch.len_utf8();
                            self.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(Token {
                        kind: TokenKind::Ident(val),
                        range: TokenRange { start, end },
                    });
                }

                // equals
                '=' => tokens.push(Token {
                    kind: TokenKind::Equals,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),

                // comma
                ',' => tokens.push(Token {
                    kind: TokenKind::Comma,
                    range: TokenRange {
                        start: idx,
                        end: idx + ch.len_utf8(),
                    },
                }),

                // str lit
                '"' => {
                    let start = idx;
                    let mut end = idx + ch.len_utf8();
                    let mut val = String::new();
                    let mut closed = false;
                    while let Some((s_idx, s_ch)) = self.peek() {
                        end = s_idx + s_ch.len_utf8();
                        if s_ch == '"' {
                            closed = true;
                            self.next();
                            break;
                        }
                        val.push(s_ch);
                        self.next();
                    }
                    if !closed {
                        return Err(Error::UnterminatedString { at: start });
                    }
                    tokens.push(Token {
                        kind: TokenKind::StrLit(val),
                        range: TokenRange { start, end },
                    });
                }

                // num lit
                ch if ch.is_ascii_digit() => {
                    let start = idx;
                    let mut end = idx + ch.len_utf8();
                    let mut val = String::new();
                    let mut dec = 0;
                    val.push(ch);

                    while let Some((n_idx, n_ch)) = self.peek() {
                        if n_ch.is_numeric() {
                            val.push(n_ch);
                            end = n_idx + n_ch.len_utf8();
                            self.next();
                        } else if n_ch == '.' {
                            if dec == 1 {
                                return Err(Error::InvalidNumber { at: start });
                            }
                            val.push(n_ch);
                            end = n_idx + n_ch.len_utf8();
                            dec += 1;
                            self.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(Token {
                        kind: TokenKind::NumLit(val),
                        range: TokenRange { start, end },
                    })
                }

                // errors
                _ => return Err(Error::UnknownToken { at: idx, ch }),
            }
        }
        tokens.push(Token {
            kind: TokenKind::Eof,
            range: TokenRange {
                start: self.src.len(),
                end: self.src.len(),
            },
        });
        Ok(tokens)
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        self.chars.peek().copied()
    }
    fn next(&mut self) -> Option<(usize, char)> {
        self.chars.next()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Error {
    UnknownToken { at: usize, ch: char },

    // literals
    UnterminatedString { at: usize },
    InvalidNumber { at: usize },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnknownToken { ch, at } => {
                write!(f, "unknown token '{ch}' at byte {at}")
            }
            Error::UnterminatedString { at } => {
                write!(f, "unterminated string at byte {at}")
            }
            Error::InvalidNumber { at } => {
                write!(f, "invalid number at byte {at}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[test]
fn debug() {
    let src = include_str!("../../tests/fixtures/simple.bt");
    let tokens = Lexer::new(src).lex();
    dbg!(&tokens);
}

#[test]
fn rejects_str_literal_without_termination() {
    let err = Lexer::new("\"foo").lex().unwrap_err();
    assert_eq!(err, Error::UnterminatedString { at: 0 });
}

#[test]
fn rejects_num_literal_with_multiple_dec_points() {
    let err = Lexer::new("5.6.4.3").lex().unwrap_err();
    assert_eq!(err, Error::InvalidNumber { at: 0 });
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
