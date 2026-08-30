use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SseError {
    #[error("invalid UTF-8 in SSE stream: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("SSE retry value is too large: {0}")]
    RetryTooLarge(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SseEvent {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    pub data: String,
    #[serde(default)]
    pub retry_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    current_id: Option<String>,
    current_event: Option<String>,
    current_data: Vec<String>,
    current_retry_ms: Option<u64>,
    saw_bom: bool,
}

impl SseParser {
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(String::from_utf8(line)?, &mut events)?;
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = String::from_utf8(std::mem::take(&mut self.buffer))?;
            self.process_line(line, &mut events)?;
        }
        self.dispatch(&mut events);
        Ok(events)
    }

    fn process_line(
        &mut self,
        mut line: String,
        events: &mut Vec<SseEvent>,
    ) -> Result<(), SseError> {
        if !self.saw_bom {
            self.saw_bom = true;
            if line.starts_with('\u{feff}') {
                line = line.trim_start_matches('\u{feff}').to_owned();
            }
        }
        if line.is_empty() {
            self.dispatch(events);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line.as_str(), ""));
        let value = value.strip_prefix(' ').unwrap_or(value).to_owned();
        match field {
            "data" => self.current_data.push(value),
            "event" => self.current_event = Some(value),
            "id" if !value.contains('\0') => self.current_id = Some(value),
            "retry" => {
                if let Ok(retry_ms) = value.parse::<u64>() {
                    self.current_retry_ms = Some(retry_ms);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if self.current_data.is_empty() {
            self.current_event = None;
            self.current_retry_ms = None;
            return;
        }
        events.push(SseEvent {
            id: self.current_id.clone(),
            event: self.current_event.take(),
            data: self.current_data.drain(..).collect::<Vec<_>>().join("\n"),
            retry_ms: self.current_retry_ms.take(),
        });
    }
}

pub fn parse_sse(input: &[u8]) -> Result<Vec<SseEvent>, SseError> {
    let mut parser = SseParser::default();
    let mut events = parser.feed_bytes(input)?;
    events.extend(parser.finish()?);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunked_events_and_preserves_last_event_id() {
        let mut parser = SseParser::default();
        assert!(parser
            .feed_bytes(b"\xef\xbb")
            .expect("partial BOM")
            .is_empty());
        assert!(parser
            .feed_bytes(b"\xbf: stream\r\nid: 7\r\nevent: update\r\ndata: line one\r\n")
            .expect("first chunk")
            .is_empty());
        assert_eq!(
            parser
                .feed_bytes(b"data: line two\r\nretry: 1500\r\n\r\nid: 8\ndata: next\n\n")
                .expect("second chunk"),
            vec![
                SseEvent {
                    id: Some("7".to_owned()),
                    event: Some("update".to_owned()),
                    data: "line one\nline two".to_owned(),
                    retry_ms: Some(1500),
                },
                SseEvent {
                    id: Some("8".to_owned()),
                    event: None,
                    data: "next".to_owned(),
                    retry_ms: None,
                },
            ]
        );
    }

    #[test]
    fn parses_json_data_and_dispatches_a_trailing_event() {
        assert_eq!(
            parse_sse(b"data: {\"ok\":true}\n\n data: ignored\ndata: trailing")
                .expect("SSE")
                .last()
                .expect("trailing event")
                .data,
            "trailing"
        );
    }
}
