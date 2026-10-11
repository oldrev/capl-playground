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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalValue {
    pub name: String,
    pub raw: u64,
    pub physical: f64,
}
