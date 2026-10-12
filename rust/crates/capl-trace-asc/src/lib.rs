//! A tolerant reader for the text forms most often exported by CANoe and candump.
//! Binary BLF/MF4 are intentionally separate future crates.

use capl_model::{BusFrame, BusKind, Direction};
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
        let (stamp, rest) = rest
            .split_once(')')
            .ok_or_else(|| TraceError::new(line_no, "unterminated candump timestamp"))?;
        let mut p = rest.split_whitespace();
        let _iface = p
            .next()
            .ok_or_else(|| TraceError::new(line_no, "missing candump interface"))?;
        let frame = p
            .next()
            .ok_or_else(|| TraceError::new(line_no, "missing candump frame"))?;
        if p.next().is_some() {
            return Err(TraceError::new(
                line_no,
                "unexpected fields after candump frame",
            ));
        }
        let (id, payload, kind) = if let Some((id, payload)) = frame.split_once("##") {
            let Some((flags, payload)) = payload.split_at_checked(1) else {
                return Err(TraceError::new(line_no, "missing CAN-FD flags"));
            };
            if hex_nibble(flags.as_bytes()[0]).is_none() {
                return Err(TraceError::new(line_no, "invalid CAN-FD flags"));
            }
            (id, payload, BusKind::CanFd)
        } else if let Some((id, payload)) = frame.split_once('#') {
            (id, payload, BusKind::Can)
        } else {
            return Err(TraceError::new(line_no, "expected id#payload"));
        };
        let id = parse_id(id, line_no)?;
        let data = if payload.eq_ignore_ascii_case("r") {
            Vec::new()
        } else {
            parse_hex_bytes(payload, line_no)?
        };
        let mut bus_frame =
            BusFrame::new(parse_timestamp(stamp, line_no)?, 1, id, Direction::Rx, data);
        bus_frame.kind = kind;
        return Ok(Some(bus_frame));
    }

    // Vector ASC: time channel id Rx|Tx d|DLC length bytes...
    let mut p = line.split_whitespace();
    let stamp_text = match p.next() {
        Some(v) => v,
        None => return Ok(None),
    };
    // ASC headers and trigger-block markers are not frames. A numeric first
    // field, however, is expected to be a timestamp and must validate.
    if !looks_numeric(stamp_text) {
        return Ok(None);
    }
    let stamp = parse_f64(stamp_text, line_no)?;
    let mut field = |what: &str| {
        p.next()
            .ok_or_else(|| TraceError::new(line_no, format!("missing {what}")))
    };
    let mut first = field("channel or bus name")?;
    let bus_kind = if first.eq_ignore_ascii_case("CANFD") || first.eq_ignore_ascii_case("CAN_FD") {
        first = field("channel")?;
        BusKind::CanFd
    } else if first.eq_ignore_ascii_case("LIN") {
        first = field("channel")?;
        BusKind::Lin
    } else if first.eq_ignore_ascii_case("CAN") {
        first = field("channel")?;
        BusKind::Can
    } else if !looks_numeric(first) {
        // ASC contains timestamped annotations such as "Start of measurement"
        // alongside frames. They are not frame records.
        return Ok(None);
    } else {
        BusKind::Can
    };
    let channel = parse_decimal(first, "channel", line_no)?;
    let id = parse_id(field("CAN id")?, line_no)?;
    let direction = match field("direction")? {
        value if value.eq_ignore_ascii_case("rx") => Direction::Rx,
        value if value.eq_ignore_ascii_case("tx") => Direction::Tx,
        value => {
            return Err(TraceError::new(
                line_no,
                format!("invalid direction {value:?}"),
            ))
        }
    };
    let frame_type = field("frame type")?;
    let length = parse_decimal(field("DLC")?, "DLC", line_no)? as usize;
    if length > 64 {
        return Err(TraceError::new(line_no, "DLC exceeds CAN-FD maximum of 64"));
    }
    let mut data = Vec::with_capacity(length);
    if frame_type.eq_ignore_ascii_case("d") {
        for _ in 0..length {
            let token = field("data byte")?;
            data.push(parse_byte(token, line_no)?);
        }
    } else if !frame_type.eq_ignore_ascii_case("r") {
        return Err(TraceError::new(
            line_no,
            format!("invalid frame type {frame_type:?}"),
        ));
    }
    if p.next().is_some() {
        return Err(TraceError::new(
            line_no,
            "unexpected fields after frame payload",
        ));
    }
    let kind = if bus_kind == BusKind::CanFd || length > 8 {
        BusKind::CanFd
    } else {
        bus_kind
    };
    let mut bus_frame = BusFrame::new(
        timestamp_to_us(stamp, line_no)?,
        channel,
        id,
        direction,
        data,
    );
    bus_frame.kind = kind;
    Ok(Some(bus_frame))
}

