//! MQTT 3.1.1 as a subscriber needs it, without IO: the packets a client
//! sends (CONNECT, SUBSCRIBE, PINGREQ, DISCONNECT and PUBACK), the packets
//! a server sends decoded from a byte stream that arrives in any pieces
//! (CONNACK, SUBACK, PUBLISH, PINGRESP), and the client's state machine
//! with its keep-alive. Both transports speak it, tungstenite natively and
//! the browser's WebSocket on the page: they hand it bytes and send what it
//! returns. Time is passed in, so tests run it on a fake clock.
//!
//! Only QoS 0 is asked for. A QoS 1 message, which a server should not send
//! after a QoS 0 subscription, is acknowledged anyway; QoS 2 is refused as
//! a protocol error. No TLS, no will, no password: the server is public.

use std::time::Duration;

/// The largest packet accepted: a vessel's message is a few hundred bytes.
pub const MAX_PACKET: usize = 1 << 20;
/// The largest value of MQTT's variable byte integer (four bytes).
pub const MAX_REMAINING_LENGTH: usize = 268_435_455;

/// Why a connection cannot go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MqttError {
    /// The bytes are not MQTT.
    Malformed(&'static str),
    /// A packet longer than [`MAX_PACKET`].
    TooLarge(usize),
    /// The server refused the connection with this CONNACK return code.
    Refused(u8),
    /// The server refused a subscription.
    SubscriptionRefused,
    /// No CONNACK, SUBACK or PINGRESP in time.
    TimedOut(&'static str),
    /// A packet the server must not send in this state.
    Unexpected(&'static str),
}

impl std::fmt::Display for MqttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed MQTT: {what}"),
            Self::TooLarge(len) => write!(f, "an MQTT packet of {len} bytes"),
            Self::Refused(code) => write!(f, "connection refused ({})", refusal(*code)),
            Self::SubscriptionRefused => f.write_str("subscription refused"),
            Self::TimedOut(what) => write!(f, "no {what} in time"),
            Self::Unexpected(what) => write!(f, "unexpected {what}"),
        }
    }
}

/// CONNACK's return codes (MQTT 3.1.1, 3.2.2.3).
fn refusal(code: u8) -> &'static str {
    match code {
        1 => "protocol version",
        2 => "client identifier",
        3 => "server unavailable",
        4 => "user name or password",
        5 => "not authorised",
        _ => "unknown reason",
    }
}

/// Appends `len` as MQTT's variable byte integer: seven bits a byte, the
/// high bit set while more follow, at most four bytes.
pub fn encode_remaining_length(mut len: usize, out: &mut Vec<u8>) {
    assert!(len <= MAX_REMAINING_LENGTH, "{len} is over MQTT's limit");
    loop {
        let byte = (len % 128) as u8;
        len /= 128;
        if len == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reads a variable byte integer from the start of `bytes`: the value and
/// the bytes it took, `None` while it is incomplete, an error after a
/// fourth byte that says more follow.
pub fn decode_remaining_length(bytes: &[u8]) -> Result<Option<(usize, usize)>, MqttError> {
    let mut value = 0usize;
    for (ix, &byte) in bytes.iter().enumerate().take(4) {
        value += usize::from(byte & 0x7f) << (7 * ix);
        if byte & 0x80 == 0 {
            return Ok(Some((value, ix + 1)));
        }
    }
    if bytes.len() >= 4 {
        return Err(MqttError::Malformed("remaining length over four bytes"));
    }
    Ok(None)
}

/// A packet the client sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing<'a> {
    Connect {
        client_id: &'a str,
        keep_alive: Duration,
    },
    Subscribe {
        packet_id: u16,
        filters: &'a [&'a str],
    },
    PubAck {
        packet_id: u16,
    },
    PingReq,
    Disconnect,
}

