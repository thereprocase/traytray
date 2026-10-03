//! Newline-delimited JSON framing (ADR 0001).
//!
//! The decoder is a pure state machine fed with byte chunks, so the same code serves Unix
//! sockets, named pipes and TCP, and can be tested without I/O. It never buffers more than
//! `MAX_FRAME_BYTES`: an oversized line is reported once and then skipped up to its newline.

use crate::limits::MAX_FRAME_BYTES;

#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    /// One complete line, without its trailing newline (a trailing `\r` is also removed).
    Line(Vec<u8>),
    /// A line exceeded the limit. Its bytes were dropped; decoding resumes after its newline.
    Oversized,
}

#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// True while skipping the rest of an oversized line.
    discarding: bool,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every frame completed by them, in order.
    pub fn push(&mut self, mut bytes: &[u8]) -> Vec<Decoded> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            let (chunk, rest, complete) = match newline {
                Some(i) => (&bytes[..i], &bytes[i + 1..], true),
                None => (bytes, &[][..], false),
            };
            bytes = rest;

            if self.discarding {
                if complete {
                    self.discarding = false;
                }
                continue;
            }
            // +1 accounts for the newline, which counts toward the frame size.
            if self.buf.len() + chunk.len() + usize::from(complete) > MAX_FRAME_BYTES {
                self.buf.clear();
                self.buf.shrink_to(4096);
                out.push(Decoded::Oversized);
                self.discarding = !complete;
                continue;
            }
            self.buf.extend_from_slice(chunk);
            if complete {
                let mut line = std::mem::take(&mut self.buf);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !line.is_empty() {
                    out.push(Decoded::Line(line));
                }
            }
        }
        out
    }

    /// Bytes currently held for an incomplete line.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// Serialise one frame as a single line. serde_json escapes newlines inside strings, so the
/// output never contains a raw newline except the terminator.
pub fn encode<T: serde::Serialize>(frame: &T) -> serde_json::Result<Vec<u8>> {
    let mut v = serde_json::to_vec(frame)?;
    v.push(b'\n');
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(d: Vec<Decoded>) -> Vec<String> {
        d.into_iter()
            .map(|x| match x {
                Decoded::Line(l) => String::from_utf8(l).unwrap(),
                Decoded::Oversized => "<oversized>".into(),
            })
            .collect()
    }

    #[test]
    fn splits_lines_across_chunks() {
        let mut d = FrameDecoder::new();
        assert!(d.push(b"{\"a\":").is_empty());
        assert_eq!(lines(d.push(b"1}\n{\"b\":2}\n{\"c\"")), vec!["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(lines(d.push(b":3}\r\n")), vec!["{\"c\":3}"]);
    }

    #[test]
    fn ignores_blank_lines() {
        let mut d = FrameDecoder::new();
        assert_eq!(lines(d.push(b"\n\r\n{}\n")), vec!["{}"]);
    }

    #[test]
    fn oversized_line_in_one_chunk_is_dropped_and_next_line_survives() {
        let mut d = FrameDecoder::new();
        let mut big = vec![b'x'; MAX_FRAME_BYTES + 10];
        big.extend_from_slice(b"\n{\"ok\":1}\n");
        assert_eq!(lines(d.push(&big)), vec!["<oversized>", "{\"ok\":1}"]);
    }

    #[test]
    fn oversized_line_streamed_is_never_buffered_past_the_limit() {
        let mut d = FrameDecoder::new();
        let chunk = vec![b'x'; 64 * 1024];
        let mut reported = 0;
        for _ in 0..20 {
            for f in d.push(&chunk) {
                assert_eq!(f, Decoded::Oversized);
                reported += 1;
            }
            assert!(d.buffered() <= MAX_FRAME_BYTES);
        }
        assert_eq!(reported, 1, "an oversized line is reported once");
        assert_eq!(lines(d.push(b"tail\n{\"next\":1}\n")), vec!["{\"next\":1}"]);
    }

    #[test]
    fn exact_limit_is_accepted_one_more_is_not() {
        let mut d = FrameDecoder::new();
        let mut ok = vec![b'y'; MAX_FRAME_BYTES - 1];
        ok.push(b'\n');
        assert!(matches!(d.push(&ok)[..], [Decoded::Line(_)]));
        let mut too_big = vec![b'y'; MAX_FRAME_BYTES];
        too_big.push(b'\n');
        assert_eq!(d.push(&too_big), vec![Decoded::Oversized]);
    }

    #[test]
    fn encode_never_emits_inner_newlines() {
        let v = encode(&serde_json::json!({"t": "a\nb"})).unwrap();
        assert_eq!(v.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(*v.last().unwrap(), b'\n');
    }
}