fn parse_f64(s: &str, line: usize) -> Result<f64, TraceError> {
    s.parse()
        .map_err(|_| TraceError::new(line, format!("invalid timestamp {s:?}")))
}

fn parse_timestamp(s: &str, line: usize) -> Result<u64, TraceError> {
    timestamp_to_us(parse_f64(s, line)?, line)
}

fn timestamp_to_us(seconds: f64, line: usize) -> Result<u64, TraceError> {
    seconds_to_us(seconds).map_err(|error| TraceError::new(line, error.message))
}
fn parse_id(s: &str, line: usize) -> Result<u32, TraceError> {
    u32::from_str_radix(strip_hex_prefix(s), 16)
        .map_err(|_| TraceError::new(line, format!("invalid CAN id {s:?}")))
}
fn parse_byte(s: &str, line: usize) -> Result<u8, TraceError> {
    u8::from_str_radix(strip_hex_prefix(s), 16)
        .map_err(|_| TraceError::new(line, format!("invalid data byte {s:?}")))
}

fn strip_hex_prefix(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

fn parse_decimal(s: &str, what: &str, line: usize) -> Result<u16, TraceError> {
    s.parse::<u16>()
        .map_err(|_| TraceError::new(line, format!("invalid {what} {s:?}")))
}

fn looks_numeric(s: &str) -> bool {
    s.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_digit() || *b == b'+' || *b == b'-' || *b == b'.')
}
fn parse_hex_bytes(s: &str, line: usize) -> Result<Vec<u8>, TraceError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(TraceError::new(line, "candump payload has odd hex length"));
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0]);
            let low = hex_nibble(pair[1]);
            high.zip(low)
                .map(|(high, low)| (high << 4) | low)
                .ok_or_else(|| {
                    TraceError::new(
                        line,
                        format!("invalid data byte {:?}", String::from_utf8_lossy(pair)),
                    )
                })
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
        let events = AscReader.read("(1.25) can0 0X18FF50E5#0102A0\n").unwrap();
        let TraceEvent::Frame(frame) = &events[0] else {
            panic!()
        };
        assert_eq!(frame.timestamp_us, 1_250_000);
        assert_eq!(frame.id, 0x18ff50e5);
    }

    #[test]
    fn parses_remote_and_can_fd_candump_frames() {
        let events = AscReader
            .read("(1.25) can0 123#R\n(1.26) can0 123##0112233\n")
            .unwrap();
        let TraceEvent::Frame(remote) = &events[0] else {
            panic!()
        };
        assert!(remote.data.is_empty());
        assert_eq!(remote.kind, BusKind::Can);
        let TraceEvent::Frame(fd) = &events[1] else {
            panic!()
        };
        assert_eq!(fd.kind, BusKind::CanFd);
        assert_eq!(fd.data, [0x11, 0x22, 0x33]);
    }

    #[test]
    fn identifies_vector_bus_kind_and_validates_candump_fd_flags() {
        let events = AscReader
            .read("0.1 LIN 1 123 Rx d 1 AA\n0.2 CANFD 2 321 Rx d 1 BB\n")
            .unwrap();
        let TraceEvent::Frame(lin) = &events[0] else {
            panic!()
        };
        let TraceEvent::Frame(fd) = &events[1] else {
            panic!()
        };
        assert_eq!(lin.kind, BusKind::Lin);
        assert_eq!(fd.channel, 2);
        assert_eq!(fd.kind, BusKind::CanFd);

        let error = AscReader.read("(1.0) can0 123##Z1122\n").unwrap_err();
        assert!(error.message.contains("CAN-FD flags"));
    }

    #[test]
    fn ignores_asc_headers_but_rejects_bad_frames() {
        let events = AscReader
            .read("date Mon Jan 01 00:00:00 2024\nBegin Triggerblock\n0.0 Start of measurement\n0.1 CAN nope Rx d 1 00\n")
            .unwrap_err();
        assert_eq!(events.line, 4);
        assert!(events.message.contains("channel"));
    }

    #[test]
    fn validates_dlc_and_payload_count() {
        let error = AscReader.read("0.1 1 123 Rx d 2 00\n").unwrap_err();
        assert!(error.message.contains("data byte"));
        let error = AscReader.read("0.1 1 123 Rx d 1 00 extra\n").unwrap_err();
        assert!(error.message.contains("unexpected fields"));
    }

    #[test]
    fn rejects_malformed_candump_without_panicking_on_unicode() {
        let error = AscReader.read("(1.0 can0 123#00\n").unwrap_err();
        assert!(error.message.contains("unterminated"));
        let error = AscReader.read("(1.0) can0 123#aéx\n").unwrap_err();
        assert!(error.message.contains("invalid data byte"));
        let error = AscReader.read("(1.0) can0 123#00 extra\n").unwrap_err();
        assert!(error.message.contains("unexpected fields"));
    }
}
