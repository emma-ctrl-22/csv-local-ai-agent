//! The tool registry. This is the *only* surface the LLM can touch.
//! Every call is validated against the real workspace before executing,
//! every execution is deterministic Rust, and every outcome is audited.

pub mod excel;
pub mod math;
pub mod pdf;

use crate::workspace::Workspace;
use serde_json::{json, Value};

/// Tool definitions in Ollama / OpenAI function-calling format.
pub fn definitions() -> Vec<Value> {
    let t = |name: &str, desc: &str, params: Value| {
        json!({
            "type": "function",
            "function": { "name": name, "description": desc, "parameters": params }
        })
    };

    let source_props = json!({
        "result_id": { "type": "string", "description": "Use a result_id from a previous step (e.g. 'r1') to chain operations. Provide EITHER this OR file." },
        "file": { "type": "string", "description": "A loaded file name, e.g. 'sales.xlsx'." },
        "sheet": { "type": "string", "description": "Sheet name; optional if the workbook has one sheet." }
    });

    vec![
        t("list_files",
          "List every loaded file, its sheets/columns/row counts, and all derived results. Call this first if unsure what exists.",
          json!({ "type": "object", "properties": {} })),

        t("get_schema",
          "Get the columns, row count and a few sample rows of a sheet or prior result. Use before referencing columns.",
          json!({ "type": "object", "properties": source_props, "required": [] })),

        t("filter_rows",
          "Keep only rows matching ALL conditions. Returns a new result_id plus a preview. Never modifies the source.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "conditions": { "type": "array", "items": { "type": "object", "properties": {
                  "column": { "type": "string" },
                  "op": { "type": "string", "enum": ["eq","ne","gt","gte","lt","lte","contains","not_empty","empty"] },
                  "value": { "description": "Number or string to compare against (omit for empty/not_empty)." }
              }, "required": ["column","op"] } }
          }, "required": ["conditions"] })),

        t("aggregate",
          "Group rows and compute sum/avg/min/max/count per group using exact decimal arithmetic. Empty group_by = totals over all rows. Returns a new result_id.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "group_by": { "type": "array", "items": { "type": "string" }, "description": "Columns to group by; [] for grand totals." },
              "aggregations": { "type": "array", "items": { "type": "object", "properties": {
                  "column": { "type": "string" },
                  "fn": { "type": "string", "enum": ["sum","avg","min","max","count"] }
              }, "required": ["column","fn"] } }
          }, "required": ["aggregations"] })),

        t("sort_rows",
          "Sort by a column (numbers numerically, text alphabetically). Returns a new result_id.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "column": { "type": "string" },
              "descending": { "type": "boolean" },
              "limit": { "type": "integer", "description": "Optional: keep only the first N rows after sorting (top-N)." }
          }, "required": ["column"] })),

        t("compute_column",
          "Add a derived column computed per-row from an arithmetic expression over other columns, e.g. 'Amount * 0.15' or '[Unit Price] * Quantity'. Exact decimal math. Returns a new result_id.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "new_column": { "type": "string" },
              "expression": { "type": "string", "description": "Arithmetic only: + - * / % ( ), numbers, column names ([brackets] for names with spaces)." },
              "round_dp": { "type": "integer", "description": "Optional decimal places to round to (e.g. 2 for money)." }
          }, "required": ["new_column","expression"] })),

        t("calculate",
          "Evaluate one arithmetic expression with exact decimal math, e.g. '(1250.40 + 980.10) * 0.15'. ALWAYS use this instead of doing arithmetic yourself.",
          json!({ "type": "object", "properties": {
              "expression": { "type": "string" }
          }, "required": ["expression"] })),

        t("show_rows",
          "Return up to 50 rows of a sheet or result so you can inspect actual values.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "offset": { "type": "integer" },
              "limit": { "type": "integer", "description": "Max 50." }
          }, "required": [] })),

        t("export_xlsx",
          "Write a result (or a whole sheet) to a NEW .xlsx file in the export folder. Never overwrites sources. Returns the saved path.",
          json!({ "type": "object", "properties": {
              "result_id": source_props["result_id"], "file": source_props["file"], "sheet": source_props["sheet"],
              "filename": { "type": "string", "description": "Base name, e.g. 'vendor_totals.xlsx'." }
          }, "required": ["filename"] })),

        t("extract_pdf_text",
          "Get text from a loaded PDF. Optionally filter to lines containing a query string. Use for reading invoices/statements.",
          json!({ "type": "object", "properties": {
              "file": { "type": "string" },
              "query": { "type": "string", "description": "Optional: only return lines containing this (case-insensitive), with 1 line of context." },
              "max_chars": { "type": "integer", "description": "Cap on returned characters (default 4000)." }
          }, "required": ["file"] })),

        t("extract_pdf_amounts",
          "Scan a loaded PDF for monetary amounts (handles 1,234.50, GHS/USD prefixes, (parentheses) negatives) and return each with its line of context. Then use `calculate` on the ones you need — never add them in your head.",
          json!({ "type": "object", "properties": {
              "file": { "type": "string" },
              "query": { "type": "string", "description": "Optional: only lines containing this text." }
          }, "required": ["file"] })),
    ]
}

/// Execute one validated tool call. Errors become structured tool results the
/// model can read and correct — they are never fatal to the conversation.
pub fn execute(ws: &mut Workspace, name: &str, args: &Value) -> (Value, String, bool) {
    let outcome = dispatch(ws, name, args);
    match outcome {
        Ok((payload, summary)) => {
            ws.audit.record(name, args.clone(), summary.clone(), true);
            (payload, summary, true)
        }
        Err(e) => {
            let msg = e.to_string();
            ws.audit
                .record(name, args.clone(), format!("FAILED: {msg}"), false);
            (json!({ "error": msg }), format!("FAILED: {msg}"), false)
        }
    }
}

fn dispatch(ws: &mut Workspace, name: &str, args: &Value) -> crate::Result<(Value, String)> {
    match name {
        "list_files" => Ok((ws.list(), "listed workspace".into())),
        "get_schema" => excel::get_schema(ws, args),
        "filter_rows" => excel::filter_rows(ws, args),
        "aggregate" => excel::aggregate(ws, args),
        "sort_rows" => excel::sort_rows(ws, args),
        "compute_column" => excel::compute_column(ws, args),
        "show_rows" => excel::show_rows(ws, args),
        "export_xlsx" => excel::export_xlsx(ws, args),
        "calculate" => math::calculate(args),
        "extract_pdf_text" => pdf::extract_text(ws, args),
        "extract_pdf_amounts" => pdf::extract_amounts(ws, args),
        other => Err(crate::CoreError::BadArg(format!(
            "unknown tool '{other}'. Available: list_files, get_schema, filter_rows, aggregate, sort_rows, compute_column, calculate, show_rows, export_xlsx, extract_pdf_text, extract_pdf_amounts"
        ))),
    }
}

// -------- shared arg helpers --------

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())
}

pub(crate) fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize)
}

pub(crate) fn source_of<'a>(
    ws: &'a Workspace,
    args: &Value,
) -> crate::Result<(&'a crate::Table, String)> {
    ws.resolve_source(
        arg_str(args, "result_id"),
        arg_str(args, "file"),
        arg_str(args, "sheet"),
    )
}
