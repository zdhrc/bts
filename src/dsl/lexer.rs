#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: TokenRange,
}

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LexErr {
    UnknownToken { at: usize, ch: char },

    // literals
    UnterminatedString { at: usize },
    InvalidNumber { at: usize },
}

impl std::fmt::Display for LexErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexErr::UnknownToken { ch, at } => {
                write!(f, "unknown token '{ch}' at byte {at}")
            }
            LexErr::UnterminatedString { at } => {
                write!(f, "unterminated string at byte {at}")
            }
            LexErr::InvalidNumber { at } => {
                write!(f, "invalid number at byte {at}")
            }
        }
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexErr> {
    let mut tokens = Vec::<Token>::new();
    let mut chars = src.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
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
            ch if ch.is_alphabetic() => {
                let start = idx;
                let mut end = idx + ch.len_utf8();
                let mut val = String::new();
                val.push(ch);

                while let Some((i_idx, i_ch)) = chars.peek().copied() {
                    if i_ch.is_alphanumeric() || i_ch == '_' {
                        val.push(i_ch);
                        end = i_idx + i_ch.len_utf8();
                        chars.next();
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
                while let Some((s_idx, s_ch)) = chars.peek().copied() {
                    end = s_idx + s_ch.len_utf8();
                    if s_ch == '"' {
                        closed = true;
                        chars.next();
                        break;
                    }
                    val.push(s_ch);
                    chars.next();
                }
                if !closed {
                    return Err(LexErr::UnterminatedString { at: start });
                }
                tokens.push(Token {
                    kind: TokenKind::StrLit(val),
                    range: TokenRange { start, end },
                });
            }

            // num lit
            ch if ch.is_numeric() => {
                let start = idx;
                let mut end = idx + ch.len_utf8();
                let mut val = String::new();
                let mut dec = 0;
                val.push(ch);

                while let Some((n_idx, n_ch)) = chars.peek().copied() {
                    if n_ch.is_numeric() {
                        val.push(n_ch);
                        end = n_idx + n_ch.len_utf8();
                        chars.next();
                    } else if n_ch == '.' {
                        if dec == 1 {
                            return Err(LexErr::InvalidNumber { at: start });
                        }
                        val.push(n_ch);
                        end = n_idx + n_ch.len_utf8();
                        dec += 1;
                        chars.next();
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
            _ => return Err(LexErr::UnknownToken { at: idx, ch }),
        }
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        range: TokenRange {
            start: src.len(),
            end: src.len(),
        },
    });
    Ok(tokens)
}

#[test]
fn debug() {
    let src = include_str!("../../tests/fixtures/simple.bt");
    let tokens = lex(src).unwrap();
    dbg!(&tokens);
}

#[test]
fn rejects_str_literal_without_termination() {
    let err = lex("\"foo").unwrap_err();
    assert_eq!(err, LexErr::UnterminatedString { at: 0 });
}

#[test]
fn rejects_num_literal_with_multiple_dec_points() {
    let err = lex("5.6.4.3").unwrap_err();
    assert_eq!(err, LexErr::InvalidNumber { at: 0 });
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
