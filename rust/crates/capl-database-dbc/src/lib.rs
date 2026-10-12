//! Parser for Vector CAN database (DBC) files.
//!
//! The lexer deliberately borrows the input. Tokens contain slices into the
//! source and the recursive-descent parser allocates only for the public model.

use capl_database_core::{ByteOrder, Message, NetworkDatabase, Signal};
use std::{fmt, iter::Peekable, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbcError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for DbcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for DbcError {}

/// A token borrows its spelling from the input DBC document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind<'a> {
    Atom(&'a str),
    String(&'a str),
    Symbol(char),
    Newline,
    Invalid(&'a str),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
    pub line: usize,
    pub column: usize,
}

/// Zero-copy DBC lexer. Newlines are emitted for line-oriented directives.
pub struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
    emitted_eof: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            column: 1,
            emitted_eof: false,
        }
    }

    fn token(&self, kind: TokenKind<'a>, line: usize, column: usize) -> Token<'a> {
        Token { kind, line, column }
    }

    fn advance_char(&mut self) -> Option<char> {
        let ch = self.source[self.offset..].chars().next()?;
        self.offset += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn is_delimiter(ch: char) -> bool {
        matches!(
            ch,
            ':' | ';' | '|' | '@' | '+' | '-' | ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"'
        )
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.offset >= self.source.len() {
                if self.emitted_eof {
                    return None;
                }
                self.emitted_eof = true;
                return Some(self.token(TokenKind::Eof, self.line, self.column));
            }
            let start = self.offset;
            let line = self.line;
            let column = self.column;
            let ch = self.source[self.offset..].chars().next().unwrap();
            if ch == '\n' {
                self.advance_char();
                return Some(self.token(TokenKind::Newline, line, column));
            }
            if ch.is_whitespace() {
                self.advance_char();
                continue;
            }
            if self.source[self.offset..].starts_with("//") {
                while self.offset < self.source.len()
                    && !self.source[self.offset..].starts_with('\n')
                {
                    self.advance_char();
                }
                continue;
            }
            if self.source[self.offset..].starts_with("/*") {
                self.advance_char();
                self.advance_char();
                let mut closed = false;
                while self.offset < self.source.len() {
                    if self.source[self.offset..].starts_with("*/") {
                        self.advance_char();
                        self.advance_char();
                        closed = true;
                        break;
                    }
                    self.advance_char();
                }
                if !closed {
                    return Some(self.token(
                        TokenKind::Invalid(&self.source[start..self.offset]),
                        line,
                        column,
                    ));
                }
                continue;
            }
            if ch == '"' {
                self.advance_char();
                let content_start = self.offset;
                let mut escaped = false;
                while self.offset < self.source.len() {
                    let current = self.source[self.offset..].chars().next().unwrap();
                    if current == '\n' && !escaped {
                        return Some(self.token(
                            TokenKind::Invalid(&self.source[start..self.offset]),
                            line,
                            column,
                        ));
                    }
                    self.advance_char();
                    if current == '"' && !escaped {
                        let end = self.offset - 1;
                        return Some(self.token(
                            TokenKind::String(&self.source[content_start..end]),
                            line,
                            column,
                        ));
                    }
                    escaped = current == '\\' && !escaped;
                    if current != '\\' {
                        escaped = false;
                    }
                }
                return Some(self.token(
                    TokenKind::Invalid(&self.source[start..self.offset]),
                    line,
                    column,
                ));
            }
            if Self::is_delimiter(ch) {
                self.advance_char();
                return Some(self.token(TokenKind::Symbol(ch), line, column));
            }
            self.advance_char();
            while self.offset < self.source.len() {
                let current = self.source[self.offset..].chars().next().unwrap();
                let previous = self.source[start..self.offset].chars().next_back();
                let next = self.source[self.offset + current.len_utf8()..]
                    .chars()
                    .next();
                let exponent_sign = matches!(previous, Some('e' | 'E'))
                    && matches!(current, '+' | '-')
                    && next.is_some_and(|ch| ch.is_ascii_digit());
                if current.is_whitespace()
                    || (Self::is_delimiter(current) && !exponent_sign)
                    || self.source[self.offset..].starts_with("//")
                    || self.source[self.offset..].starts_with("/*")
                {
                    break;
                }
                self.advance_char();
            }
            return Some(self.token(
                TokenKind::Atom(&self.source[start..self.offset]),
                line,
                column,
            ));
        }
    }
}

pub fn parse(input: &str) -> Result<NetworkDatabase, DbcError> {
    Parser::new(input).parse()
}

struct Parser<'a> {
    tokens: Peekable<Lexer<'a>>,
    database: NetworkDatabase,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            tokens: Lexer::new(input).peekable(),
            database: NetworkDatabase::default(),
        }
    }

    fn parse(mut self) -> Result<NetworkDatabase, DbcError> {
        loop {
            self.skip_separators();
            let token = *self.tokens.peek().expect("lexer always emits EOF");
            match token.kind {
                TokenKind::Eof => return Ok(self.database),
                TokenKind::Invalid(_) => return Err(self.error(token, "invalid token")),
                TokenKind::Atom("BU_") => self.parse_nodes()?,
                TokenKind::Atom("BO_") => self.parse_message()?,
                TokenKind::Atom("SG_") => self.parse_signal()?,
                TokenKind::Atom("VERSION") => self.parse_version()?,
                TokenKind::Atom("NS_") | TokenKind::Atom("BS_") => self.skip_line(),
                _ => self.skip_statement(),
            }
        }
    }

    fn peek(&mut self) -> Token<'a> {
        *self.tokens.peek().expect("lexer always emits EOF")
    }
    fn bump(&mut self) -> Token<'a> {
        self.tokens.next().expect("lexer always emits EOF")
    }
    fn skip_separators(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Symbol(';')
        ) {
            self.bump();
        }
    }
    fn skip_line(&mut self) {
        while !matches!(self.peek().kind, TokenKind::Newline | TokenKind::Eof) {
            self.bump();
        }
    }
    fn skip_statement(&mut self) {
        // Most metadata declarations end in `;`; header lists such as
        // `NS_DESC_` are line-oriented and have no terminator.
        while !matches!(
            self.peek().kind,
            TokenKind::Symbol(';') | TokenKind::Newline | TokenKind::Eof
        ) {
            self.bump();
        }
        if matches!(
            self.peek().kind,
            TokenKind::Symbol(';') | TokenKind::Newline
        ) {
            self.bump();
        }
    }

    fn parse_version(&mut self) -> Result<(), DbcError> {
        let keyword = self.bump();
        match self.bump().kind {
            TokenKind::String(_) => Ok(()),
            _ => Err(self.error(keyword, "VERSION expects a quoted string")),
        }
    }

    fn parse_nodes(&mut self) -> Result<(), DbcError> {
        let keyword = self.bump();
        self.expect_symbol(':', keyword)?;
        while !matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Eof | TokenKind::Symbol(';')
        ) {
            let token = self.bump();
            let name = self.expect_atom(token, "node name")?;
            if name != "Vector__XXX" && !self.database.nodes.iter().any(|n| n == name) {
                self.database.nodes.push(name.to_owned());
            }
        }
        Ok(())
    }

    fn parse_message(&mut self) -> Result<(), DbcError> {
        let keyword = self.bump();
        let id_token = self.bump();
        let id = self.parse_u32(id_token, "message id")?;
        let name_token = self.bump();
        let name = self.expect_atom(name_token, "message name")?.to_owned();
        self.expect_symbol(':', keyword)?;
        let length_token = self.bump();
        let length = self.parse_u8(length_token, "message length")?;
        let transmitter = if matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Eof | TokenKind::Symbol(';')
        ) {
            None
        } else {
            let transmitter_token = self.bump();
            Some(
                self.expect_atom(transmitter_token, "transmitter")?
                    .to_owned(),
            )
        };
        self.database.messages.push(Message {
            id,
            name,
            length,
            transmitter,
            signals: Vec::new(),
        });
        Ok(())
    }

    fn parse_signal(&mut self) -> Result<(), DbcError> {
        let keyword = self.bump();
        let name_token = self.bump();
        let name = self.expect_atom(name_token, "signal name")?.to_owned();
        if self.database.messages.is_empty() {
            return Err(self.error(keyword, "SG_ appears before BO_"));
        }
        if !matches!(self.peek().kind, TokenKind::Symbol(':')) {
            let marker = self.bump();
            self.expect_atom(marker, "multiplexer marker")?;
        }
        self.expect_symbol(':', keyword)?;
        let start_token = self.bump();
        let start_bit = self.parse_u16(start_token, "signal start bit")?;
        self.expect_symbol('|', keyword)?;
        let length_token = self.bump();
        let length = self.parse_u16(length_token, "signal length")?;
        self.expect_symbol('@', keyword)?;
        let order_token = self.bump();
        let order = self.parse_u8(order_token, "signal byte order")?;
        let byte_order = match order {
            0 => ByteOrder::BigEndian,
            1 => ByteOrder::LittleEndian,
            _ => return Err(self.error(keyword, "byte order must be 0 or 1")),
        };
        let signed = match self.bump().kind {
            TokenKind::Symbol('+') => false,
            TokenKind::Symbol('-') => true,
            _ => return Err(self.error(keyword, "signal sign must be '+' or '-'")),
        };
        let (factor, offset) = if matches!(self.peek().kind, TokenKind::Symbol('(')) {
            self.bump();
            let factor = self.parse_f64_signed("signal factor")?;
            self.expect_symbol(',', keyword)?;
            let offset = self.parse_f64_signed("signal offset")?;
            self.expect_symbol(')', keyword)?;
            (factor, offset)
        } else {
            (1.0, 0.0)
        };
        if matches!(self.peek().kind, TokenKind::Symbol('[')) {
            self.bump();
            self.parse_f64_signed("signal minimum")?;
            self.expect_symbol('|', keyword)?;
            self.parse_f64_signed("signal maximum")?;
            self.expect_symbol(']', keyword)?;
        }
        let unit = if matches!(self.peek().kind, TokenKind::String(_)) {
            let unit_token = self.bump();
            Some(unescape(self.expect_string(unit_token, "signal unit")?))
        } else {
            None
        };
        while !matches!(
            self.peek().kind,
            TokenKind::Symbol(';') | TokenKind::Newline | TokenKind::Eof
        ) {
            self.bump();
        }
        self.database
            .messages
            .last_mut()
            .unwrap()
            .signals
            .push(Signal {
                name,
                start_bit,
                length,
                byte_order,
                signed,
                factor,
                offset,
                unit,
            });
        Ok(())
    }

    fn parse_f64_signed(&mut self, what: &str) -> Result<f64, DbcError> {
        let negative = matches!(self.peek().kind, TokenKind::Symbol('-'));
        if negative || matches!(self.peek().kind, TokenKind::Symbol('+')) {
            self.bump();
        }
        let token = self.bump();
        let text = self.expect_atom(token, what)?;
        let value =
            f64::from_str(text).map_err(|_| self.error(token, format!("invalid {what}")))?;
        Ok(if negative { -value } else { value })
    }
    fn parse_u32(&mut self, token: Token<'a>, what: &str) -> Result<u32, DbcError> {
        let text = self.expect_atom(token, what)?;
        text.parse()
            .map_err(|_| self.error(token, format!("invalid {what}")))
    }
    fn parse_u16(&mut self, token: Token<'a>, what: &str) -> Result<u16, DbcError> {
        let text = self.expect_atom(token, what)?;
        text.parse()
            .map_err(|_| self.error(token, format!("invalid {what}")))
    }
    fn parse_u8(&mut self, token: Token<'a>, what: &str) -> Result<u8, DbcError> {
        let text = self.expect_atom(token, what)?;
        text.parse()
            .map_err(|_| self.error(token, format!("invalid {what}")))
    }
    fn expect_atom(&self, token: Token<'a>, what: &str) -> Result<&'a str, DbcError> {
        match token.kind {
            TokenKind::Atom(text) => Ok(text),
            _ => Err(self.error(token, format!("expected {what}"))),
        }
    }
    fn expect_string(&self, token: Token<'a>, what: &str) -> Result<&'a str, DbcError> {
        match token.kind {
            TokenKind::String(text) => Ok(text),
            _ => Err(self.error(token, format!("expected {what}"))),
        }
    }
    fn expect_symbol(&mut self, symbol: char, context: Token<'a>) -> Result<(), DbcError> {
        match self.bump().kind {
            TokenKind::Symbol(actual) if actual == symbol => Ok(()),
            _ => Err(self.error(context, format!("expected '{symbol}'"))),
        }
    }
    fn error(&self, token: Token<'a>, message: impl Into<String>) -> DbcError {
        DbcError {
            line: token.line,
            message: message.into(),
        }
    }
}

fn unescape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => output.push('"'),
                Some('\\') => output.push('\\'),
                Some('n') => output.push('\n'),
                Some('r') => output.push('\r'),
                Some('t') => output.push('\t'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_borrows_spelling_and_tracks_lines() {
        let source = "BO_ 1 Msg: 8 ECU\n SG_ Speed : 0|16@1+ (0.1,-10)\n";
        let tokens: Vec<_> = Lexer::new(source).collect();
        assert!(matches!(tokens[0].kind, TokenKind::Atom("BO_")));
        assert!(matches!(tokens[1].kind, TokenKind::Atom("1")));
        assert_eq!(tokens[0].line, 1);
        assert!(tokens.iter().any(|token| token.line == 2));
    }

    #[test]
    fn parses_messages_and_signals() {
        let db = parse("VERSION \"1.0\"\nBU_: ECU1 ECU2 Vector__XXX\nBO_ 100 Msg: 8 ECU1\n SG_ Speed : 0|16@1+ (0.1,-10) [0|100] \"km/h\" ECU2;\n").unwrap();
        assert_eq!(db.nodes, ["ECU1", "ECU2"]);
        assert_eq!(db.messages[0].id, 100);
        assert_eq!(db.messages[0].signals[0].factor, 0.1);
        assert_eq!(db.messages[0].signals[0].offset, -10.0);
        assert_eq!(db.messages[0].signals[0].unit.as_deref(), Some("km/h"));
    }

    #[test]
    fn accepts_multiplexed_and_multiline_input() {
        let db = parse("BU_: ECU\nBO_ 1 M: 8 ECU\nSG_ Mode M : 0|4@1+ (1,0) [0|15] \"\" ECU;\nSG_ Value m2 : 8|8@0- (0.5, -1) ECU;\n").unwrap();
        assert_eq!(db.messages[0].signals.len(), 2);
        assert_eq!(db.messages[0].signals[1].byte_order, ByteOrder::BigEndian);
        assert!(db.messages[0].signals[1].signed);
    }

    #[test]
    fn keeps_exponent_signs_inside_numeric_tokens() {
        let db =
            parse("BO_ 1 Exponents: 8 ECU\nSG_ Gain : 0|8@1+ (1e-3,+2E+1) [0|1e+2] \"\" ECU;\n")
                .unwrap();
        assert_eq!(db.messages[0].signals[0].factor, 1e-3);
        assert_eq!(db.messages[0].signals[0].offset, 20.0);
    }

    #[test]
    fn skips_standard_header_lists_without_swallowing_messages() {
        let db = parse(
            "VERSION \"8.0\"\nNS_ :\n  NS_DESC_\n  CM_\nBS_:\nBU_: ECU\nBO_ 7 HeaderSafe: 1 ECU\n",
        )
        .unwrap();
        assert_eq!(db.messages[0].name, "HeaderSafe");
    }

    #[test]
    fn reports_structural_errors() {
        let error = parse("SG_ Broken : 0|8@1+ (1,0)\n").unwrap_err();
        assert!(error.message.contains("before BO_"));
        let error = parse("BO_ nope Msg: 8 ECU\n").unwrap_err();
        assert!(error.message.contains("message id"));
    }
}