impl Outgoing<'_> {
    /// The packet's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let header = match self {
            Self::Connect {
                client_id,
                keep_alive,
            } => {
                push_str(&mut body, "MQTT");
                // Protocol level 4 (3.1.1); flags: a clean session only.
                body.extend_from_slice(&[4, 0x02]);
                let seconds = keep_alive.as_secs().min(u64::from(u16::MAX)) as u16;
                body.extend_from_slice(&seconds.to_be_bytes());
                push_str(&mut body, client_id);
                0x10
            }
            Self::Subscribe { packet_id, filters } => {
                body.extend_from_slice(&packet_id.to_be_bytes());
                for filter in *filters {
                    push_str(&mut body, filter);
                    body.push(0); // QoS 0
                }
                0x82
            }
            Self::PubAck { packet_id } => {
                body.extend_from_slice(&packet_id.to_be_bytes());
                0x40
            }
            Self::PingReq => 0xc0,
            Self::Disconnect => 0xe0,
        };
        let mut out = Vec::with_capacity(body.len() + 5);
        out.push(header);
        encode_remaining_length(body.len(), &mut out);
        out.extend_from_slice(&body);
        out
    }
}

/// A UTF-8 string with its two-byte length.
fn push_str(out: &mut Vec<u8>, text: &str) {
    let len = text.len().min(usize::from(u16::MAX));
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&text.as_bytes()[..len]);
}

/// An application message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publish {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    /// Present for QoS 1 and 2.
    pub packet_id: Option<u16>,
    pub retain: bool,
}

/// A packet the server sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    ConnAck {
        session_present: bool,
        code: u8,
    },
    SubAck {
        packet_id: u16,
        codes: Vec<u8>,
    },
    Publish(Publish),
    PingResp,
    /// Any other packet type, by its four high bits.
    Other(u8),
}

impl Packet {
    /// Decodes one packet of `kind` (the fixed header's first byte) from
    /// its `body`.
    fn decode(kind: u8, body: &[u8]) -> Result<Self, MqttError> {
        match kind >> 4 {
            2 => match body {
                [flags, code] => Ok(Self::ConnAck {
                    session_present: flags & 1 == 1,
                    code: *code,
                }),
                _ => Err(MqttError::Malformed("CONNACK length")),
            },
            9 => {
                if body.len() < 3 {
                    return Err(MqttError::Malformed("SUBACK length"));
                }
                Ok(Self::SubAck {
                    packet_id: u16::from_be_bytes([body[0], body[1]]),
                    codes: body[2..].to_vec(),
                })
            }
            3 => {
                let qos = (kind >> 1) & 0x03;
                if qos == 3 {
                    return Err(MqttError::Malformed("PUBLISH QoS 3"));
                }
                let (topic, mut rest) = read_str(body)?;
                let packet_id = if qos > 0 {
                    let [high, low, tail @ ..] = rest else {
                        return Err(MqttError::Malformed("PUBLISH packet id"));
                    };
                    rest = tail;
                    Some(u16::from_be_bytes([*high, *low]))
                } else {
                    None
                };
                Ok(Self::Publish(Publish {
                    topic: topic.to_owned(),
                    payload: rest.to_vec(),
                    qos,
                    packet_id,
                    retain: kind & 1 == 1,
                }))
            }
            13 if body.is_empty() => Ok(Self::PingResp),
            13 => Err(MqttError::Malformed("PINGRESP length")),
            other => Ok(Self::Other(other)),
        }
    }
}

/// A UTF-8 string with its two-byte length, and what follows it.
fn read_str(bytes: &[u8]) -> Result<(&str, &[u8]), MqttError> {
    let [high, low, rest @ ..] = bytes else {
        return Err(MqttError::Malformed("string length"));
    };
    let len = usize::from(u16::from_be_bytes([*high, *low]));
    if rest.len() < len {
        return Err(MqttError::Malformed("string past the packet"));
    }
    let text = std::str::from_utf8(&rest[..len]).map_err(|_| MqttError::Malformed("UTF-8"))?;
    Ok((text, &rest[len..]))
}

