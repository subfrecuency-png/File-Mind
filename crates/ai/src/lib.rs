//! AI adapters. Local processing is the default; every adapter call is
//! minimal (filename + ≤ 2 KB snippet) and audited.
//!
//! Phase 4 adds the ONNX classifier, Phase 7 the ONNX embedder (`ort`) and
//! the Ollama / cloud HTTP adapters.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Hard cap on text sent to any adapter, per call.
pub const MAX_SNIPPET_BYTES: usize = 2048;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub file_name: String,
    pub snippet: String,
    pub purpose: Purpose,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Classify,
    ProjectName,
    ParseQuery,
    Summarize,
}

impl Payload {
    /// Build a payload, truncating the snippet at a UTF-8 boundary ≤ [`MAX_SNIPPET_BYTES`].
    pub fn new(file_name: impl Into<String>, text: &str, purpose: Purpose) -> Self {
        let mut end = text.len().min(MAX_SNIPPET_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            file_name: file_name.into(),
            snippet: text[..end].to_string(),
            purpose,
        }
    }
}

pub trait AiAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_local(&self) -> bool;
    fn complete(&self, payload: &Payload, instruction: &str) -> Result<String>;
}

/// The default: no adapter configured. Every call is a no-op error the caller handles.
pub struct NoneAdapter;

impl AiAdapter for NoneAdapter {
    fn name(&self) -> &'static str {
        "none"
    }
    fn is_local(&self) -> bool {
        true
    }
    fn complete(&self, _: &Payload, _: &str) -> Result<String> {
        anyhow::bail!("no AI adapter configured")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_capped() {
        let big = "é".repeat(5000);
        let p = Payload::new("x.txt", &big, Purpose::Classify);
        assert!(p.snippet.len() <= MAX_SNIPPET_BYTES);
        assert!(std::str::from_utf8(p.snippet.as_bytes()).is_ok());
    }
}
