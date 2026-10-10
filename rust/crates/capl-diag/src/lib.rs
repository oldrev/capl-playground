//! `capl-diag` — source positions and the diagnostic bag.
//!
//! Every other crate in the workspace depends on this one and on nothing else,
//! and sits underneath the lexer and the parser.
#![forbid(unsafe_code)]

use core::fmt;

/// A half-open span in the translation unit plus a resolved line/column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Loc {
    pub line: u32,
    pub col: u32,
    pub pos: u32,
    pub end: u32,
}

impl Loc {
    pub const fn new(line: u32, col: u32, pos: u32, end: u32) -> Self {
        Self { line, col, pos, end }
    }
    /// Span covering `self` through the end of `other`.
    pub fn to(self, other: Loc) -> Loc {
        Loc { end: other.end, ..self }
    }
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{})", self.line, self.col)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Stage {
    Lex,
    Parse,
    Sema,
    Codegen,
    Link,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Diagnostic {
    pub severity: Severity,
    pub stage: Stage,
    pub loc: Loc,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "Error",
            Severity::Warning => "Warning",
            Severity::Note => "Note",
        };
        write!(f, "source.can{}: {}: {}", self.loc, sev, self.message)
    }
}

/// Collects diagnostics while the compiler keeps going as long as it can.
#[derive(Clone, Debug, Default)]
pub struct DiagBag {
    items: Vec<Diagnostic>,
    errors: usize,
}

impl DiagBag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn error(&mut self, stage: Stage, loc: Loc, message: impl Into<String>) {
        self.errors += 1;
        self.items.push(Diagnostic { severity: Severity::Error, stage, loc, message: message.into() });
    }

    pub fn warn(&mut self, stage: Stage, loc: Loc, message: impl Into<String>) {
        self.items.push(Diagnostic { severity: Severity::Warning, stage, loc, message: message.into() });
    }

    pub fn note(&mut self, stage: Stage, loc: Loc, message: impl Into<String>) {
        self.items.push(Diagnostic { severity: Severity::Note, stage, loc, message: message.into() });
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }
    pub fn error_count(&self) -> usize {
        self.errors
    }
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }
    pub fn merge(&mut self, other: DiagBag) {
        self.errors += other.errors;
        self.items.extend(other.items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_errors_only() {
        let mut bag = DiagBag::new();
        bag.warn(Stage::Lex, Loc::default(), "unknown escape");
        bag.error(Stage::Parse, Loc::new(3, 5, 10, 11), "expected ';'");
        assert_eq!(bag.error_count(), 1);
        assert_eq!(bag.items().len(), 2);
        assert_eq!(bag.items()[1].to_string(), "source.can(3,5): Error: expected ';'");
    }
}
