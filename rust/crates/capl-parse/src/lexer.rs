use capl_diag::{DiagBag, Loc, Stage};

pub const KEYWORDS: &[&str] = &[
    "variables",
    "includes",
    "on",
    "start",
    "preStart",
    "stopMeasurement",
    "preStop",
    "timer",
    "msTimer",
    "message",
    "key",
    "sysvar",
    "envVar",
    "errorFrame",
    "busOff",
    "signal",
    "this",
    "if",
    "else",
    "while",
    "for",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "return",
    "const",
    "enum",
    "struct",
    "export",
    "static",
    "testcase",
    "testfunction",
];

pub const TYPE_KEYWORDS: &[&str] = &[
    "int", "long", "int64", "byte", "word", "dword", "qword", "float", "double", "char", "void",
    "timer", "msTimer", "message", "struct", "enum",
];

const PUNCTUATORS: &[&str] = &[
    ">>=", "<<=", "...", "++", "--", "->", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "::", "+", "-", "*", "/", "%", "=", "<", ">", "!",
    "~", "&", "|", "^", "?", ":", ";", ",", ".", "(", ")", "{", "}", "[", "]",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Ident,
    Keyword,
    Int,
    Float,
    Char,
    Str,
    Punct,
    Comment,
    Preproc,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokKind,
    pub text: String,
    pub int: i64,
    pub float: f64,
    pub string: String,
    pub loc: Loc,
}

