use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use std::{fmt, iter::Peekable, str::CharIndices};

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
    LParen,
    RParen,

    Comma,
    Dot,
    Ellipsis,
    Equals,
    FatArrow,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AmpAmp,
    PipePipe,
    Bang,
    Question,
    Colon,

    Ident(String),
    String(String),
    Number(String),

    // an interpolated string arrives flat: literal chunks around ${...} holes
    // whose contents lex as ordinary tokens
    TemplateOpen,
    TemplateChunk(String),
    InterpOpen,
    InterpClose,
    TemplateClose,

    Eof,
}

const TRIPLE_QUOTE: &str = r#"""""#;

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
            self.lex_token(idx, ch, &mut tokens, &mut errors);
        }
        tokens.push(Token::new(TokenKind::Eof, self.src.len(), self.src.len()));

        if errors.is_empty() { Ok(tokens) } else { Err(errors) }
    }

    // lexes the token starting at an already-consumed char; strings append
    // several tokens when interpolations flatten them
    fn lex_token(&mut self, idx: usize, ch: char, tokens: &mut Tokens, errors: &mut Errors) {
        match ch {
            ch if ch.is_whitespace() => {}
            // parenbraceckets
            '{' => tokens.push(Token::new(TokenKind::LBrace, idx, idx + ch.len_utf8())),
            '}' => tokens.push(Token::new(TokenKind::RBrace, idx, idx + ch.len_utf8())),
            '[' => tokens.push(Token::new(TokenKind::LBrack, idx, idx + ch.len_utf8())),
            ']' => tokens.push(Token::new(TokenKind::RBrack, idx, idx + ch.len_utf8())),
            '(' => tokens.push(Token::new(TokenKind::LParen, idx, idx + ch.len_utf8())),
            ')' => tokens.push(Token::new(TokenKind::RParen, idx, idx + ch.len_utf8())),

            // punctuation
            ',' => tokens.push(Token::new(TokenKind::Comma, idx, idx + ch.len_utf8())),
            // ... spreads, a lone .. stays two dots so diagnostics point at real tokens
            '.' => {
                if matches!(self.peek(), Some((_, '.'))) {
                    let (second, _) = self.next().expect("peeked character is present");
                    if matches!(self.peek(), Some((_, '.'))) {
                        let (third, _) = self.next().expect("peeked character is present");
                        tokens.push(Token::new(TokenKind::Ellipsis, idx, third + '.'.len_utf8()));
                    } else {
                        tokens.push(Token::new(TokenKind::Dot, idx, idx + ch.len_utf8()));
                        tokens.push(Token::new(TokenKind::Dot, second, second + '.'.len_utf8()));
                    }
                } else {
                    tokens.push(Token::new(TokenKind::Dot, idx, idx + ch.len_utf8()));
                }
            }

            // operators, = peeks for == and so on
            '+' => tokens.push(Token::new(TokenKind::Plus, idx, idx + ch.len_utf8())),
            '-' => tokens.push(Token::new(TokenKind::Minus, idx, idx + ch.len_utf8())),
            '*' => tokens.push(Token::new(TokenKind::Star, idx, idx + ch.len_utf8())),
            '/' => {
                if matches!(self.peek(), Some((_, '/'))) {
                    self.skip_comment();
                } else {
                    tokens.push(Token::new(TokenKind::Slash, idx, idx + ch.len_utf8()));
                }
            }
            '#' => self.skip_comment(),
            '%' => tokens.push(Token::new(TokenKind::Percent, idx, idx + ch.len_utf8())),
            '?' => tokens.push(Token::new(TokenKind::Question, idx, idx + ch.len_utf8())),
            ':' => tokens.push(Token::new(TokenKind::Colon, idx, idx + ch.len_utf8())),
            '=' => match self.lex_paired(ch) {
                Some(end) => tokens.push(Token::new(TokenKind::EqEq, idx, end)),
                None => match self.lex_trailing('>') {
                    Some(end) => tokens.push(Token::new(TokenKind::FatArrow, idx, end)),
                    None => tokens.push(Token::new(TokenKind::Equals, idx, idx + ch.len_utf8())),
                },
            },
            '!' => match self.lex_trailing('=') {
                Some(end) => tokens.push(Token::new(TokenKind::NotEq, idx, end)),
                None => tokens.push(Token::new(TokenKind::Bang, idx, idx + ch.len_utf8())),
            },
            '<' => match self.heredoc_intro(idx) {
                Some((delimiter, dedent, intro_end)) => {
                    // the first < is consumed, eat the rest of the introducer
                    while matches!(self.peek(), Some((p_idx, _)) if p_idx < intro_end) {
                        self.next();
                    }
                    self.lex_heredoc(idx, intro_end, delimiter, dedent, tokens, errors);
                }
                None => match self.lex_trailing('=') {
                    Some(end) => tokens.push(Token::new(TokenKind::LtEq, idx, end)),
                    None => tokens.push(Token::new(TokenKind::Lt, idx, idx + ch.len_utf8())),
                },
            },
            '>' => match self.lex_trailing('=') {
                Some(end) => tokens.push(Token::new(TokenKind::GtEq, idx, end)),
                None => tokens.push(Token::new(TokenKind::Gt, idx, idx + ch.len_utf8())),
            },

            // & and | only exist doubled
            '&' => match self.lex_paired(ch) {
                Some(end) => tokens.push(Token::new(TokenKind::AmpAmp, idx, end)),
                None => {
                    errors.push(Error::new(
                        ErrorKind::UnpairedOperator(ch),
                        SrcRange::new(idx, idx + ch.len_utf8()),
                    ));
                }
            },
            '|' => match self.lex_paired(ch) {
                Some(end) => tokens.push(Token::new(TokenKind::PipePipe, idx, end)),
                None => {
                    errors.push(Error::new(
                        ErrorKind::UnpairedOperator(ch),
                        SrcRange::new(idx, idx + ch.len_utf8()),
                    ));
                }
            },

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
                        '{' | '}' | '[' | ']' | '(' | ')' | ',' | '.' | '=' | '"' | '-' | '+' | '*' | '/' | '%' | '<'
                        | '>' | '!' | '&' | '|' | '?' | ':' | '#' => break,
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

            // strings:
            // - a plain string stays a single String token
            // - ${...} interpolations flatten it into a template token run
            // - """ opens a multi-line string
            '"' => {
                if self.src[idx..].starts_with(TRIPLE_QUOTE) {
                    self.next();
                    self.next();
                    self.lex_multiline_string(idx, tokens, errors);
                } else {
                    self.lex_string(idx, tokens, errors);
                }
            }

            // numbers
            ch if ch.is_ascii_digit() => {
                let token = self.lex_number(idx, ch, errors);
                tokens.push(token);
            }

            // errors
            _ => {
                errors.push(Error::new(ErrorKind::UnknownToken, SrcRange::new(idx, idx + ch.len_utf8())));
            }
        }
    }

    // comments run to the end of the line, the newline stays for the whitespace skip
    fn skip_comment(&mut self) {
        while matches!(self.peek(), Some((_, ch)) if ch != '\n') {
            self.next();
        }
    }

    fn lex_string(&mut self, start: usize, tokens: &mut Tokens, errors: &mut Errors) {
        let mut end = start + '"'.len_utf8();
        let mut pieces = StringPieces::default();

        loop {
            match self.next() {
                Some((s_idx, '"')) => {
                    end = s_idx + '"'.len_utf8();
                    break;
                }
                Some((s_idx, '$')) => {
                    end = s_idx + '$'.len_utf8();
                    match self.peek() {
                        // $${ escapes a literal ${, any other $ stays literal
                        Some((_, '$')) => {
                            let (d_idx, _) = self.next().expect("peeked character is present");
                            end = d_idx + '$'.len_utf8();
                            if matches!(self.peek(), Some((_, '{'))) {
                                let (b_idx, _) = self.next().expect("peeked character is present");
                                end = b_idx + '{'.len_utf8();
                                pieces.push_str(s_idx, "${", end);
                            } else {
                                pieces.push_str(s_idx, "$$", end);
                            }
                        }
                        Some((_, '{')) => {
                            self.next();
                            pieces.flush();
                            end = self.lex_interpolation(s_idx, None, &mut pieces.tokens, errors);
                        }
                        _ => pieces.push(s_idx, '$'),
                    }
                }
                Some((s_idx, c)) => {
                    pieces.push(s_idx, c);
                    end = s_idx + c.len_utf8();
                }
                None => {
                    errors.push(Error::new(ErrorKind::UnterminatedString, SrcRange::new(start, end)));
                    break;
                }
            }
        }

        let open = SrcRange::new(start, start + '"'.len_utf8());
        pieces.emit(open, SrcRange::new(end - '"'.len_utf8(), end), tokens);
    }

    // <<DELIM or <<-DELIM directly before an identifier opens a heredoc
    fn heredoc_intro(&self, at: usize) -> Option<(&'src str, bool, usize)> {
        let rest = self.src[at..].strip_prefix("<<")?;
        let (dedent, word) = match rest.strip_prefix('-') {
            Some(word) => (true, word),
            None => (false, rest),
        };
        let len = word.chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').count();
        let delimiter = &word[..len];

        if !delimiter.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
            return None;
        }

        let intro_end = at + "<<".len() + usize::from(dedent) + delimiter.len();
        Some((delimiter, dedent, intro_end))
    }

    // the closing delimiter must sit alone on its line; its leading whitespace is
    // stripped from every content line, and the newline before it is dropped
    fn lex_multiline_string(&mut self, start: usize, tokens: &mut Tokens, errors: &mut Errors) {
        let content_from = start + TRIPLE_QUOTE.len();
        let Some(closing) = self.src[content_from..].find(TRIPLE_QUOTE).map(|found| content_from + found) else {
            errors.push(Error::new(
                ErrorKind::UnterminatedString,
                SrcRange::new(start, self.src.len()),
            ));
            while self.next().is_some() {}
            tokens.push(Token::new(TokenKind::String(String::new()), start, self.src.len()));
            return;
        };
        let end = closing + TRIPLE_QUOTE.len();

        let closing_line = self.src[..closing]
            .rfind('\n')
            .map(|newline| newline + '\n'.len_utf8())
            .filter(|line| *line >= content_from);
        let indent = match closing_line {
            Some(line) if self.src[line..closing].chars().all(char::is_whitespace) => &self.src[line..closing],
            Some(_) => {
                errors.push(Error::new(
                    ErrorKind::ContentBeforeMultilineCloser,
                    SrcRange::new(closing, end),
                ));
                ""
            }
            // both delimiters share a line
            None => {
                errors.push(Error::new(
                    ErrorKind::ContentAfterMultilineOpener,
                    SrcRange::new(content_from, closing),
                ));
                ""
            }
        };

        let at_line_start = if closing_line.is_some() {
            self.scan_opener_line(errors)
        } else {
            true
        };
        let mut pieces = StringPieces::default();
        self.scan_multiline(closing, indent, true, at_line_start, &mut pieces, errors);

        // consume the closing quotes; the newline before their line is not part of the value
        self.next();
        self.next();
        self.next();
        pieces.pop_newline();

        pieces.emit(SrcRange::new(start, content_from), SrcRange::new(closing, end), tokens);
    }

    // content runs from the next line to the first line holding only the delimiter
    // word; << keeps lines verbatim, <<- strips their longest shared indentation,
    // and either way the newline ending the last content line stays in the value
    fn lex_heredoc(&mut self, start: usize, intro_end: usize, delimiter: &str, dedent: bool, tokens: &mut Tokens, errors: &mut Errors) {
        let unterminated = |lexer: &mut Self, errors: &mut Errors| {
            errors.push(Error::new(
                ErrorKind::UnterminatedString,
                SrcRange::new(start, lexer.src.len()),
            ));
            while lexer.next().is_some() {}
            Token::new(TokenKind::String(String::new()), start, lexer.src.len())
        };

        let Some(content_from) = self.src[intro_end..]
            .find('\n')
            .map(|newline| intro_end + newline + '\n'.len_utf8())
        else {
            tokens.push(unterminated(self, errors));
            return;
        };

        // find the closer line first so positions drive the scan
        let mut closer = None;
        let mut line_start = content_from;
        loop {
            let line_end = self.src[line_start..]
                .find('\n')
                .map_or(self.src.len(), |newline| line_start + newline);
            let line = &self.src[line_start..line_end];
            if line.trim() == delimiter {
                let leading = line.len() - line.trim_start().len();
                closer = Some((line_start, line_start + leading + delimiter.len()));
                break;
            }
            if line_end == self.src.len() {
                break;
            }
            line_start = line_end + '\n'.len_utf8();
        }
        let Some((closer_start, end)) = closer else {
            tokens.push(unterminated(self, errors));
            return;
        };

        let indent = if dedent {
            common_indent(&self.src[content_from..closer_start])
        } else {
            ""
        };

        let at_line_start = self.scan_opener_line(errors);
        let mut pieces = StringPieces::default();
        self.scan_multiline(closer_start, indent, false, at_line_start, &mut pieces, errors);

        // consume the closer line through the delimiter word
        while matches!(self.peek(), Some((p_idx, _)) if p_idx < end) {
            self.next();
        }

        pieces.emit(SrcRange::new(start, intro_end), SrcRange::new(closer_start, end), tokens);
    }

    // only whitespace may follow a multi-line opener on its line; content found
    // there is lexed as-is, mid-line, so it doesn't also trip the indent check
    fn scan_opener_line(&mut self, errors: &mut Errors) -> bool {
        loop {
            match self.peek() {
                Some((_, '\n')) => {
                    self.next();
                    return true;
                }
                Some((p_idx, p_ch)) if !p_ch.is_whitespace() => {
                    errors.push(Error::new(
                        ErrorKind::ContentAfterMultilineOpener,
                        SrcRange::new(p_idx, p_idx + p_ch.len_utf8()),
                    ));
                    return false;
                }
                Some(_) => {
                    self.next();
                }
                None => return true,
            }
        }
    }

    // drives multi-line content up to the closing delimiter's line at stop,
    // stripping indent at line starts; strict rejects underindented lines
    fn scan_multiline(
        &mut self,
        stop: usize,
        indent: &str,
        strict: bool,
        mut at_line_start: bool,
        pieces: &mut StringPieces,
        errors: &mut Errors,
    ) {
        loop {
            if matches!(self.peek(), Some((p_idx, _)) if p_idx == stop) {
                break;
            }

            if at_line_start {
                at_line_start = false;

                // blank lines and the closing line keep no indentation
                let blank = {
                    let mut ahead = self.chars.clone();
                    loop {
                        match ahead.next() {
                            Some((a_idx, _)) if a_idx == stop => break true,
                            Some((_, '\n')) => break true,
                            Some((_, a_ch)) if a_ch.is_whitespace() => {}
                            _ => break false,
                        }
                    }
                };

                if blank {
                    while matches!(self.peek(), Some((p_idx, p_ch)) if p_idx != stop && p_ch != '\n' && p_ch.is_whitespace()) {
                        self.next();
                    }
                } else {
                    for want in indent.chars() {
                        match self.peek() {
                            Some((_, got)) if got == want => {
                                self.next();
                            }
                            Some((p_idx, p_ch)) => {
                                if strict {
                                    errors.push(Error::new(
                                        ErrorKind::UnderindentedMultilineLine,
                                        SrcRange::new(p_idx, p_idx + p_ch.len_utf8()),
                                    ));
                                }
                                break;
                            }
                            None => break,
                        }
                    }
                }
                continue;
            }

            match self.next() {
                Some((n_idx, '\n')) => {
                    pieces.push(n_idx, '\n');
                    at_line_start = true;
                }
                // \r\n collapses to \n so dedent and trimming see one newline shape
                Some((_, '\r')) if matches!(self.peek(), Some((_, '\n'))) => {}
                Some((s_idx, '$')) => match self.peek() {
                    // $${ escapes a literal ${, any other $ stays literal
                    Some((_, '$')) => {
                        let (d_idx, _) = self.next().expect("peeked character is present");
                        if matches!(self.peek(), Some((_, '{'))) {
                            let (b_idx, _) = self.next().expect("peeked character is present");
                            pieces.push_str(s_idx, "${", b_idx + '{'.len_utf8());
                        } else {
                            pieces.push_str(s_idx, "$$", d_idx + '$'.len_utf8());
                        }
                    }
                    Some((_, '{')) => {
                        self.next();
                        pieces.flush();
                        self.lex_interpolation(s_idx, Some(stop), &mut pieces.tokens, errors);
                    }
                    _ => pieces.push(s_idx, '$'),
                },
                Some((c_idx, ch)) => pieces.push(c_idx, ch),
                None => break,
            }
        }
    }

    // scans a ${...} hole, open = idx of $ and ${ is already eaten; contents lex
    // as ordinary tokens between InterpOpen and InterpClose markers, strings
    // included (quoted segments like llm["Chat Completion"]); multi-line holes
    // still end on a newline or the delimiter at stop
    fn lex_interpolation(&mut self, open: usize, stop: Option<usize>, tokens: &mut Tokens, errors: &mut Errors) -> usize {
        tokens.push(Token::new(TokenKind::InterpOpen, open, open + "${".len()));
        let mut end = open + "${".len();
        let mut depth = 0usize;

        loop {
            match self.peek() {
                Some((i_idx, '}')) if depth == 0 => {
                    self.next();
                    end = i_idx + '}'.len_utf8();
                    tokens.push(Token::new(TokenKind::InterpClose, i_idx, end));
                    return end;
                }
                Some((i_idx, _)) if Some(i_idx) == stop => break,
                Some((_, '\n')) if stop.is_some() => break,
                None => break,
                Some((i_idx, i_ch)) => {
                    self.next();
                    // nested braces stay ordinary tokens, only the balancing } closes the hole
                    match i_ch {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    self.lex_token(i_idx, i_ch, tokens, errors);
                    end = tokens.last().map_or(end, |token| token.range.end);
                }
            }
        }

        errors.push(Error::new(ErrorKind::UnterminatedInterpolation, SrcRange::new(open, end)));
        end
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

    // consumes the trailing char of a two-char operator, returns its end
    fn lex_trailing(&mut self, trailing: char) -> Option<usize> {
        match self.peek() {
            Some((idx, ch)) if ch == trailing => {
                self.next();
                Some(idx + ch.len_utf8())
            }
            _ => None,
        }
    }

    fn lex_paired(&mut self, ch: char) -> Option<usize> {
        self.lex_trailing(ch)
    }
}

// accumulates one string's scanned pieces; without a hole they collapse back
// into a single String token, with one they emit flat as TemplateOpen, chunk
// and hole tokens, TemplateClose
#[derive(Debug, Default)]
struct StringPieces {
    tokens: Tokens,
    lit: String,
    lit_from: usize,
    lit_to: usize,
}

impl StringPieces {
    fn push(&mut self, idx: usize, ch: char) {
        if self.lit.is_empty() {
            self.lit_from = idx;
        }
        self.lit.push(ch);
        self.lit_to = idx + ch.len_utf8();
    }

    fn push_str(&mut self, from: usize, text: &str, to: usize) {
        if self.lit.is_empty() {
            self.lit_from = from;
        }
        self.lit.push_str(text);
        self.lit_to = to;
    }

    // the newline before a """ closer's line is not part of the value
    fn pop_newline(&mut self) {
        if self.lit.ends_with('\n') {
            self.lit.pop();
            self.lit_to -= '\n'.len_utf8();
        }
    }

    fn flush(&mut self) {
        if !self.lit.is_empty() {
            let chunk = TokenKind::TemplateChunk(std::mem::take(&mut self.lit));
            self.tokens.push(Token::new(chunk, self.lit_from, self.lit_to));
        }
    }

    // a hole always pushed InterpOpen, so any buffered token means a template
    fn emit(mut self, open: SrcRange, close: SrcRange, tokens: &mut Tokens) {
        if self.tokens.is_empty() {
            tokens.push(Token::new(TokenKind::String(self.lit), open.start, close.end));
        } else {
            self.flush();
            tokens.push(Token { kind: TokenKind::TemplateOpen, range: open });
            tokens.append(&mut self.tokens);
            tokens.push(Token { kind: TokenKind::TemplateClose, range: close });
        }
    }
}

// the longest whitespace prefix shared by the non-blank lines
fn common_indent(content: &str) -> &str {
    let mut indent: Option<&str> = None;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let leading = &line[..line.len() - line.trim_start().len()];
        indent = Some(match indent {
            None => leading,
            Some(shared) => {
                let mut end = 0;
                for (a, b) in shared.chars().zip(leading.chars()) {
                    if a != b {
                        break;
                    }
                    end += a.len_utf8();
                }
                &shared[..end]
            }
        });
    }

    indent.unwrap_or("")
}