/// Splits a byte stream into packets. Over WebSocket a message may hold
/// several packets or part of one, so the bytes are pushed as they come
/// and packets taken while whole ones are there.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    /// Where the next packet starts in `buffer`.
    start: usize,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        // Drop what was taken before growing, so the buffer stays small.
        if self.start > 0 && self.start * 2 >= self.buffer.len() {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole packet, `None` until one is there.
    pub fn next_packet(&mut self) -> Result<Option<Packet>, MqttError> {
        let pending = &self.buffer[self.start..];
        let Some((&kind, rest)) = pending.split_first() else {
            return Ok(None);
        };
        let Some((len, used)) = decode_remaining_length(rest)? else {
            return Ok(None);
        };
        if len > MAX_PACKET {
            return Err(MqttError::TooLarge(len));
        }
        let Some(body) = rest.get(used..used + len) else {
            return Ok(None);
        };
        let packet = Packet::decode(kind, body)?;
        self.start += 1 + used + len;
        Ok(Some(packet))
    }

    /// Bytes received but not yet a whole packet.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.buffer.len() - self.start
    }
}

/// Where a connection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    /// The transport is not open yet.
    Idle,
    /// CONNECT sent, waiting for CONNACK.
    Connecting,
    /// SUBSCRIBE sent, waiting for SUBACK.
    Subscribing,
    /// Subscribed: messages flow.
    Live,
    /// DISCONNECT sent, or the connection failed.
    Closed,
}

/// How long the server may take to answer CONNECT, SUBSCRIBE or PINGREQ.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// One connection's MQTT client: what to send when the transport opens,
/// what the server's bytes mean, and when to ping. A new connection is a
/// new client.
#[derive(Debug)]
pub struct Client {
    client_id: String,
    keep_alive: Duration,
    filters: Vec<String>,
    state: ClientState,
    decoder: Decoder,
    /// When the client last sent a packet, and when it asked for an answer
    /// it has not had.
    last_sent: Duration,
    awaiting: Option<(Duration, &'static str)>,
    subscribe_id: u16,
}

impl Client {
    /// A client that subscribes to `filters` once connected, and pings
    /// within `keep_alive` of its last packet.
    pub fn new(client_id: impl Into<String>, keep_alive: Duration, filters: &[&str]) -> Self {
        Self {
            client_id: client_id.into(),
            keep_alive,
            filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
            state: ClientState::Idle,
            decoder: Decoder::default(),
            last_sent: Duration::ZERO,
            awaiting: None,
            subscribe_id: 1,
        }
    }

    pub fn state(&self) -> ClientState {
        self.state
    }

    /// The transport opened at `now`: CONNECT goes to `out`.
    pub fn on_open(&mut self, now: Duration, out: &mut Vec<Vec<u8>>) {
        self.send(
            Outgoing::Connect {
                client_id: &self.client_id,
                keep_alive: self.keep_alive,
            }
            .encode(),
            now,
            out,
        );
        self.state = ClientState::Connecting;
        self.awaiting = Some((now, "CONNACK"));
    }

    /// The server's `bytes` at `now`: its messages go to `messages`, the
    /// client's answers to `out`.
    pub fn on_bytes(
        &mut self,
        bytes: &[u8],
        now: Duration,
        out: &mut Vec<Vec<u8>>,
        messages: &mut Vec<Publish>,
    ) -> Result<(), MqttError> {
        self.decoder.push(bytes);
        while let Some(packet) = self.decoder.next_packet().inspect_err(|_| self.fail())? {
            self.on_packet(packet, now, out, messages)
                .inspect_err(|_| self.fail())?;
        }
        Ok(())
    }

