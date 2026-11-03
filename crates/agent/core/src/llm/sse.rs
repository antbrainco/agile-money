//! Server-sent events, as both chat wires stream them. Bytes go in as they
//! arrive; whole events come out once their blank line does.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` field; `None` means the default `message`.
    pub event: Option<String>,
    /// `data:` lines joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]);
            self.line(line.strip_suffix('\r').unwrap_or(&line), &mut events);
        }
        events
    }

    /// Flushes an event the stream ended without a blank line after.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            let line = String::from_utf8_lossy(&rest);
            self.line(line.trim_end_matches(['\r', '\n']), &mut events);
        }
        self.line("", &mut events);
        events
    }

    fn line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                events.push(SseEvent {
                    event: self.event.take(),
                    data: std::mem::take(&mut self.data).join("\n"),
                });
            }
            self.event = None;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_and_line_endings() {
        let mut sse = SseParser::default();
        assert!(sse.feed(b"event: ping\r\nda").is_empty());
        let events = sse.feed(b"ta: {\"a\":1}\r\n\r\n: comment\ndata: x\ndata: y\n\n");
        assert_eq!(
            events,
            [
                SseEvent {
                    event: Some("ping".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "x\ny".into()
                },
            ]
        );
        assert_eq!(sse.feed(b"data: [DONE]").len(), 0);
        assert_eq!(sse.finish()[0].data, "[DONE]");
    }
}
