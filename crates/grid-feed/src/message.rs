//! The messages and their binary encoding.

use std::fmt;

/// The version every message starts with. A decoder rejects any other.
pub const PROTOCOL_VERSION: u8 = 1;

const HELLO: u8 = 0x01;
const RESYNC: u8 = 0x02;
const SNAPSHOT: u8 = 0x10;
const DELTA: u8 = 0x11;
const HEARTBEAT: u8 = 0x12;

/// `Hello` flags.
const HAS_LAST_SEQ: u8 = 0b01;
const HAS_TOKEN: u8 = 0b10;

/// One instrument's quote on the wire: what changes with each update. The
/// client takes the rest (ticker, name, sector, previous close) from its own
/// instrument table by `id`.
///
/// A value type: its fields are the record's fixed layout, which changes
/// only with [`PROTOCOL_VERSION`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuoteRecord {
    /// The instrument id: the row key.
    pub id: u32,
    pub last: f64,
    pub bid: f64,
    pub ask: f64,
    pub volume: i64,
}

impl QuoteRecord {
    /// Bytes per record: `id` (u32), `last`, `bid`, `ask` (f64) and
    /// `volume` (i64).
    pub const LEN: usize = 4 + 3 * 8 + 8;

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.last.to_le_bytes());
        out.extend_from_slice(&self.bid.to_le_bytes());
        out.extend_from_slice(&self.ask.to_le_bytes());
        out.extend_from_slice(&self.volume.to_le_bytes());
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.u32()?,
            last: reader.f64()?,
            bid: reader.f64()?,
            ask: reader.f64()?,
            volume: reader.i64()?,
        })
    }
}

/// What the client sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// The first message on a connection.
    Hello {
        /// The last sequence number the client applied, from an earlier
        /// connection; `None` on the first.
        last_seq: Option<u64>,
        /// An access token, for a server that asks for one.
        token: Option<String>,
    },
    /// The client saw a gap: send a snapshot.
    Resync,
}

/// What the server sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    /// Every row, as of `seq`.
    Snapshot {
        seq: u64,
        /// Server time when sent, in milliseconds since the Unix epoch.
        time_ms: u64,
        rows: Vec<QuoteRecord>,
    },
    /// The rows changed since the previous message, the latest value of
    /// each, and the keys removed.
    Delta {
        seq: u64,
        /// Server time when sent, in milliseconds since the Unix epoch.
        time_ms: u64,
        upserts: Vec<QuoteRecord>,
        removes: Vec<u32>,
    },
    /// The connection is alive; `seq` is the last sequence number taken.
    Heartbeat {
        seq: u64,
        /// Server time when sent, in milliseconds since the Unix epoch.
        time_ms: u64,
    },
}

/// Why bytes are not a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The message ends before its fields do.
    Truncated,
    /// Another protocol version.
    Version(u8),
    /// An unknown message type.
    MessageType(u8),
    /// Bytes after the message's last field.
    TrailingBytes,
    /// A `Hello` token that is not UTF-8, or unknown `Hello` flags.
    Malformed,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("message truncated"),
            Self::Version(version) => write!(f, "protocol version {version} not supported"),
            Self::MessageType(kind) => write!(f, "unknown message type {kind:#04x}"),
            Self::TrailingBytes => f.write_str("bytes after the message"),
            Self::Malformed => f.write_str("malformed message"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl ClientMessage {
    /// Appends the encoded message to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Hello { last_seq, token } => {
                out.extend_from_slice(&[PROTOCOL_VERSION, HELLO]);
                let flags = if last_seq.is_some() { HAS_LAST_SEQ } else { 0 }
                    | if token.is_some() { HAS_TOKEN } else { 0 };
                out.push(flags);
                if let Some(seq) = last_seq {
                    out.extend_from_slice(&seq.to_le_bytes());
                }
                if let Some(token) = token {
                    // A token longer than a u16 length is cut at a char
                    // boundary; tokens are short.
                    let mut len = token.len().min(usize::from(u16::MAX));
                    while !token.is_char_boundary(len) {
                        len -= 1;
                    }
                    out.extend_from_slice(&(len as u16).to_le_bytes());
                    out.extend_from_slice(&token.as_bytes()[..len]);
                }
            }
            Self::Resync => out.extend_from_slice(&[PROTOCOL_VERSION, RESYNC]),
        }
    }

    /// The encoded message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    /// Decodes one message. Never panics: malformed bytes are an error.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let message = match reader.header()? {
            HELLO => {
                let flags = reader.u8()?;
                if flags & !(HAS_LAST_SEQ | HAS_TOKEN) != 0 {
                    return Err(DecodeError::Malformed);
                }
                let last_seq = if flags & HAS_LAST_SEQ != 0 {
                    Some(reader.u64()?)
                } else {
                    None
                };
                let token = if flags & HAS_TOKEN != 0 {
                    let len = usize::from(reader.u16()?);
                    let bytes = reader.take(len)?;
                    let token = std::str::from_utf8(bytes).map_err(|_| DecodeError::Malformed)?;
                    Some(token.to_owned())
                } else {
                    None
                };
                Self::Hello { last_seq, token }
            }
            RESYNC => Self::Resync,
            kind => return Err(DecodeError::MessageType(kind)),
        };
        reader.finish()?;
        Ok(message)
    }
}

