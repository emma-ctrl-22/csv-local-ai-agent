//! ledger-core — the deterministic engine behind Ledger Local.
//!
//! Design rule #1: the LLM never does arithmetic. It only *plans* by emitting
//! tool calls; everything numeric happens here in Rust with `rust_decimal`.
//!
//! Layout:
//!   - `workspace`     in-memory files (Excel/PDF), derived result tables, audit log
//!   - `tools`         tool schemas + validated, deterministic implementations
//!   - `ollama`        minimal blocking client for the local Ollama HTTP API
//!   - `orchestrator`  the agent loop: chat -> tool calls -> results -> answer
//!   - `mathexpr`      safe decimal expression parser/evaluator

pub mod audit;
pub mod mathexpr;
pub mod ollama;
pub mod orchestrator;
pub mod tools;
pub mod workspace;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A single spreadsheet cell. Money-safe: numbers are decimals, never f64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum Cell {
    Empty,
    Text(String),
    Number(Decimal),
    Bool(bool),
}

impl Cell {
    pub fn as_display(&self) -> String {
        match self {
            Cell::Empty => String::new(),
            Cell::Text(s) => s.clone(),
            Cell::Number(d) => d.normalize().to_string(),
            Cell::Bool(b) => b.to_string(),
        }
    }

    pub fn as_number(&self) -> Option<Decimal> {
        match self {
            Cell::Number(d) => Some(*d),
            // Accountants' spreadsheets are full of numbers stored as text.
            Cell::Text(s) => parse_decimal_lenient(s),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Cell::Empty) || matches!(self, Cell::Text(s) if s.trim().is_empty())
    }
}

/// Parse "1,234.50", "GHS 1,234.50", "(500)" (accounting negative), "12%".
pub fn parse_decimal_lenient(raw: &str) -> Option<Decimal> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let negative_parens = s.starts_with('(') && s.ends_with(')');
    let mut cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    if negative_parens && !cleaned.starts_with('-') {
        cleaned.insert(0, '-');
    }
    let d: Decimal = cleaned.parse().ok()?;
    if s.ends_with('%') {
        Some(d / Decimal::from(100))
    } else {
        Some(d)
    }
}

/// A rectangular table: the unit every tool consumes and produces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn col_index(&self, name: &str) -> Option<usize> {
        let want = name.trim().to_lowercase();
        self.columns
            .iter()
            .position(|c| c.trim().to_lowercase() == want)
    }

    /// Compact schema summary the model can reason about without seeing all data.
    pub fn schema_summary(&self, sample_rows: usize) -> serde_json::Value {
        let samples: Vec<serde_json::Value> = self
            .rows
            .iter()
            .take(sample_rows)
            .map(|r| row_to_json(&self.columns, r))
            .collect();
        serde_json::json!({
            "columns": self.columns,
            "row_count": self.rows.len(),
            "sample_rows": samples,
        })
    }

    pub fn preview(&self, n: usize) -> Vec<serde_json::Value> {
        self.rows
            .iter()
            .take(n)
            .map(|r| row_to_json(&self.columns, r))
            .collect()
    }
}

pub fn row_to_json(columns: &[String], row: &[Cell]) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (i, col) in columns.iter().enumerate() {
        let cell = row.get(i).cloned().unwrap_or(Cell::Empty);
        let v = match cell {
            Cell::Empty => serde_json::Value::Null,
            // Decimals go out as strings so JSON floats can't corrupt them.
            Cell::Number(d) => serde_json::Value::String(d.normalize().to_string()),
            Cell::Text(s) => serde_json::Value::String(s),
            Cell::Bool(b) => serde_json::Value::Bool(b),
        };
        obj.insert(col.clone(), v);
    }
    serde_json::Value::Object(obj)
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("file not found in workspace: {0}. Load it first, or call list_files.")]
    FileNotFound(String),
    #[error("sheet '{0}' not found in '{1}'. Available sheets: {2}")]
    SheetNotFound(String, String, String),
    #[error("column '{0}' does not exist. Available columns: {1}")]
    ColumnNotFound(String, String),
    #[error("result '{0}' not found. Use the result_id returned by a previous step.")]
    ResultNotFound(String),
    #[error("invalid argument: {0}")]
    BadArg(String),
    #[error("expression error: {0}")]
    Expr(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Excel read error: {0}")]
    Excel(String),
    #[error("Excel write error: {0}")]
    ExcelWrite(String),
    #[error("PDF error: {0}")]
    Pdf(String),
    #[error("Ollama error: {0}")]
    Ollama(String),
    #[error("refusing to overwrite a source file: {0}")]
    OverwriteRefused(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
