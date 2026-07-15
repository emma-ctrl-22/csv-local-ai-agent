//! Minimal blocking client for the local Ollama HTTP API (/api/chat, /api/tags).
//! Plain HTTP to localhost — no TLS stack, no API keys, fully offline.

use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub num_ctx: u32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:7b-instruct".into(),
            temperature: 0.1, // planning, not creative writing
            num_ctx: 8192,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// Ollama returns arguments as a JSON object (not a string).
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // system | user | assistant | tool
    #[serde(default)]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Newer Ollama versions accept the tool name on tool-result messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into(), tool_calls: None, tool_name: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into(), tool_calls: None, tool_name: None }
    }
    pub fn tool(name: &str, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: Some(name.to_string()),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    options: Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
    #[serde(default)]
    done: bool,
}

pub struct OllamaClient {
    http: reqwest::blocking::Client,
    pub cfg: OllamaConfig,
}

impl OllamaClient {
    pub fn new(cfg: OllamaConfig) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            // Local CPU inference can be slow; be generous.
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| CoreError::Ollama(e.to_string()))?;
        Ok(Self { http, cfg })
    }

    /// One non-streamed chat round. Returns the assistant message, which may
    /// carry tool_calls instead of (or alongside) content.
    pub fn chat(&self, messages: &[ChatMessage], tools: Option<&[Value]>) -> Result<ChatMessage> {
        let req = ChatRequest {
            model: &self.cfg.model,
            messages,
            stream: false,
            tools,
            options: serde_json::json!({
                "temperature": self.cfg.temperature,
                "num_ctx": self.cfg.num_ctx,
            }),
        };
        let url = format!("{}/api/chat", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .json(&req)
            .send()
            .map_err(|e| CoreError::Ollama(format!(
                "cannot reach Ollama at {url}: {e}. Is Ollama running? (`ollama serve`)"
            )))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(CoreError::Ollama(format!(
                "Ollama returned {status}: {body}. If the model isn't pulled yet: `ollama pull {}`",
                self.cfg.model
            )));
        }
        let parsed: ChatResponse = resp
            .json()
            .map_err(|e| CoreError::Ollama(format!("bad response from Ollama: {e}")))?;
        let _ = parsed.done;
        Ok(parsed.message)
    }

    /// Preload the model into memory without generating output, so the user's
    /// first real message isn't paying the cold-start cost. Best-effort.
    pub fn warm(&self) -> Result<()> {
        let url = format!("{}/api/generate", self.cfg.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.cfg.model,
            "prompt": "",
            "keep_alive": "30m",
            "stream": false,
        });
        self.http
            .post(&url)
            // model load on a slow CPU can take a while
            .timeout(Duration::from_secs(300))
            .json(&body)
            .send()
            .map_err(|e| CoreError::Ollama(e.to_string()))?;
        Ok(())
    }

    /// Health check + model presence. Returns the list of local model names.
    pub fn health(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .map_err(|e| CoreError::Ollama(format!("Ollama not reachable at {url}: {e}")))?;
        let v: Value = resp
            .json()
            .map_err(|e| CoreError::Ollama(e.to_string()))?;
        Ok(v.get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }
}