impl ServerMessage {
    /// The message's sequence number.
    pub fn seq(&self) -> u64 {
        match self {
            Self::Snapshot { seq, .. } | Self::Delta { seq, .. } | Self::Heartbeat { seq, .. } => {
                *seq
            }
        }
    }

    /// Server time when sent, in milliseconds since the Unix epoch.
    pub fn time_ms(&self) -> u64 {
        match self {
            Self::Snapshot { time_ms, .. }
            | Self::Delta { time_ms, .. }
            | Self::Heartbeat { time_ms, .. } => *time_ms,
        }
    }

    /// Appends the encoded message to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Snapshot { seq, time_ms, rows } => {
                out.reserve(2 + 8 + 8 + 4 + rows.len() * QuoteRecord::LEN);
                out.extend_from_slice(&[PROTOCOL_VERSION, SNAPSHOT]);
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(&time_ms.to_le_bytes());
                encode_records(rows, out);
            }
            Self::Delta {
                seq,
                time_ms,
                upserts,
                removes,
            } => {
                out.reserve(2 + 8 + 8 + 8 + upserts.len() * QuoteRecord::LEN + removes.len() * 4);
                out.extend_from_slice(&[PROTOCOL_VERSION, DELTA]);
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(&time_ms.to_le_bytes());
                encode_records(upserts, out);
                out.extend_from_slice(&(removes.len() as u32).to_le_bytes());
                for id in removes {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
            Self::Heartbeat { seq, time_ms } => {
                out.extend_from_slice(&[PROTOCOL_VERSION, HEARTBEAT]);
                out.extend_from_slice(&seq.to_le_bytes());
                out.extend_from_slice(&time_ms.to_le_bytes());
            }
        }
    }

    /// The encoded message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    /// Decodes one message. Never panics: malformed bytes are an error, and
    /// a count larger than the bytes left is one before anything is
    /// allocated.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let kind = reader.header()?;
        let message = match kind {
            SNAPSHOT | DELTA | HEARTBEAT => {
                let (seq, time_ms) = (reader.u64()?, reader.u64()?);
                match kind {
                    SNAPSHOT => Self::Snapshot {
                        seq,
                        time_ms,
                        rows: decode_records(&mut reader)?,
                    },
                    DELTA => {
                        let upserts = decode_records(&mut reader)?;
                        let count = reader.count(4)?;
                        let removes = (0..count).map(|_| reader.u32()).collect::<Result<_, _>>()?;
                        Self::Delta {
                            seq,
                            time_ms,
                            upserts,
                            removes,
                        }
                    }
                    _ => Self::Heartbeat { seq, time_ms },
                }
            }
            kind => return Err(DecodeError::MessageType(kind)),
        };
        reader.finish()?;
        Ok(message)
    }
}

fn encode_records(records: &[QuoteRecord], out: &mut Vec<u8>) {
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for record in records {
        record.encode(out);
    }
}

fn decode_records(reader: &mut Reader<'_>) -> Result<Vec<QuoteRecord>, DecodeError> {
    let count = reader.count(QuoteRecord::LEN)?;
    (0..count).map(|_| QuoteRecord::decode(reader)).collect()
}

