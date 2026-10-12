//! Trace events are deliberately independent of a parser or a UI.

use capl_model::BusFrame;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    Frame(BusFrame),
    Comment(String),
    Error { line: usize, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError {
    pub line: usize,
    pub message: String,
}

impl TraceError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}
impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for TraceError {}

pub trait TraceReader {
    fn read(&self, input: &str) -> Result<Vec<TraceEvent>, TraceError>;
}

pub fn seconds_to_us(seconds: f64) -> Result<u64, TraceError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(TraceError::new(
            0,
            "timestamp must be a finite non-negative number",
        ));
    }
    let micros = seconds * 1_000_000.0;
    if !micros.is_finite() || micros >= u64::MAX as f64 {
        return Err(TraceError::new(0, "timestamp exceeds microsecond range"));
    }
    Ok(micros.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_timestamps_outside_the_microsecond_range() {
        assert!(seconds_to_us(f64::MAX).is_err());
        assert!(seconds_to_us(f64::INFINITY).is_err());
        assert_eq!(seconds_to_us(1.25).unwrap(), 1_250_000);
    }
}
