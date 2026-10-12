//! Stable, format-independent bus models shared by parsers, runtime and UI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    #[default]
    Rx,
    Tx,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BusKind {
    #[default]
    Can,
    CanFd,
    Lin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusFrame {
    pub timestamp_us: u64,
    pub channel: u16,
    pub id: u32,
    pub direction: Direction,
    pub kind: BusKind,
    pub data: Vec<u8>,
}

impl BusFrame {
    pub fn new(
        timestamp_us: u64,
        channel: u16,
        id: u32,
        direction: Direction,
        data: Vec<u8>,
    ) -> Self {
        Self {
            timestamp_us,
            channel,
            id,
            direction,
            kind: BusKind::Can,
            data,
        }
    }
    pub fn dlc(&self) -> usize {
        self.data.len()
    }
}

/// Parse a hexadecimal bus identifier, accepting an optional `0x`/`0X` prefix.
pub fn parse_hex_u32(input: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(strip_hex_prefix(input.trim()), 16)
}

/// Parse whitespace- or comma-separated hexadecimal data bytes.
pub fn parse_hex_bytes(input: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    input
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|token| !token.is_empty())
        .map(|token| u8::from_str_radix(strip_hex_prefix(token), 16))
        .collect()
}

fn strip_hex_prefix(input: &str) -> &str {
    input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .unwrap_or(input)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalValue {
    pub name: String,
    pub raw: u64,
    pub physical: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_entered_hex_frames_consistently() {
        assert_eq!(parse_hex_u32(" 0X18FF50E5 ").unwrap(), 0x18ff50e5);
        assert_eq!(parse_hex_bytes("12, 0X34\tAB").unwrap(), [0x12, 0x34, 0xab]);
        assert!(parse_hex_u32("0x0x10").is_err());
        assert!(parse_hex_bytes("1FF").is_err());
    }
}