/// A bounds-checked cursor over a message.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.bytes.len() < len {
            return Err(DecodeError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        self.array().map(i64::from_le_bytes)
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        self.array().map(f64::from_le_bytes)
    }

    /// The version check and the message type.
    fn header(&mut self) -> Result<u8, DecodeError> {
        let version = self.u8()?;
        if version != PROTOCOL_VERSION {
            return Err(DecodeError::Version(version));
        }
        self.u8()
    }

    /// A u32 count of items `item_len` bytes each, checked against the bytes
    /// left so a forged count allocates nothing.
    fn count(&mut self, item_len: usize) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count.saturating_mul(item_len) > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        Ok(count)
    }

    fn finish(&self) -> Result<(), DecodeError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32) -> QuoteRecord {
        QuoteRecord {
            id,
            last: 100. + f64::from(id) * 0.01,
            bid: 99.99,
            ask: 100.01,
            volume: 1_000 * i64::from(id),
        }
    }

    fn server_messages() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Snapshot {
                seq: 1,
                time_ms: 1_700_000_000_000,
                rows: (0..150).map(record).collect(),
            },
            ServerMessage::Snapshot {
                seq: 9,
                time_ms: 0,
                rows: Vec::new(),
            },
            ServerMessage::Delta {
                seq: 2,
                time_ms: 1_700_000_000_020,
                upserts: vec![record(3), record(149)],
                removes: vec![7, 8],
            },
            ServerMessage::Delta {
                seq: u64::MAX,
                time_ms: u64::MAX,
                upserts: Vec::new(),
                removes: Vec::new(),
            },
            ServerMessage::Heartbeat {
                seq: 42,
                time_ms: 1_700_000_001_000,
            },
        ]
    }

    #[test]
    fn test_server_messages_round_trip() {
        for message in server_messages() {
            let bytes = message.to_bytes();
            assert_eq!(ServerMessage::decode(&bytes), Ok(message));
        }
    }

    #[test]
    fn test_client_messages_round_trip() {
        for message in [
            ClientMessage::Hello {
                last_seq: None,
                token: None,
            },
            ClientMessage::Hello {
                last_seq: Some(1234),
                token: Some("dev-token-é".into()),
            },
            ClientMessage::Hello {
                last_seq: Some(0),
                token: None,
            },
            ClientMessage::Resync,
        ] {
            assert_eq!(ClientMessage::decode(&message.to_bytes()), Ok(message));
        }
    }

    #[test]
    fn test_records_are_fixed_width() {
        let delta = ServerMessage::Delta {
            seq: 1,
            time_ms: 0,
            upserts: vec![record(1), record(2), record(3)],
            removes: vec![4],
        };
        assert_eq!(QuoteRecord::LEN, 36);
        assert_eq!(delta.to_bytes().len(), 2 + 16 + 4 + 3 * 36 + 4 + 4);
    }

    #[test]
    fn test_malformed_messages_are_errors() {
        let mut snapshot = server_messages()[0].to_bytes();
        // Every prefix of a valid message is truncated.
        for len in 0..snapshot.len() {
            assert!(
                ServerMessage::decode(&snapshot[..len]).is_err(),
                "prefix {len}"
            );
        }
        snapshot.push(0);
        assert_eq!(
            ServerMessage::decode(&snapshot),
            Err(DecodeError::TrailingBytes)
        );
        snapshot[0] = 2;
        assert_eq!(
            ServerMessage::decode(&snapshot),
            Err(DecodeError::Version(2))
        );
        // A forged count is refused before allocating.
        let mut forged = vec![PROTOCOL_VERSION, SNAPSHOT];
        forged.extend_from_slice(&[0; 16]);
        forged.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(ServerMessage::decode(&forged), Err(DecodeError::Truncated));
        // A client message is not a server message, and the reverse.
        assert_eq!(
            ServerMessage::decode(&ClientMessage::Resync.to_bytes()),
            Err(DecodeError::MessageType(RESYNC))
        );
        assert_eq!(
            ClientMessage::decode(&[PROTOCOL_VERSION, HELLO, 0b100]),
            Err(DecodeError::Malformed)
        );
        assert_eq!(
            ClientMessage::decode(&[PROTOCOL_VERSION, HELLO, HAS_TOKEN, 1, 0, 0xff]),
            Err(DecodeError::Malformed)
        );
    }

    #[test]
    fn test_random_bytes_decode_to_errors_without_panic() {
        let mut rng = Lcg(0x5eed);
        let valid = server_messages()
            .iter()
            .map(ServerMessage::to_bytes)
            .collect::<Vec<_>>();
        let mut errors = 0;
        for round in 0..400 {
            let bytes = if round % 4 != 3 {
                // Noise of any length; two thirds start with a valid header,
                // so the body decoders run on it.
                let len = (rng.next() % 128) as usize;
                let mut bytes = (0..len).map(|_| rng.next() as u8).collect::<Vec<_>>();
                if round % 4 != 0 && bytes.len() >= 2 {
                    bytes[0] = PROTOCOL_VERSION;
                    bytes[1] = [HELLO, RESYNC, SNAPSHOT, DELTA, HEARTBEAT][round % 5];
                }
                bytes
            } else {
                // A valid message with one byte flipped, cut short.
                let mut bytes = valid[round % valid.len()].clone();
                let ix = (rng.next() as usize) % bytes.len();
                bytes[ix] ^= (rng.next() as u8) | 1;
                let cut = (rng.next() as usize) % bytes.len();
                bytes.truncate(cut.max(ix + 1).min(bytes.len() - 1));
                bytes
            };
            // An error, or (by chance) a message whose encoding is exactly
            // these bytes: the encoding is canonical.
            match ServerMessage::decode(&bytes) {
                Ok(message) => assert_eq!(message.to_bytes(), bytes),
                Err(_) => errors += 1,
            }
            if let Ok(message) = ClientMessage::decode(&bytes) {
                assert_eq!(message.to_bytes(), bytes);
            }
        }
        assert!(errors >= 390, "{errors} of 400 random strings were errors");
        // With another version, always an error.
        for _ in 0..100 {
            let len = 1 + (rng.next() % 64) as usize;
            let mut bytes = (0..len).map(|_| rng.next() as u8).collect::<Vec<_>>();
            bytes[0] = PROTOCOL_VERSION.wrapping_add(1 + (rng.next() % 200) as u8);
            assert!(ServerMessage::decode(&bytes).is_err());
            assert!(ClientMessage::decode(&bytes).is_err());
        }
    }

    /// A small deterministic generator, so the test needs no dependency.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }
}