impl Token {
    pub fn is_kw(&self, kw: &str) -> bool {
        self.kind == TokKind::Keyword && self.text == kw
    }
    pub fn is_punct(&self, p: &str) -> bool {
        self.kind == TokKind::Punct && self.text == p
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LexOptions {
    /// keep comments and `#...` lines in the token stream
    pub keep_trivia: bool,
}

pub struct Lexer<'a, 'd> {
    src: &'a [u8],
    raw: &'a str,
    i: usize,
    line: u32,
    line_start: usize,
    diags: &'d mut DiagBag,
    opts: LexOptions,
}

impl<'a, 'd> Lexer<'a, 'd> {
    pub fn new(src: &'a str, diags: &'d mut DiagBag) -> Self {
        Self::with_options(src, diags, LexOptions::default())
    }

    pub fn with_options(src: &'a str, diags: &'d mut DiagBag, opts: LexOptions) -> Self {
        Self {
            src: src.as_bytes(),
            raw: src,
            i: 0,
            line: 1,
            line_start: 0,
            diags,
            opts,
        }
    }

    pub fn collect_all(mut self) -> Vec<Token> {
        let mut out = Vec::new();
        loop {
            match self.next_token() {
                Some(t) => {
                    let eof = t.kind == TokKind::Eof;
                    out.push(t);
                    if eof {
                        break;
                    }
                }
                None => continue,
            }
        }
        out
    }

    fn peek(&self, n: usize) -> u8 {
        *self.src.get(self.i + n).unwrap_or(&0)
    }

    fn loc_from(&self, start: usize, line: u32, col: u32) -> Loc {
        Loc::new(line, col, start as u32, self.i as u32)
    }

    fn token(&self, kind: TokKind, start: usize, line: u32, col: u32) -> Token {
        Token {
            kind,
            text: self.raw[start..self.i].to_string(),
            int: 0,
            float: 0.0,
            string: String::new(),
            loc: self.loc_from(start, line, col),
        }
    }

    /// `None` means "trivia was skipped, call me again".
    pub fn next_token(&mut self) -> Option<Token> {
        while self.i < self.src.len() {
            match self.src[self.i] {
                b'\n' => {
                    self.i += 1;
                    self.line += 1;
                    self.line_start = self.i;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => self.i += 1,
                _ => break,
            }
        }
        let start = self.i;
        let line = self.line;
        let col = (start - self.line_start + 1) as u32;

        if self.i >= self.src.len() {
            return Some(self.token(TokKind::Eof, start, line, col));
        }

        let c = self.src[self.i];

        // comments -------------------------------------------------------
        if c == b'/' && self.peek(1) == b'/' {
            while self.i < self.src.len() && self.src[self.i] != b'\n' {
                self.i += 1;
            }
            return self.trivia(TokKind::Comment, start, line, col);
        }
        if c == b'/' && self.peek(1) == b'*' {
            self.i += 2;
            while self.i < self.src.len() && !(self.src[self.i] == b'*' && self.peek(1) == b'/') {
                if self.src[self.i] == b'\n' {
                    self.line += 1;
                    self.i += 1;
                    self.line_start = self.i;
                } else {
                    self.i += 1;
                }
            }
            if self.i >= self.src.len() {
                let loc = self.loc_from(start, line, col);
                self.diags
                    .error(Stage::Lex, loc, "unterminated block comment");
            } else {
                self.i += 2;
            }
            return self.trivia(TokKind::Comment, start, line, col);
        }
        if c == b'#' {
            while self.i < self.src.len() && self.src[self.i] != b'\n' {
                self.i += 1;
            }
            return self.trivia(TokKind::Preproc, start, line, col);
        }

        // identifiers ----------------------------------------------------
        if c.is_ascii_alphabetic() || c == b'_' || c == b'@' {
            while self.i < self.src.len()
                && (self.src[self.i].is_ascii_alphanumeric()
                    || self.src[self.i] == b'_'
                    || self.src[self.i] == b'@')
            {
                self.i += 1;
            }
            let text = &self.raw[start..self.i];
            let kind = if KEYWORDS.contains(&text) || TYPE_KEYWORDS.contains(&text) {
                TokKind::Keyword
            } else {
                TokKind::Ident
            };
            return Some(self.token(kind, start, line, col));
        }

        // numbers --------------------------------------------------------
        if c.is_ascii_digit() || (c == b'.' && self.peek(1).is_ascii_digit()) {
            return Some(self.number(start, line, col));
        }

        // character literal ----------------------------------------------
        if c == b'\'' {
            self.i += 1;
            let v = if self.src[self.i] == b'\\' {
                self.i += 1;
                self.escape(start, line, col)
            } else {
                let v = self.src[self.i] as i64;
                self.i += 1;
                v
            };
            if self.peek(0) != b'\'' {
                let loc = self.loc_from(start, line, col);
                self.diags
                    .error(Stage::Lex, loc, "unterminated character literal");
            } else {
                self.i += 1;
            }
            let mut t = self.token(TokKind::Char, start, line, col);
            t.int = v;
            return Some(t);
        }

        // string literal -------------------------------------------------
        if c == b'"' {
            self.i += 1;
            let mut value = String::new();
            while self.i < self.src.len() && self.src[self.i] != b'"' {
                if self.src[self.i] == b'\n' {
                    let loc = self.loc_from(start, line, col);
                    self.diags
                        .error(Stage::Lex, loc, "unterminated string literal");
                    break;
                }
                if self.src[self.i] == b'\\' {
                    self.i += 1;
                    value.push(self.escape(start, line, col) as u8 as char);
                } else {
                    value.push(self.src[self.i] as char);
                    self.i += 1;
                }
            }
            if self.peek(0) == b'"' {
                self.i += 1;
            }
            let mut t = self.token(TokKind::Str, start, line, col);
            t.string = value;
            return Some(t);
        }

        // punctuation ----------------------------------------------------
        for p in PUNCTUATORS {
            if self.raw[self.i..].starts_with(p) {
                self.i += p.len();
                return Some(self.token(TokKind::Punct, start, line, col));
            }
        }

        self.i += 1;
        let loc = self.loc_from(start, line, col);
        self.diags.error(
            Stage::Lex,
            loc,
            format!("unexpected character '{}'", c as char),
        );
        None
    }

    fn trivia(&mut self, kind: TokKind, start: usize, line: u32, col: u32) -> Option<Token> {
        if self.opts.keep_trivia {
            Some(self.token(kind, start, line, col))
        } else {
            None
        }
    }

    fn escape(&mut self, start: usize, line: u32, col: u32) -> i64 {
        let c = self.src[self.i];
        self.i += 1;
        match c {
            b'n' => 10,
            b't' => 9,
            b'r' => 13,
            b'0' => 0,
            b'b' => 8,
            b'f' => 12,
            b'v' => 11,
            b'a' => 7,
            b'\\' => 92,
            b'\'' => 39,
            b'"' => 34,
            b'x' => {
                let mut v = 0i64;
                let mut n = 0;
                while n < 2 && self.i < self.src.len() && self.src[self.i].is_ascii_hexdigit() {
                    v = v * 16 + (self.src[self.i] as char).to_digit(16).unwrap() as i64;
                    self.i += 1;
                    n += 1;
                }
                v
            }
            other => {
                let loc = self.loc_from(start, line, col);
                self.diags.warn(
                    Stage::Lex,
                    loc,
                    format!("unknown escape sequence '\\{}'", other as char),
                );
                other as i64
            }
        }
    }

    fn number(&mut self, start: usize, line: u32, col: u32) -> Token {
        if self.src[self.i] == b'0' && (self.peek(1) | 0x20) == b'x' {
            self.i += 2;
            while self.i < self.src.len() && self.src[self.i].is_ascii_hexdigit() {
                self.i += 1;
            }
            let mut t = self.token(TokKind::Int, start, line, col);
            t.int = i64::from_str_radix(&self.raw[start + 2..self.i], 16).unwrap_or(0);
            return t;
        }
        let mut is_float = false;
        while self.i < self.src.len() && self.src[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            is_float = true;
            self.i += 1;
            while self.i < self.src.len() && self.src[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if (self.peek(0) | 0x20) == b'e' {
            let save = self.i;
            self.i += 1;
            if self.peek(0) == b'+' || self.peek(0) == b'-' {
                self.i += 1;
            }
            if self.peek(0).is_ascii_digit() {
                is_float = true;
                while self.i < self.src.len() && self.src[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        let text = &self.raw[start..self.i];
        let mut t = self.token(
            if is_float {
                TokKind::Float
            } else {
                TokKind::Int
            },
            start,
            line,
            col,
        );
        if is_float {
            t.float = text.parse().unwrap_or(0.0);
        } else {
            t.int = text.parse().unwrap_or(0);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_handler() {
        let mut bag = DiagBag::new();
        let toks = Lexer::new("on timer t1 { x += 0x10; }", &mut bag).collect_all();
        assert!(!bag.has_errors());
        assert_eq!(toks[0].text, "on");
        assert_eq!(toks[2].kind, TokKind::Ident);
        assert!(toks.iter().any(|tok| tok.int == 16));
    }
}
