//! Language-model adapters. Ollama (local) is the default when it is
//! running; the cloud adapter is opt-in and must be enabled explicitly.
//! Both receive at most one [`Payload`] (≤ 2 KB of text) per call.

use crate::{AiAdapter, Payload};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    None,
    Ollama,
    Cloud,
}

/// Persisted under settings `ai.*`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub adapter: Kind,
    pub ollama_url: String,
    pub ollama_model: String,
    pub cloud_model: String,
    /// Stored in the settings table (SQLCipher arrives in Phase 9); the
    /// `ANTHROPIC_API_KEY` environment variable is used when this is empty.
    pub cloud_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            adapter: Kind::None,
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "llama3.2".into(),
            cloud_model: "claude-opus-5".into(),
            cloud_key: String::new(),
        }
    }
}

impl Config {
    pub fn build(&self) -> Box<dyn AiAdapter> {
        match self.adapter {
            Kind::None => Box::new(crate::NoneAdapter),
            Kind::Ollama => Box::new(Ollama {
                url: self.ollama_url.clone(),
                model: self.ollama_model.clone(),
            }),
            Kind::Cloud => Box::new(Cloud {
                model: self.cloud_model.clone(),
                key: if self.cloud_key.is_empty() {
                    std::env::var("ANTHROPIC_API_KEY").unwrap_or_default()
                } else {
                    self.cloud_key.clone()
                },
            }),
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into()
}

pub struct Ollama {
    pub url: String,
    pub model: String,
}

impl Ollama {
    /// True when an Ollama server answers on `url`.
    pub fn reachable(url: &str) -> bool {
        let a: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(800)))
            .build()
            .into();
        a.get(format!("{}/api/tags", url.trim_end_matches('/')))
            .call()
            .is_ok()
    }

    pub fn models(url: &str) -> Result<Vec<String>> {
        let v: serde_json::Value = agent()
            .get(format!("{}/api/tags", url.trim_end_matches('/')))
            .call()?
            .body_mut()
            .read_json()?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl AiAdapter for Ollama {
    fn name(&self) -> &'static str {
        "ollama"
    }
    fn is_local(&self) -> bool {
        true
    }
    fn complete(&self, payload: &Payload, instruction: &str) -> Result<String> {
        let prompt = format!(
            "{instruction}\n\nFile: {}\n---\n{}\n---",
            payload.file_name, payload.snippet
        );
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "options": {"temperature": 0.1, "num_predict": 400}
        });
        let v: serde_json::Value = agent()
            .post(format!("{}/api/generate", self.url.trim_end_matches('/')))
            .send_json(&body)
            .with_context(|| format!("ollama at {}", self.url))?
            .body_mut()
            .read_json()?;
        v["response"]
            .as_str()
            .map(|s| s.trim().to_string())
            .context("ollama: no response field")
    }
}

/// Anthropic Messages API. Opt-in; every call is audited by the caller.
pub struct Cloud {
    pub model: String,
    pub key: String,
}

impl AiAdapter for Cloud {
    fn name(&self) -> &'static str {
        "cloud"
    }
    fn is_local(&self) -> bool {
        false
    }
    fn complete(&self, payload: &Payload, instruction: &str) -> Result<String> {
        anyhow::ensure!(
            !self.key.is_empty(),
            "cloud adapter has no API key (filemind ai use cloud --key … or ANTHROPIC_API_KEY)"
        );
        let user = format!(
            "{instruction}\n\nFile: {}\n---\n{}\n---",
            payload.file_name, payload.snippet
        );
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 400,
            "messages": [{"role": "user", "content": user}]
        });
        let v: serde_json::Value = agent()
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .send_json(&body)
            .context("anthropic api")?
            .body_mut()
            .read_json()?;
        v["content"][0]["text"]
            .as_str()
            .map(|s| s.trim().to_string())
            .with_context(|| format!("unexpected reply: {v}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Purpose;
    use std::io::{Read, Write};

    /// A one-shot fake Ollama that records the request and answers.
    fn fake_ollama() -> (String, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut req = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(i) = text.find("\r\n\r\n") {
                    let len: usize = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            let body = r#"{"response":"  The rent is $2,100. [2] lease  "}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            String::from_utf8_lossy(&req).to_string()
        });
        (url, h)
    }

    #[test]
    fn ollama_adapter_posts_prompt_and_trims_reply() {
        let (url, h) = fake_ollama();
        let o = Ollama {
            url,
            model: "llama3.2".into(),
        };
        let p = Payload::new(
            "search results",
            "[1] lease apartment 2024.txt: rent $2,100",
            Purpose::Ask,
        );
        let a = o.complete(&p, "Answer briefly.").unwrap();
        assert_eq!(a, "The rent is $2,100. [2] lease");
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /api/generate"));
        assert!(req.contains("llama3.2"), "{req}");
        assert!(req.contains("rent $2,100"));
        assert!(req.contains("\"stream\"") && req.contains("false"), "{req}");
    }

    #[test]
    fn cloud_without_key_refuses_before_any_network() {
        let c = Cloud {
            model: "m".into(),
            key: String::new(),
        };
        let p = Payload::new("x", "y", Purpose::Ask);
        let e = c.complete(&p, "i").unwrap_err().to_string();
        assert!(e.contains("no API key"), "{e}");
    }

    #[test]
    fn config_round_trips_and_defaults_to_none() {
        let c = Config::default();
        assert_eq!(c.adapter, Kind::None);
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["adapter"], "none");
        let back: Config = serde_json::from_value(v).unwrap();
        assert_eq!(back.ollama_url, c.ollama_url);
    }
}
