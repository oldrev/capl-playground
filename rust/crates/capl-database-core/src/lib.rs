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
                        && signal.length > 0
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
    match signal.byte_order {
        ByteOrder::LittleEndian => {
            for offset in 0..signal.length.min(64) {
                let bit = signal.start_bit.saturating_add(offset) as usize;
                let byte = bit / 8;
                if byte >= data.len() {
                    break;
                }
                value |= (((data[byte] >> (bit % 8)) & 1) as u64) << offset;
            }
        }
        ByteOrder::BigEndian => {
            let mut bit = signal.start_bit as usize;
            for _ in 0..signal.length.min(64) {
                let byte = bit / 8;
                if byte >= data.len() {
                    break;
                }
                value = (value << 1) | u64::from((data[byte] >> (7 - bit % 8)) & 1);
                bit = if bit.is_multiple_of(8) {
                    bit + 15
                } else {
                    bit - 1
                };
            }
        }
    }
    value
}

#[cfg(test)]
mod decode_tests {
    use super::*;

    #[test]
    fn decodes_little_endian_raw_and_physical_values() {
        let db = NetworkDatabase {
            messages: vec![Message {
                id: 0x123,
                name: "Sample".into(),
                length: 8,
                transmitter: None,
                signals: vec![Signal {
                    name: "Speed".into(),
                    start_bit: 0,
                    length: 16,
                    byte_order: ByteOrder::LittleEndian,
                    signed: false,
                    factor: 0.5,
                    offset: 1.0,
                    unit: Some("km/h".into()),
                }],
            }],
            nodes: vec![],
        };
        let frame = BusFrame::new(0, 1, 0x123, capl_model::Direction::Rx, vec![0x34, 0x12]);
        let decoded = db.decode(&frame).unwrap();
        assert_eq!(decoded[0].raw, 0x1234);
        assert_eq!(decoded[0].physical, 2331.0);
    }

    #[test]
    fn decodes_motorola_sawtooth_and_signed_values() {
        let db = NetworkDatabase {
            messages: vec![Message {
                id: 7,
                name: "Motorola".into(),
                length: 1,
                transmitter: None,
                signals: vec![Signal {
                    name: "SignedNibble".into(),
                    start_bit: 4,
                    length: 4,
                    byte_order: ByteOrder::BigEndian,
                    signed: true,
                    factor: 2.0,
                    offset: -1.0,
                    unit: None,
                }],
            }],
            nodes: vec![],
        };
        let frame = BusFrame::new(0, 1, 7, capl_model::Direction::Rx, vec![0x28]);
        let decoded = db.decode(&frame).unwrap();
        assert_eq!(decoded[0].raw, 0b1010);
        assert_eq!(decoded[0].physical, -13.0);
    }
}
