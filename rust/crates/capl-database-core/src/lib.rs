use capl_model::{BusFrame, SignalValue};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByteOrder {
    LittleEndian,
    BigEndian,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub name: String,
    pub start_bit: u16,
    pub length: u16,
    pub byte_order: ByteOrder,
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: u32,
    pub name: String,
    pub length: u8,
    pub transmitter: Option<String>,
    pub signals: Vec<Signal>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetworkDatabase {
    pub messages: Vec<Message>,
    pub nodes: Vec<String>,
}

impl NetworkDatabase {
    pub fn message(&self, id: u32) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }
    pub fn decode(&self, frame: &BusFrame) -> Option<Vec<SignalValue>> {
        let message = self.message(frame.id)?;
        Some(
            message
                .signals
                .iter()
                .map(|signal| {
                    let raw = extract_raw(&frame.data, signal);
                    let signed = if signal.signed
                        && signal.length < 64
                        && (raw & (1u64 << (signal.length - 1))) != 0
                    {
                        (raw as i64 - (1i64 << signal.length)) as f64
                    } else {
                        raw as f64
                    };
                    SignalValue {
                        name: signal.name.clone(),
                        raw,
                        physical: signed * signal.factor + signal.offset,
                    }
                })
                .collect(),
        )
    }
}

fn extract_raw(data: &[u8], signal: &Signal) -> u64 {
    let mut value = 0u64;
    for i in 0..signal.length.min(64) {
        let bit = signal.start_bit + i;
        let byte = (bit / 8) as usize;
        if byte >= data.len() {
            break;
        }
        let bit_in_byte = match signal.byte_order {
            ByteOrder::LittleEndian => bit % 8,
            ByteOrder::BigEndian => 7 - (bit % 8),
        };
        value |= (((data[byte] >> bit_in_byte) & 1) as u64) << i;
    }
    value
}
