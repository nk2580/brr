use std::{error::Error, fmt};

pub(super) const MAX_SSE_FRAME_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SseFrame {
    pub event: String,
    pub data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SseDecodeError {
    FrameTooLarge,
    InvalidUtf8,
}

impl fmt::Display for SseDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge => write!(formatter, "SSE frame exceeds 32 MiB"),
            Self::InvalidUtf8 => write!(formatter, "SSE frame contains invalid UTF-8"),
        }
    }
}

impl Error for SseDecodeError {}

#[derive(Default)]
pub(super) struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data_lines: Vec<String>,
    frame_bytes: usize,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, SseDecodeError> {
        let mut frames = Vec::new();
        for byte in chunk {
            self.frame_bytes = self
                .frame_bytes
                .checked_add(1)
                .filter(|size| *size <= MAX_SSE_FRAME_BYTES)
                .ok_or(SseDecodeError::FrameTooLarge)?;
            if *byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                let line =
                    std::str::from_utf8(&self.line).map_err(|_| SseDecodeError::InvalidUtf8)?;
                if line.is_empty() {
                    if !self.data_lines.is_empty() {
                        frames.push(SseFrame {
                            event: self.event.take().unwrap_or_else(|| "message".into()),
                            data: self.data_lines.join("\n"),
                        });
                    } else {
                        self.event = None;
                    }
                    self.data_lines.clear();
                    self.frame_bytes = 0;
                } else if !line.starts_with(':') {
                    if let Some(value) = line.strip_prefix("event:") {
                        self.event = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
                    } else if let Some(value) = line.strip_prefix("data:") {
                        self.data_lines
                            .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
                    }
                }
                self.line.clear();
            } else {
                self.line.push(*byte);
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::{SseDecodeError, SseDecoder, MAX_SSE_FRAME_BYTES};

    #[test]
    fn preserves_utf8_codepoints_split_across_chunks() {
        let frame = "event: message\ndata: {\"delta\":\"hello 😀\"}\n\n".as_bytes();
        let emoji = frame
            .windows(4)
            .position(|window| window == "😀".as_bytes())
            .unwrap();
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&frame[..emoji + 2]).unwrap().is_empty());
        let decoded = decoder.push(&frame[emoji + 2..]).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].event, "message");
        assert_eq!(decoded[0].data, r#"{"delta":"hello 😀"}"#);
    }

    #[test]
    fn rejects_invalid_utf8_at_a_complete_line() {
        let mut decoder = SseDecoder::default();
        assert_eq!(
            decoder.push(b"data: \xff\n"),
            Err(SseDecodeError::InvalidUtf8)
        );
    }

    #[test]
    fn rejects_an_oversized_unterminated_frame() {
        let mut decoder = SseDecoder::default();
        let chunk = vec![b'x'; MAX_SSE_FRAME_BYTES + 1];
        assert_eq!(decoder.push(&chunk), Err(SseDecodeError::FrameTooLarge));
    }
}