    fn on_packet(
        &mut self,
        packet: Packet,
        now: Duration,
        out: &mut Vec<Vec<u8>>,
        messages: &mut Vec<Publish>,
    ) -> Result<(), MqttError> {
        match (self.state, packet) {
            (ClientState::Connecting, Packet::ConnAck { code: 0, .. }) => {
                let filters: Vec<&str> = self.filters.iter().map(String::as_str).collect();
                let subscribe = Outgoing::Subscribe {
                    packet_id: self.subscribe_id,
                    filters: &filters,
                }
                .encode();
                self.send(subscribe, now, out);
                self.state = ClientState::Subscribing;
                self.awaiting = Some((now, "SUBACK"));
            }
            (ClientState::Connecting, Packet::ConnAck { code, .. }) => {
                return Err(MqttError::Refused(code));
            }
            (ClientState::Subscribing, Packet::SubAck { packet_id, codes })
                if packet_id == self.subscribe_id =>
            {
                if codes.iter().any(|code| *code & 0x80 != 0) {
                    return Err(MqttError::SubscriptionRefused);
                }
                self.state = ClientState::Live;
                self.awaiting = None;
            }
            (ClientState::Subscribing | ClientState::Live, Packet::Publish(publish)) => {
                match (publish.qos, publish.packet_id) {
                    (0, _) => {}
                    (1, Some(packet_id)) => {
                        self.send(Outgoing::PubAck { packet_id }.encode(), now, out)
                    }
                    _ => return Err(MqttError::Unexpected("QoS 2 message")),
                }
                messages.push(publish);
            }
            (_, Packet::PingResp) => {
                if matches!(self.awaiting, Some((_, "PINGRESP"))) {
                    self.awaiting = None;
                }
            }
            (_, Packet::ConnAck { .. }) => return Err(MqttError::Unexpected("CONNACK")),
            (_, Packet::SubAck { .. }) => return Err(MqttError::Unexpected("SUBACK")),
            (_, Packet::Publish(_)) => return Err(MqttError::Unexpected("PUBLISH")),
            (_, Packet::Other(_)) => {}
        }
        Ok(())
    }

    /// Time passes: a PINGREQ goes to `out` when the keep-alive is due; an
    /// answer that did not come in time is an error.
    pub fn on_tick(&mut self, now: Duration, out: &mut Vec<Vec<u8>>) -> Result<(), MqttError> {
        if let Some((since, what)) = self.awaiting
            && now.saturating_sub(since) > ANSWER_TIMEOUT
        {
            self.fail();
            return Err(MqttError::TimedOut(what));
        }
        if self.state == ClientState::Live
            && self.awaiting.is_none()
            && now.saturating_sub(self.last_sent) >= self.keep_alive.mul_f32(0.75)
        {
            self.send(Outgoing::PingReq.encode(), now, out);
            self.awaiting = Some((now, "PINGRESP"));
        }
        Ok(())
    }

    /// Says goodbye: DISCONNECT goes to `out` if the server knows the
    /// client.
    pub fn disconnect(&mut self, now: Duration, out: &mut Vec<Vec<u8>>) {
        if matches!(
            self.state,
            ClientState::Connecting | ClientState::Subscribing | ClientState::Live
        ) {
            self.send(Outgoing::Disconnect.encode(), now, out);
        }
        self.state = ClientState::Closed;
        self.awaiting = None;
    }

    fn send(&mut self, packet: Vec<u8>, now: Duration, out: &mut Vec<Vec<u8>>) {
        out.push(packet);
        self.last_sent = now;
    }

