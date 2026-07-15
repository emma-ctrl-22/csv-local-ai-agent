//! Append-only audit journal. Every tool execution is recorded here — what
//! ran, with which arguments, and how it went. Exposed to the UI as the
//! "Journal" rail and flushed to `journal.jsonl` beside exports.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u32,
    pub time: DateTime<Utc>,
    pub tool: String,
    /// Human-readable one-liner, e.g. "sum(Amount) grouped by Vendor on sales.xlsx/Sheet1 -> r2 (14 rows)"
    pub summary: String,
    /// Exact arguments the model supplied — full reproducibility.
    pub args: serde_json::Value,
    pub ok: bool,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    next_seq: u32,
}

impl AuditLog {
    pub fn record(&mut self, tool: &str, args: serde_json::Value, summary: String, ok: bool) -> u32 {
        self.next_seq += 1;
        self.entries.push(AuditEntry {
            seq: self.next_seq,
            time: Utc::now(),
            tool: tool.to_string(),
            summary,
            args,
            ok,
        });
        self.next_seq
    }

    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Append all entries newer than `after_seq` to a JSONL file.
    pub fn flush_jsonl(&self, path: &Path, after_seq: u32) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for e in self.entries.iter().filter(|e| e.seq > after_seq) {
            let line = serde_json::to_string(e).unwrap_or_default();
            writeln!(f, "{line}")?;
        }
        Ok(())
    }
}
