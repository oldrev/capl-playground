use capl_database_core::{ByteOrder, Message, NetworkDatabase, Signal};
use std::fmt;

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

pub fn parse(input: &str) -> Result<NetworkDatabase, DbcError> {
    let mut db = NetworkDatabase::default();
    for (n, line) in input.lines().enumerate() {
        let line_no = n + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        match parts.first().copied() {
            Some("BU_:") => db.nodes.extend(
                parts
                    .iter()
                    .skip(1)
                    .filter(|p| **p != "Vector__XXX")
                    .map(|p| p.to_string()),
            ),
            Some("BO_") => {
                if parts.len() < 5 {
                    return Err(err(line_no, "invalid BO_ line"));
                }
                let id = parts[1]
                    .trim_end_matches(':')
                    .parse()
                    .map_err(|_| err(line_no, "invalid message id"))?;
                let length = parts[3]
                    .parse()
                    .map_err(|_| err(line_no, "invalid message length"))?;
                db.messages.push(Message {
                    id,
                    name: parts[2].to_string(),
                    length,
                    transmitter: Some(parts[4].to_string()),
                    signals: Vec::new(),
                });
            }
            Some("SG_") => {
                let message = db
                    .messages
                    .last_mut()
                    .ok_or_else(|| err(line_no, "SG_ appears before BO_"))?;
                let spec = parts
                    .get(3)
                    .ok_or_else(|| err(line_no, "invalid SG_ line"))?;
                let (start_bit, length, byte_order, signed) = parse_signal_spec(spec, line_no)?;
                let (factor, offset) = parts
                    .get(4)
                    .and_then(|v| v.strip_prefix('('))
                    .and_then(|v| v.strip_suffix(')'))
                    .and_then(|v| v.split_once(','))
                    .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                    .unwrap_or((1.0, 0.0));
                message.signals.push(Signal {
                    name: parts[1].to_string(),
                    start_bit,
                    length,
                    byte_order,
                    signed,
                    factor,
                    offset,
                    unit: None,
                });
            }
            _ => {}
        }
    }
    Ok(db)
}
fn parse_signal_spec(spec: &str, line: usize) -> Result<(u16, u16, ByteOrder, bool), DbcError> {
    let (pos, typ) = spec
        .split_once('@')
        .ok_or_else(|| err(line, "invalid signal bit spec"))?;
    let (start, len) = pos
        .split_once('|')
        .ok_or_else(|| err(line, "invalid signal bit range"))?;
    let mut chars = typ.chars();
    let order = match chars.next() {
        Some('1') => ByteOrder::LittleEndian,
        Some('0') => ByteOrder::BigEndian,
        _ => return Err(err(line, "invalid byte order")),
    };
    let signed = matches!(chars.next(), Some('-'));
    Ok((
        start
            .parse()
            .map_err(|_| err(line, "invalid signal start bit"))?,
        len.parse()
            .map_err(|_| err(line, "invalid signal length"))?,
        order,
        signed,
    ))
}
fn err(line: usize, message: impl Into<String>) -> DbcError {
    DbcError {
        line,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_messages_and_signals() {
        let db = parse("BU_: ECU1 ECU2\nBO_ 100 Msg: 8 ECU1\n SG_ Speed : 0|16@1+ (0.1,-10) [0|100] \"km/h\" ECU2\n").unwrap();
        assert_eq!(db.messages[0].id, 100);
        assert_eq!(db.messages[0].signals[0].factor, 0.1);
    }
}