    fn fail(&mut self) {
        self.state = ClientState::Closed;
        self.awaiting = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn test_remaining_length_round_trips_at_its_edges() {
        for (len, encoded) in [
            (0, vec![0x00]),
            (127, vec![0x7f]),
            (128, vec![0x80, 0x01]),
            (16_383, vec![0xff, 0x7f]),
            (16_384, vec![0x80, 0x80, 0x01]),
            (2_097_151, vec![0xff, 0xff, 0x7f]),
            (2_097_152, vec![0x80, 0x80, 0x80, 0x01]),
            (MAX_REMAINING_LENGTH, vec![0xff, 0xff, 0xff, 0x7f]),
        ] {
            let mut out = Vec::new();
            encode_remaining_length(len, &mut out);
            assert_eq!(out, encoded, "{len}");
            assert_eq!(
                decode_remaining_length(&encoded),
                Ok(Some((len, encoded.len())))
            );
            // Every proper prefix is incomplete, not wrong.
            for cut in 0..encoded.len() {
                assert_eq!(decode_remaining_length(&encoded[..cut]), Ok(None));
            }
        }
        assert!(decode_remaining_length(&[0xff, 0xff, 0xff, 0xff, 0x01]).is_err());
        assert!(decode_remaining_length(&[0x80, 0x80, 0x80, 0x80]).is_err());
    }

    #[test]
    fn test_outgoing_packets_are_mqtt_3_1_1() {
        let connect = Outgoing::Connect {
            client_id: "ab",
            keep_alive: S(30),
        }
        .encode();
        assert_eq!(
            connect,
            [
                0x10, 14, 0, 4, b'M', b'Q', b'T', b'T', 4, 0x02, 0, 30, 0, 2, b'a', b'b'
            ]
        );
        let subscribe = Outgoing::Subscribe {
            packet_id: 7,
            filters: &["a/#"],
        }
        .encode();
        assert_eq!(subscribe, [0x82, 8, 0, 7, 0, 3, b'a', b'/', b'#', 0]);
        assert_eq!(Outgoing::PingReq.encode(), [0xc0, 0]);
        assert_eq!(Outgoing::Disconnect.encode(), [0xe0, 0]);
        assert_eq!(
            Outgoing::PubAck { packet_id: 258 }.encode(),
            [0x40, 2, 1, 2]
        );
    }

    /// A PUBLISH as a server sends it.
    fn publish(topic: &str, payload: &[u8], qos: u8, packet_id: u16) -> Vec<u8> {
        let mut body = Vec::new();
        push_str(&mut body, topic);
        if qos > 0 {
            body.extend_from_slice(&packet_id.to_be_bytes());
        }
        body.extend_from_slice(payload);
        let mut out = vec![0x30 | (qos << 1)];
        encode_remaining_length(body.len(), &mut out);
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn test_decoder_takes_packets_in_any_pieces() {
        let long = vec![b'x'; 300]; // a two-byte remaining length
        let mut stream = publish("vessels-v2/1/location", b"{}", 0, 0);
        stream.extend(publish("vessels-v2/2/metadata", &long, 0, 0));
        stream.extend([0xd0, 0x00]);
        for piece in [1, 2, 3, 7, 64, stream.len()] {
            let mut decoder = Decoder::default();
            let mut packets = Vec::new();
            for chunk in stream.chunks(piece) {
                decoder.push(chunk);
                while let Some(packet) = decoder.next_packet().expect("valid") {
                    packets.push(packet);
                }
            }
            assert_eq!(packets.len(), 3, "pieces of {piece}");
            let Packet::Publish(second) = &packets[1] else {
                panic!("a PUBLISH");
            };
            assert_eq!(second.topic, "vessels-v2/2/metadata");
            assert_eq!(second.payload.len(), 300);
            assert_eq!(packets[2], Packet::PingResp);
            assert_eq!(decoder.pending(), 0);
        }
    }

    #[test]
    fn test_decoder_refuses_what_is_not_mqtt() {
        let mut decoder = Decoder::default();
        decoder.push(&[0x30, 0xff, 0xff, 0xff, 0xff, 0x01]);
        assert!(matches!(
            decoder.next_packet(),
            Err(MqttError::Malformed(_))
        ));
        let mut decoder = Decoder::default();
        let mut huge = vec![0x30];
        encode_remaining_length(MAX_PACKET + 1, &mut huge);
        decoder.push(&huge);
        assert_eq!(
            decoder.next_packet(),
            Err(MqttError::TooLarge(MAX_PACKET + 1))
        );
        let mut decoder = Decoder::default();
        // A topic longer than the packet.
        decoder.push(&[0x30, 3, 0, 9, b'a']);
        assert!(decoder.next_packet().is_err());
    }

    /// A client through CONNECT, CONNACK, SUBSCRIBE and SUBACK at `now`.
    fn live_client(now: Duration) -> Client {
        let mut client = Client::new("ships; 1", S(30), &["vessels-v2/#"]);
        let mut out = Vec::new();
        client.on_open(now, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0], 0x10);
        out.clear();
        let mut messages = Vec::new();
        client
            .on_bytes(&[0x20, 2, 0, 0], now, &mut out, &mut messages)
            .expect("CONNACK");
        assert_eq!(client.state(), ClientState::Subscribing);
        assert_eq!(out[0][0], 0x82);
        client
            .on_bytes(&[0x90, 3, 0, 1, 0], now, &mut out, &mut messages)
            .expect("SUBACK");
        assert_eq!(client.state(), ClientState::Live);
        client
    }

    #[test]
    fn test_client_connects_subscribes_and_receives() {
        let mut client = live_client(S(0));
        let (mut out, mut messages) = (Vec::new(), Vec::new());
        let mut bytes = publish("vessels-v2/230/location", b"{\"sog\":1}", 0, 0);
        bytes.extend(publish("vessels-v2/231/location", b"{}", 1, 9));
        client
            .on_bytes(&bytes, S(1), &mut out, &mut messages)
            .expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].payload, b"{\"sog\":1}");
        // The QoS 1 message is acknowledged.
        assert_eq!(out, vec![vec![0x40, 2, 0, 9]]);
        // QoS 2 is a protocol error here.
        let mut client = live_client(S(0));
        let error = client.on_bytes(&publish("t", b"", 2, 1), S(1), &mut out, &mut messages);
        assert!(error.is_err());
        assert_eq!(client.state(), ClientState::Closed);
    }

