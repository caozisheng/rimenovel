use crate::llm::LlmError;

/// Decode SSE only after complete byte-delimited lines arrive, so UTF-8 codepoints
/// may span any number of HTTP chunks. Data fields are joined per SSE framing.
#[derive(Default)]
pub(super) struct Decoder {
    line: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
    skip_lf: bool,
}

impl Decoder {
    /// The consumer returns true only for a terminal, successfully handled event.
    pub(super) fn feed(
        &mut self,
        bytes: &[u8],
        mut consume: impl FnMut(&str, &str) -> Result<bool, LlmError>,
    ) -> Result<bool, LlmError> {
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte != b'\r' && byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            self.skip_lf = byte == b'\r';
            let line = std::str::from_utf8(&self.line)
                .map_err(|e| LlmError::Stream(format!("invalid SSE UTF-8: {e}")))?;
            if line.is_empty() {
                let complete = self.has_data && consume(&self.event, &self.data)?;
                self.event.clear();
                self.data.clear();
                self.has_data = false;
                if complete {
                    return Ok(true);
                }
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => {
                        self.event.clear();
                        self.event.push_str(value);
                    }
                    "data" => {
                        if self.has_data {
                            self.data.push('\n');
                        }
                        self.data.push_str(value);
                        self.has_data = true;
                    }
                    _ => {}
                }
            }
            self.line.clear();
        }
        Ok(false)
    }

    pub(super) fn finish(&self) -> Result<(), LlmError> {
        if !self.line.is_empty() || self.has_data || !self.event.is_empty() {
            return Err(LlmError::Stream("incomplete SSE event at end of stream".into()));
        }
        Ok(())
    }
}