pub(super) fn lex(src: &str) -> Result<Tokens, Diags> {
    Lexer::new(src)
        .lex()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ErrorKind {
    UnknownToken,
    InvalidIdentToken,
    UnterminatedString,
    ContentAfterMultilineOpener,
    ContentBeforeMultilineCloser,
    UnderindentedMultilineLine,
    UnterminatedInterpolation,
    InvalidNumber,
    UnpairedOperator(char),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownToken => formatter.write_str("unknown token"),
            Self::InvalidIdentToken => formatter.write_str("invalid ident token"),
            Self::UnterminatedString => formatter.write_str("unterminated string"),
            Self::ContentAfterMultilineOpener => {
                formatter.write_str("multi-line string content must start on the line after the opening \"\"\"")
            }
            Self::ContentBeforeMultilineCloser => {
                formatter.write_str("the closing \"\"\" of a multi-line string must sit on its own line")
            }
            Self::UnderindentedMultilineLine => formatter.write_str("line is indented less than the closing \"\"\""),
            Self::UnterminatedInterpolation => formatter.write_str("unterminated interpolation"),
            Self::InvalidNumber => formatter.write_str("invalid number"),
            Self::UnpairedOperator(op) => write!(formatter, "stray `{op}`; did you mean `{op}{op}`?"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

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
    }

    #[test]
    fn lexes_minus_as_an_operator_token() {
        for src in ["-5", "- 5"] {
            let tokens = Lexer::new(src).lex().unwrap();
            let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

            assert_eq!(kinds, [TokenKind::Minus, TokenKind::Number("5".to_owned()), TokenKind::Eof]);
        }

        let tokens = Lexer::new("-x").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Minus);
        assert_eq!(tokens[1].kind, TokenKind::Ident("x".to_owned()));
    }

    #[test]
    fn lexes_single_char_operators_with_ranges() {
        let tokens = Lexer::new("+ - * / % ? : ! < >").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::Question,
                TokenKind::Colon,
                TokenKind::Bang,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[0].range, SrcRange::new(0, 1));
        assert_eq!(tokens[9].range, SrcRange::new(18, 19));
    }

    #[test]
    fn lexes_two_char_operators_over_their_full_range() {
        let tokens = Lexer::new("== != <= >= && ||").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::EqEq,
                TokenKind::NotEq,
                TokenKind::LtEq,
                TokenKind::GtEq,
                TokenKind::AmpAmp,
                TokenKind::PipePipe,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[0].range, SrcRange::new(0, 2));
        assert_eq!(tokens[5].range, SrcRange::new(15, 17));
    }

    #[test]
    fn lexes_ellipses_and_keeps_shorter_dot_runs_as_dots() {
        let tokens = Lexer::new("...x").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Ellipsis);
        assert_eq!(tokens[0].range, SrcRange::new(0, 3));
        assert_eq!(tokens[1].kind, TokenKind::Ident("x".to_owned()));

        let tokens = Lexer::new("a..b").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            [
                TokenKind::Ident("a".to_owned()),
                TokenKind::Dot,
                TokenKind::Dot,
                TokenKind::Ident("b".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_fat_arrows_apart_from_equals() {
        let tokens = Lexer::new("a => b = c == d").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("a".to_owned()),
                TokenKind::FatArrow,
                TokenKind::Ident("b".to_owned()),
                TokenKind::Equals,
                TokenKind::Ident("c".to_owned()),
                TokenKind::EqEq,
                TokenKind::Ident("d".to_owned()),
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[1].range, SrcRange::new(2, 4));
    }

    #[test]
    fn distinguishes_adjacent_operators_without_whitespace() {
        let tokens = Lexer::new("a==b a=b !x x!=y").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("a".to_owned()),
                TokenKind::EqEq,
                TokenKind::Ident("b".to_owned()),
                TokenKind::Ident("a".to_owned()),
                TokenKind::Equals,
                TokenKind::Ident("b".to_owned()),
                TokenKind::Bang,
                TokenKind::Ident("x".to_owned()),
                TokenKind::Ident("x".to_owned()),
                TokenKind::NotEq,
                TokenKind::Ident("y".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn breaks_idents_at_operator_characters() {
        let tokens = Lexer::new("foo+bar cond?x:y").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("foo".to_owned()),
                TokenKind::Plus,
                TokenKind::Ident("bar".to_owned()),
                TokenKind::Ident("cond".to_owned()),
                TokenKind::Question,
                TokenKind::Ident("x".to_owned()),
                TokenKind::Colon,
                TokenKind::Ident("y".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn rejects_unpaired_logical_operators() {
        assert_error_kinds("a & b", &[ErrorKind::UnpairedOperator('&')]);
        assert_error_kinds("a | b", &[ErrorKind::UnpairedOperator('|')]);

        let errors = Lexer::new("a & b").lex().unwrap_err();
        assert_eq!(errors[0].range(), SrcRange::new(2, 3));
    }

    #[test]
    fn lexes_interpolation_contents_as_ordinary_tokens() {
        let tokens = Lexer::new(r#""${a + b}""#).lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        // the parser rejects non-path holes, the lexer just hands it tokens
        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::InterpOpen,
                TokenKind::Ident("a".to_owned()),
                TokenKind::Plus,
                TokenKind::Ident("b".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
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
    fn lexes_parens_and_breaks_idents_before_them() {
        let tokens = Lexer::new("choice(1)").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("choice".to_owned()),
                TokenKind::LParen,
                TokenKind::Number("1".to_owned()),
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_interpolated_strings_into_template_tokens() {
        let tokens = Lexer::new(r#""a ${trace.index} b""#).lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::TemplateChunk("a ".to_owned()),
                TokenKind::InterpOpen,
                TokenKind::Ident("trace".to_owned()),
                TokenKind::Dot,
                TokenKind::Ident("index".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateChunk(" b".to_owned()),
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[0].range, SrcRange::new(0, 1));
        assert_eq!(tokens[2].range, SrcRange::new(3, 5));
        assert_eq!(tokens[6].range, SrcRange::new(16, 17));
        assert_eq!(tokens[8].range, SrcRange::new(19, 20));
    }

    #[test]
    fn lexes_a_lone_interpolation_and_tolerates_padding() {
        let tokens = Lexer::new(r#""${ index }""#).lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::InterpOpen,
                TokenKind::Ident("index".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn keeps_dollar_signs_and_escapes_literal() {
        let tokens = Lexer::new(r#""a$b $$c $${d}""#).lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::String("a$b $$c ${d}".to_owned()));
    }

    #[test]
    fn rejects_unterminated_interpolations() {
        // a quote inside a hole lexes as an inner string, so a malformed hole
        // reports every layer it breaks
        assert_error_kinds(
            r#""${trace""#,
            &[
                ErrorKind::UnterminatedString,
                ErrorKind::UnterminatedInterpolation,
                ErrorKind::UnterminatedString,
            ],
        );
        assert_error_kinds(
            r#""${trace"#,
            &[ErrorKind::UnterminatedInterpolation, ErrorKind::UnterminatedString],
        );
    }

    #[test]
    fn lexes_quoted_segments_inside_interpolations() {
        let tokens = Lexer::new(r#""x ${llm["Chat Completion"].output}""#).lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::TemplateChunk("x ".to_owned()),
                TokenKind::InterpOpen,
                TokenKind::Ident("llm".to_owned()),
                TokenKind::LBrack,
                TokenKind::String("Chat Completion".to_owned()),
                TokenKind::RBrack,
                TokenKind::Dot,
                TokenKind::Ident("output".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_comments_to_the_end_of_the_line() {
        let tokens = Lexer::new("a = 1 # trailing\n// a full line\nb = 2 // another")
            .lex()
            .unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("a".to_owned()),
                TokenKind::Equals,
                TokenKind::Number("1".to_owned()),
                TokenKind::Ident("b".to_owned()),
                TokenKind::Equals,
                TokenKind::Number("2".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn keeps_division_and_string_contents_apart_from_comments() {
        let tokens = Lexer::new("1 / 2").lex().unwrap();
        assert_eq!(tokens[1].kind, TokenKind::Slash);

        let tokens = Lexer::new(r##""a # b // c""##).lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::String("a # b // c".to_owned()));
    }

    #[test]
    fn breaks_idents_at_comment_markers() {
        let tokens = Lexer::new("foo# comment\nbar//comment").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("foo".to_owned()),
                TokenKind::Ident("bar".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_multiline_strings_with_stripped_indentation() {
        let src = "\"\"\"\n    a\n      b\n\n    c\n    \"\"\"";
        let tokens = Lexer::new(src).lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::String("a\n  b\n\nc".to_owned()));
        assert_eq!(tokens[0].range, SrcRange::new(0, src.len()));
    }

    #[test]
    fn keeps_multiline_trailing_newlines_only_before_a_blank_line() {
        let tokens = Lexer::new("\"\"\"\n  a\n\n  \"\"\"").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::String("a\n".to_owned()));

        let tokens = Lexer::new("\"\"\"\n  a\n  \"\"\"").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::String("a".to_owned()));

        let tokens = Lexer::new("\"\"\"\n\"\"\"").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::String(String::new()));
    }

    #[test]
    fn keeps_quotes_and_escapes_literal_in_multiline_strings() {
        let tokens = Lexer::new("\"\"\"\n\"quoted\" and \"\" and $${d}\n\"\"\"").lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::String("\"quoted\" and \"\" and ${d}".to_owned()));
    }

    #[test]
    fn lexes_multiline_interpolations_into_template_tokens() {
        let tokens = Lexer::new("\"\"\"\n  q ${trace.index}\n  a\n  \"\"\"").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::TemplateChunk("q ".to_owned()),
                TokenKind::InterpOpen,
                TokenKind::Ident("trace".to_owned()),
                TokenKind::Dot,
                TokenKind::Ident("index".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateChunk("\na".to_owned()),
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[2].range, SrcRange::new(8, 10));
        assert_eq!(tokens[6].range, SrcRange::new(21, 22));
    }

    #[test]
    fn rejects_multiline_content_beside_the_delimiters() {
        assert_error_kinds("\"\"\"x\n\"\"\"", &[ErrorKind::ContentAfterMultilineOpener]);
        assert_error_kinds("\"\"\"\nx \"\"\"", &[ErrorKind::ContentBeforeMultilineCloser]);
        assert_error_kinds("\"\"\" \"\"\"", &[ErrorKind::ContentAfterMultilineOpener]);
    }

    #[test]
    fn rejects_underindented_multiline_lines() {
        let src = "\"\"\"\n  a\n b\n  \"\"\"";
        assert_error_kinds(src, &[ErrorKind::UnderindentedMultilineLine]);

        let errors = Lexer::new(src).lex().unwrap_err();
        assert_eq!(errors[0].range(), SrcRange::new(9, 10));
    }

    #[test]
    fn rejects_multiline_strings_without_termination() {
        assert_error_kinds("\"\"\"\nfoo", &[ErrorKind::UnterminatedString]);
    }

    #[test]
    fn rejects_unterminated_multiline_interpolations_at_the_line_end() {
        assert_error_kinds("\"\"\"\n${trace\nrest\n\"\"\"", &[ErrorKind::UnterminatedInterpolation]);
    }

    #[test]
    fn lexes_verbatim_heredocs_keeping_indentation_and_the_final_newline() {
        let src = "<<EOT\n  a\n b\nEOT";
        let tokens = Lexer::new(src).lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::String("  a\n b\n".to_owned()));
        assert_eq!(tokens[0].range, SrcRange::new(0, src.len()));
    }

    #[test]
    fn lexes_dedent_heredocs_stripping_the_shared_indentation() {
        let src = "<<-EOT\n    a\n      b\n\n    c\n    EOT";
        let tokens = Lexer::new(src).lex().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::String("a\n  b\n\nc\n".to_owned()));

        // an unindented line caps the stripping instead of erroring
        let tokens = Lexer::new("<<-EOT\n    a\nb\nEOT").lex().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::String("    a\nb\n".to_owned()));
    }

    #[test]
    fn lexes_heredoc_interpolations_into_template_tokens() {
        let tokens = Lexer::new("<<EOT\nq ${trace.index}\nEOT").lex().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|token| token.kind.clone()).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::TemplateOpen,
                TokenKind::TemplateChunk("q ".to_owned()),
                TokenKind::InterpOpen,
                TokenKind::Ident("trace".to_owned()),
                TokenKind::Dot,
                TokenKind::Ident("index".to_owned()),
                TokenKind::InterpClose,
                TokenKind::TemplateChunk("\n".to_owned()),
                TokenKind::TemplateClose,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[2].range, SrcRange::new(8, 10));
    }

    #[test]
    fn closes_heredocs_only_on_exact_delimiter_lines() {
        let tokens = Lexer::new("<<EOT\nEOTX\n EOT \nx").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::String("EOTX\n".to_owned()),
                TokenKind::Ident("x".to_owned()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn keeps_left_angles_without_a_delimiter_word_as_comparisons() {
        let tokens = Lexer::new("a << b").lex().unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();

        assert_eq!(
            kinds,
            [
                TokenKind::Ident("a".to_owned()),
                TokenKind::Lt,
                TokenKind::Lt,
                TokenKind::Ident("b".to_owned()),
                TokenKind::Eof,
            ]
        );

        let tokens = Lexer::new("a <= b").lex().unwrap();
        assert_eq!(tokens[1].kind, TokenKind::LtEq);
    }

    #[test]
    fn rejects_heredocs_without_termination() {
        assert_error_kinds("<<EOT\nfoo", &[ErrorKind::UnterminatedString]);
        assert_error_kinds("x = <<EOT", &[ErrorKind::UnterminatedString]);
    }

    #[test]
    fn rejects_heredoc_content_on_the_opener_line() {
        assert_error_kinds("<<EOT x\nfoo\nEOT", &[ErrorKind::ContentAfterMultilineOpener]);
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