    #[test]
    fn test_client_reports_refusals() {
        let mut client = Client::new("x", S(30), &["a"]);
        let (mut out, mut messages) = (Vec::new(), Vec::new());
        client.on_open(S(0), &mut out);
        assert_eq!(
            client.on_bytes(&[0x20, 2, 0, 5], S(0), &mut out, &mut messages),
            Err(MqttError::Refused(5))
        );
        let mut client = Client::new("x", S(30), &["a"]);
        client.on_open(S(0), &mut out);
        client
            .on_bytes(&[0x20, 2, 0, 0], S(0), &mut out, &mut messages)
            .expect("CONNACK");
        assert_eq!(
            client.on_bytes(&[0x90, 3, 0, 1, 0x80], S(0), &mut out, &mut messages),
            Err(MqttError::SubscriptionRefused)
        );
        // A PUBLISH before the connection is accepted.
        let mut client = Client::new("x", S(30), &["a"]);
        client.on_open(S(0), &mut out);
        assert!(
            client
                .on_bytes(&publish("t", b"", 0, 0), S(0), &mut out, &mut messages)
                .is_err()
        );
    }

    #[test]
    fn test_keep_alive_pings_and_times_out() {
        let mut client = live_client(S(0));
        let mut out = Vec::new();
        // Nothing to send before three quarters of the keep-alive.
        client.on_tick(S(22), &mut out).expect("quiet");
        assert!(out.is_empty());
        client.on_tick(S(23), &mut out).expect("ping");
        assert_eq!(out, vec![vec![0xc0, 0]]);
        out.clear();
        // An answer in time clears the wait, and the next ping comes a
        // keep-alive's three quarters later.
        let mut messages = Vec::new();
        client
            .on_bytes(&[0xd0, 0], S(24), &mut out, &mut messages)
            .expect("PINGRESP");
        client.on_tick(S(40), &mut out).expect("quiet");
        assert!(out.is_empty());
        client.on_tick(S(46), &mut out).expect("ping");
        assert_eq!(out.len(), 1);
        // No answer within the timeout: the connection is dead.
        assert!(client.on_tick(S(46) + ANSWER_TIMEOUT, &mut out).is_ok());
        assert_eq!(
            client.on_tick(S(47) + ANSWER_TIMEOUT, &mut out),
            Err(MqttError::TimedOut("PINGRESP"))
        );
        assert_eq!(client.state(), ClientState::Closed);
    }

    #[test]
    fn test_connack_must_come_in_time() {
        let mut client = Client::new("x", S(30), &["a"]);
        let mut out = Vec::new();
        client.on_open(S(10), &mut out);
        assert!(client.on_tick(S(25), &mut out).is_ok());
        assert_eq!(
            client.on_tick(S(26), &mut out),
            Err(MqttError::TimedOut("CONNACK"))
        );
    }

    #[test]
    fn test_disconnect_says_goodbye_once_connected() {
        let mut client = live_client(S(0));
        let mut out = Vec::new();
        client.disconnect(S(1), &mut out);
        assert_eq!(out, vec![vec![0xe0, 0]]);
        assert_eq!(client.state(), ClientState::Closed);
        let mut idle = Client::new("x", S(30), &["a"]);
        out.clear();
        idle.disconnect(S(1), &mut out);
        assert!(out.is_empty());
    }
}
