//! Incremental Server-Sent Events decoder.
//!
//! Handles the subset every provider uses: `event:` and `data:` fields,
//! comment lines (`: heartbeat`), `\n` or `\r\n` line endings, and frames
//! split across arbitrary network chunks.

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// Empty when the frame had no `event:` field.
    pub event: String,
    pub data: String,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<SseEvent>) {
        self.buf.extend_from_slice(chunk);
        let Self {
            buf,
            event,
            data,
            has_data,
        } = self;
        let mut start = 0;
        while let Some(pos) = buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + pos;
            apply_line(strip_cr(&buf[start..end]), event, data, has_data, out);
            start = end + 1;
        }
        buf.drain(..start);
    }

    /// Flushes a trailing frame that was not terminated by a blank line.
    pub fn finish(&mut self, out: &mut Vec<SseEvent>) {
        let Self {
            buf,
            event,
            data,
            has_data,
        } = self;
        if !buf.is_empty() {
            apply_line(strip_cr(buf), event, data, has_data, out);
            buf.clear();
        }
        apply_line(b"", event, data, has_data, out);
    }
}

fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn apply_line(
    line: &[u8],
    event: &mut String,
    data: &mut String,
    has_data: &mut bool,
    out: &mut Vec<SseEvent>,
) {
    if line.is_empty() {
        if *has_data {
            out.push(SseEvent {
                event: std::mem::take(event),
                data: std::mem::take(data),
            });
            *has_data = false;
        } else {
            event.clear();
        }
        return;
    }
    if line[0] == b':' {
        return;
    }
    let (field, value) = match line.iter().position(|&b| b == b':') {
        Some(i) => {
            let v = &line[i + 1..];
            (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
        }
        None => (line, &b""[..]),
    };
    let value = String::from_utf8_lossy(value);
    match field {
        b"data" => {
            if *has_data {
                data.push('\n');
            }
            data.push_str(&value);
            *has_data = true;
        }
        b"event" => {
            event.clear();
            event.push_str(&value);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut d = SseDecoder::default();
        let mut out = Vec::new();
        for c in chunks {
            d.push(c, &mut out);
        }
        d.finish(&mut out);
        out
    }

    #[test]
    fn frames_split_mid_line_and_mid_utf8_reassemble() {
        let whole =
            "event: delta\r\ndata: {\"t\":\"héllo\"}\r\n\r\n: heartbeat\n\ndata: [DONE]\n\n";
        let bytes = whole.as_bytes();
        // Split inside the two-byte 'é' and inside the CRLF.
        let cut_a = whole.find('é').unwrap() + 1;
        let cut_b = whole.find("\r\n\r\n").unwrap() + 1;
        let events = decode(&[&bytes[..cut_a], &bytes[cut_a..cut_b], &bytes[cut_b..]]);
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "delta".into(),
                    data: "{\"t\":\"héllo\"}".into()
                },
                SseEvent {
                    event: String::new(),
                    data: "[DONE]".into()
                },
            ]
        );
    }

    #[test]
    fn multi_line_data_joins_with_newline_and_unterminated_tail_flushes() {
        let events = decode(&[b"data: a\ndata: b\n\ndata: tail"]);
        assert_eq!(events[0].data, "a\nb");
        assert_eq!(events[1].data, "tail");
    }

    #[test]
    fn event_name_does_not_leak_into_next_frame() {
        let events = decode(&[b"event: one\ndata: 1\n\ndata: 2\n\n"]);
        assert_eq!(events[0].event, "one");
        assert_eq!(events[1].event, "");
    }
}
