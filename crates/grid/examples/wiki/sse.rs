//! A parser of the Server-Sent Events format, as the HTML standard
//! interprets an event stream ("9.2.6 Interpreting an event stream"), fed
//! bytes as they arrive: a chunk may end anywhere, even inside a UTF-8
//! character or between the CR and the LF of a line end. No I/O; the
//! native client (`native.rs`) feeds it what it reads. The browser parses
//! the stream itself (`EventSource`).
//!
//! - Lines end with CRLF, LF or CR; a byte-order mark at the start is
//!   skipped.
//! - `data:` lines append to the event's data, joined with LF; an empty
//!   line dispatches the event, if it has data.
//! - `event:` names the event's type (`message` without one).
//! - `id:` sets the last event ID, which every later event carries and
//!   a reconnect sends as `Last-Event-ID`; an ID with a NUL is ignored.
//! - `retry:` with digits only sets the reconnection time, in ms.
//! - A line starting with `:` is a comment; other fields are ignored.
//! - An event the stream ends in the middle of is dropped.

use std::time::Duration;

/// One dispatched event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// `message` unless the stream named another type.
    pub kind: String,
    pub data: String,
    /// The last event ID when it was dispatched, empty if none.
    pub id: String,
}

/// What the parser found in a chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SseItem {
    Event(SseEvent),
    /// The server's reconnection time.
    Retry(Duration),
}

/// The parser's state between chunks.
#[derive(Debug, Default)]
pub struct SseParser {
    /// The bytes of a line not ended yet.
    line: Vec<u8>,
    data: String,
    kind: String,
    last_id: String,
    /// Whether the first line was seen (the byte-order mark is checked
    /// there).
    started: bool,
    /// The last chunk ended with CR: an LF at the start of the next one
    /// ends nothing.
    after_cr: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last event ID so far: what a reconnect sends as
    /// `Last-Event-ID`; empty if none. (Each event carries it too.)
    #[cfg(test)]
    pub fn last_event_id(&self) -> &str {
        &self.last_id
    }

    /// Parses `bytes`, the next chunk of the stream, appending what it
    /// completes to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<SseItem>) {
        let mut rest = bytes;
        if self.after_cr {
            self.after_cr = false;
            if let Some(stripped) = rest.strip_prefix(b"\n") {
                rest = stripped;
            }
        }
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n' || byte == b'\r') {
            self.line.extend_from_slice(&rest[..end]);
            let line = std::mem::take(&mut self.line);
            self.process(&line, out);
            self.line = line;
            self.line.clear();
            let cr = rest[end] == b'\r';
            rest = &rest[end + 1..];
            if cr {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    Some(_) => {}
                    None => self.after_cr = true,
                }
            }
        }
        self.line.extend_from_slice(rest);
    }

    fn process(&mut self, line: &[u8], out: &mut Vec<SseItem>) {
        let mut line = line;
        if !self.started {
            self.started = true;
            if let Some(stripped) = line.strip_prefix("\u{feff}".as_bytes()) {
                line = stripped;
            }
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line[0] == b':' {
            return;
        }
        let (field, value) = match line.iter().position(|&byte| byte == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &[][..]),
        };
        let value = String::from_utf8_lossy(value);
        match field {
            b"data" => {
                self.data.push_str(&value);
                self.data.push('\n');
            }
            b"event" => self.kind = value.into_owned(),
            b"id" if !value.contains('\0') => self.last_id = value.into_owned(),
            b"retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                if let Ok(ms) = value.parse::<u64>() {
                    out.push(SseItem::Retry(Duration::from_millis(ms)));
                }
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseItem>) {
        let kind = std::mem::take(&mut self.kind);
        if self.data.is_empty() {
            return;
        }
        let mut data = std::mem::take(&mut self.data);
        data.pop();
        out.push(SseItem::Event(SseEvent {
            kind: if kind.is_empty() {
                "message".into()
            } else {
                kind
            },
            data,
            id: self.last_id.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> (Vec<SseItem>, SseParser) {
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut out);
        }
        (out, parser)
    }

    fn event(kind: &str, data: &str, id: &str) -> SseItem {
        SseItem::Event(SseEvent {
            kind: kind.into(),
            data: data.into(),
            id: id.into(),
        })
    }

    #[test]
    fn test_multi_line_data_joins_with_line_feeds() {
        let (out, _) = parse(&[b"data: {\"a\":\ndata:  1}\ndata\n\n"]);
        // One space after the colon is dropped, the second kept; a bare
        // `data` adds an empty line.
        assert_eq!(out, [event("message", "{\"a\":\n 1}\n", "")]);
    }

    #[test]
    fn test_ids_carry_over_and_reset() {
        let (out, parser) = parse(&[
            b"id: [{\"t\":1}]\ndata: a\n\ndata: b\n\nid\ndata: c\n\nid: x\0y\ndata: d\n\n",
        ]);
        assert_eq!(
            out,
            [
                event("message", "a", "[{\"t\":1}]"),
                event("message", "b", "[{\"t\":1}]"),
                event("message", "c", ""),
                // An ID with a NUL is ignored.
                event("message", "d", ""),
            ]
        );
        assert_eq!(parser.last_event_id(), "");
        let (_, parser) = parse(&[b"id: 42\n\n"]);
        assert_eq!(
            parser.last_event_id(),
            "42",
            "an ID without data still counts"
        );
    }

    #[test]
    fn test_retry_takes_digits_only() {
        let (out, _) = parse(&[b"retry: 2500\nretry: 1s\nretry:\nretry: -1\n"]);
        assert_eq!(out, [SseItem::Retry(Duration::from_millis(2_500))]);
    }

    #[test]
    fn test_comments_types_and_unknown_fields() {
        let (out, _) = parse(&[
            b": ok\n:\nevent: error\nfoo: bar\ndata: boom\n\n",
            b"event: x\n\ndata: plain\n\n",
        ]);
        // An event without data dispatches nothing and its type is reset.
        assert_eq!(
            out,
            [event("error", "boom", ""), event("message", "plain", "")]
        );
    }

    #[test]
    fn test_line_ends_and_byte_order_mark() {
        let crlf = parse(&["\u{feff}data: a\r\n\r\n".as_bytes()]).0;
        let cr = parse(&[b"data: a\r\rdata: b\r\r"]).0;
        assert_eq!(crlf, [event("message", "a", "")]);
        assert_eq!(cr, [event("message", "a", ""), event("message", "b", "")]);
    }

    #[test]
    fn test_any_split_of_the_stream_parses_the_same() {
        let stream =
            "id: 7\r\nevent: message\r\ndata: {\"title\":\"Zürich – 東京\"}\r\n\r\n: keep\n\
                      retry: 100\ndata: second\rdata: line\r\r"
                .as_bytes();
        let (whole, _) = parse(&[stream]);
        assert_eq!(whole.len(), 3);
        for at in 0..=stream.len() {
            let (split, _) = parse(&[&stream[..at], &stream[at..]]);
            assert_eq!(split, whole, "split at {at}");
        }
        // Byte by byte: a CR and its LF, and a UTF-8 character, in
        // different chunks.
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(parse(&bytes).0, whole);
    }

    #[test]
    fn test_an_unfinished_event_is_not_dispatched() {
        let (out, _) = parse(&[b"data: a\n\ndata: half"]);
        assert_eq!(out, [event("message", "a", "")]);
        let (out, _) = parse(&[b"data: b\n"]);
        assert!(out.is_empty(), "no empty line yet");
    }
}
