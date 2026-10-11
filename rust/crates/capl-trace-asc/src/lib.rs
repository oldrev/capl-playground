//! A tolerant reader for the text forms most often exported by CANoe and candump.
//! Binary BLF/MF4 are intentionally separate future crates.

use capl_model::{BusFrame, Direction};
use capl_trace_core::{seconds_to_us, TraceError, TraceEvent, TraceReader};

#[derive(Clone, Copy, Debug, Default)]
pub struct AscReader;

impl TraceReader for AscReader {
    fn read(&self, input: &str) -> Result<Vec<TraceEvent>, TraceError> {
        let mut out = Vec::new();
        for (line_no, line) in input.lines().enumerate() {
            let line_no = line_no + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with("//") {
                continue;
            }
            if let Some(event) = parse_line(line, line_no)? {
                out.push(TraceEvent::Frame(event));
            }
        }
        Ok(out)
    }
}

fn parse_line(line: &str, line_no: usize) -> Result<Option<BusFrame>, TraceError> {
    // candump: (seconds) iface id#payload
    if let Some(rest) = line.strip_prefix('(') {
        if let Some((stamp, rest)) = rest.split_once(')') {
            let mut p = rest.split_whitespace();
            let _iface = p
                .next()
                .ok_or_else(|| TraceError::new(line_no, "missing candump interface"))?;
            let frame = p
                .next()
                .ok_or_else(|| TraceError::new(line_no, "missing candump frame"))?;
            let (id, payload) = frame
                .split_once('#')
                .ok_or_else(|| TraceError::new(line_no, "expected id#payload"))?;
            let id = parse_id(id, line_no)?;
            let data = parse_hex_bytes(payload, line_no)?;
            return Ok(Some(BusFrame::new(
                seconds_to_us(parse_f64(stamp, line_no)?)?,
                1,
                id,
                Direction::Rx,
                data,
            )));
        }
    }

    // Vector ASC: time channel id Rx|Tx d|DLC length bytes...
    let mut p = line.split_whitespace();
    let stamp = match p.next() {
        Some(v) => parse_f64(v, line_no)?,
        None => return Ok(None),
    };
    let mut tokens: Vec<&str> = p.collect();
    // Some exports include a literal CAN/LIN bus name before channel.
    if tokens
        .first()
        .is_some_and(|v| v.eq_ignore_ascii_case("CAN") || v.eq_ignore_ascii_case("LIN"))
    {
        tokens.remove(0);
    }
    if tokens.len() < 5 {
        return Ok(None);
    }
    let channel = tokens[0].parse::<u16>().unwrap_or(1);
    let id = parse_id(tokens[1], line_no)?;
    let direction = if tokens.iter().any(|v| v.eq_ignore_ascii_case("Tx")) {
        Direction::Tx
    } else {
        Direction::Rx
    };
    let d_index = tokens
        .iter()
        .position(|v| v.eq_ignore_ascii_case("d") || v.eq_ignore_ascii_case("dlc"));
    let start = d_index.map(|i| i + 1).unwrap_or(3);
    let length = tokens
        .get(start)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut data = Vec::with_capacity(length);
    for token in tokens.iter().skip(start + 1).take(length) {
        data.push(parse_byte(token, line_no)?);
    }
    if data.len() != length {
        return Err(TraceError::new(
            line_no,
            "frame has fewer data bytes than its DLC",
        ));
    }
    Ok(Some(BusFrame::new(
        seconds_to_us(stamp)?,
        channel,
        id,
        direction,
        data,
    )))
}

fn parse_f64(s: &str, line: usize) -> Result<f64, TraceError> {
    s.parse()
        .map_err(|_| TraceError::new(line, format!("invalid timestamp {s:?}")))
}
fn parse_id(s: &str, line: usize) -> Result<u32, TraceError> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|_| TraceError::new(line, format!("invalid CAN id {s:?}")))
}
fn parse_byte(s: &str, line: usize) -> Result<u8, TraceError> {
    u8::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|_| TraceError::new(line, format!("invalid data byte {s:?}")))
}
fn parse_hex_bytes(s: &str, line: usize) -> Result<Vec<u8>, TraceError> {
    if !s.len().is_multiple_of(2) {
        return Err(TraceError::new(line, "candump payload has odd hex length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| parse_byte(&s[i..i + 2], line))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_vector_asc() {
        let events = AscReader.read("0.100000 1 123 Rx d 3 11 22 FF\n").unwrap();
        let TraceEvent::Frame(frame) = &events[0] else {
            panic!()
        };
        assert_eq!(frame.timestamp_us, 100_000);
        assert_eq!(frame.id, 0x123);
        assert_eq!(frame.data, [0x11, 0x22, 0xff]);
    }
    #[test]
    fn parses_candump() {
        let events = AscReader.read("(1.25) can0 18FF50E5#0102A0\n").unwrap();
        let TraceEvent::Frame(frame) = &events[0] else {
            panic!()
        };
        assert_eq!(frame.timestamp_us, 1_250_000);
        assert_eq!(frame.id, 0x18ff50e5);
    }
}
