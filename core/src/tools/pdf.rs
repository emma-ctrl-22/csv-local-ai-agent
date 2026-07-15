//! PDF tools. v1 handles digitally-generated PDFs (extractable text).
//! Scanned PDFs need OCR — flagged in TODO.md.

use super::arg_str;
use crate::workspace::{FileKind, Workspace};
use crate::{CoreError, Result};
use regex::Regex;
use serde_json::{json, Value};

fn pdf_text<'a>(ws: &'a Workspace, file: &str) -> Result<&'a str> {
    let f = ws.file(file)?;
    match &f.kind {
        FileKind::Pdf { text, .. } => Ok(text),
        _ => Err(CoreError::BadArg(format!(
            "'{}' is not a PDF — use spreadsheet tools for it",
            f.name
        ))),
    }
}

pub fn extract_text(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let file = arg_str(args, "file")
        .ok_or_else(|| CoreError::BadArg("'file' is required".into()))?;
    let max_chars = args
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(4000) as usize;
    let query = arg_str(args, "query");

    let text = pdf_text(ws, file)?;
    let body = match query {
        Some(q) => {
            let ql = q.to_lowercase();
            let lines: Vec<&str> = text.lines().collect();
            let mut keep = vec![false; lines.len()];
            for (i, l) in lines.iter().enumerate() {
                if l.to_lowercase().contains(&ql) {
                    if i > 0 { keep[i - 1] = true; }
                    keep[i] = true;
                    if i + 1 < lines.len() { keep[i + 1] = true; }
                }
            }
            let hits: Vec<&str> = lines
                .iter()
                .zip(keep.iter())
                .filter(|(_, k)| **k)
                .map(|(l, _)| *l)
                .collect();
            if hits.is_empty() {
                format!("(no lines containing '{q}')")
            } else {
                hits.join("\n")
            }
        }
        None => text.to_string(),
    };

    let truncated = body.len() > max_chars;
    let mut cut = body;
    if truncated {
        // don't split a UTF-8 char
        let mut end = max_chars;
        while end > 0 && !cut.is_char_boundary(end) {
            end -= 1;
        }
        cut.truncate(end);
    }

    Ok((
        json!({ "file": file, "text": cut, "truncated": truncated }),
        format!(
            "extracted text from {file}{}",
            query.map(|q| format!(" (query: '{q}')")).unwrap_or_default()
        ),
    ))
}

/// Find things that look like money: 1,234.50 / GHS 1,200 / (450.00) / $99.
pub fn extract_amounts(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let file = arg_str(args, "file")
        .ok_or_else(|| CoreError::BadArg("'file' is required".into()))?;
    let query = arg_str(args, "query").map(|s| s.to_lowercase());

    let text = pdf_text(ws, file)?;
    // currency-prefixed or plain amounts with thousands separators/decimals,
    // and accounting-style (parenthesised) negatives
    let re = Regex::new(
        r"(?x)
        (?P<amt>
            \(?\s*
            (?:GHS|GH₵|₵|USD|\$|EUR|€|GBP|£)?\s*
            -?\d{1,3}(?:,\d{3})*(?:\.\d{1,4})?
            \s*\)?
        )",
    )
    .unwrap();

    let mut found = Vec::new();
    for line in text.lines() {
        if let Some(q) = &query {
            if !line.to_lowercase().contains(q.as_str()) {
                continue;
            }
        }
        for cap in re.captures_iter(line) {
            let raw = cap.name("amt").unwrap().as_str().trim();
            // skip bare small integers with no separators/decimals/currency —
            // usually item counts, dates or page numbers, not money
            let has_money_shape = raw.contains('.')
                || raw.contains(',')
                || raw.contains('(')
                || raw.chars().any(|c| !(c.is_ascii_digit() || c == '-' || c.is_whitespace()));
            if !has_money_shape {
                continue;
            }
            if let Some(d) = crate::parse_decimal_lenient(raw) {
                found.push(json!({
                    "amount": d.normalize().to_string(),
                    "as_written": raw,
                    "line": line.trim(),
                }));
            }
        }
        if found.len() >= 200 {
            break;
        }
    }

    let n = found.len();
    Ok((
        json!({
            "file": file,
            "amounts": found,
            "note": "Verify against the line context. Use `calculate` to combine amounts — never add them yourself."
        }),
        format!(
            "scanned {file} for amounts -> {n} found{}",
            query.map(|q| format!(" (query: '{q}')")).unwrap_or_default()
        ),
    ))
}
