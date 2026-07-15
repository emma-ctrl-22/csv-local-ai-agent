//! The in-memory workspace: Excel workbooks and PDFs the user has opened,
//! plus derived result tables produced by tools. Source files are read-only
//! by construction — exports always go to new files in the export directory.

use crate::audit::AuditLog;
use crate::{Cell, CoreError, Result, Table};
use calamine::{open_workbook_auto, Data, Reader};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum FileKind {
    Excel { sheets: BTreeMap<String, Table> },
    Pdf { text: String, page_count_hint: usize },
}

#[derive(Debug)]
pub struct LoadedFile {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
}

/// Lineage for a derived result — what produced it and from what. Powers the
/// "changes made" view in the UI (added columns, row-count deltas).
#[derive(Debug, Clone, Default)]
pub struct ResultMeta {
    pub op: String,
    pub source_id: Option<String>,
    pub source_label: String,
    pub source_columns: Vec<String>,
    pub source_row_count: usize,
    pub summary: String,
}

#[derive(Debug)]
struct StoredResult {
    table: Table,
    meta: ResultMeta,
}

#[derive(Debug)]
pub struct Workspace {
    files: BTreeMap<String, LoadedFile>,
    results: BTreeMap<String, StoredResult>,
    next_result: u32,
    pub audit: AuditLog,
    pub export_dir: PathBuf,
}

impl Workspace {
    pub fn new(export_dir: PathBuf) -> Self {
        Self {
            files: BTreeMap::new(),
            results: BTreeMap::new(),
            next_result: 0,
            audit: AuditLog::default(),
            export_dir,
        }
    }

    // ---------- loading ----------

    pub fn load_path(&mut self, path: &Path) -> Result<serde_json::Value> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| CoreError::BadArg("path has no file name".into()))?
            .to_string();
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let kind = match ext.as_str() {
            "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" => Self::load_excel(path)?,
            "csv" | "tsv" => Self::load_csv(path, if ext == "tsv" { b'\t' } else { b',' })?,
            "pdf" => Self::load_pdf(path)?,
            other => {
                return Err(CoreError::BadArg(format!(
                    "unsupported file type '.{other}' — supported: xlsx, xls, xlsm, csv, tsv, pdf"
                )))
            }
        };

        let summary = describe_kind(&name, &kind);
        self.files.insert(
            name.clone(),
            LoadedFile {
                path: path.to_path_buf(),
                name,
                kind,
            },
        );
        Ok(summary)
    }

    fn load_excel(path: &Path) -> Result<FileKind> {
        let mut wb =
            open_workbook_auto(path).map_err(|e| CoreError::Excel(e.to_string()))?;
        let mut sheets = BTreeMap::new();
        let names: Vec<String> = wb.sheet_names().to_vec();
        for sheet_name in names {
            let range = match wb.worksheet_range(&sheet_name) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let mut rows_iter = range.rows();
            let columns: Vec<String> = match rows_iter.next() {
                Some(header) => header
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let s = data_to_cell(c).as_display();
                        if s.trim().is_empty() {
                            format!("Column{}", i + 1)
                        } else {
                            s.trim().to_string()
                        }
                    })
                    .collect(),
                None => Vec::new(),
            };
            let rows: Vec<Vec<Cell>> = rows_iter
                .map(|r| r.iter().map(data_to_cell).collect())
                .filter(|r: &Vec<Cell>| r.iter().any(|c| !c.is_empty()))
                .collect();
            sheets.insert(sheet_name, Table { columns, rows });
        }
        if sheets.is_empty() {
            return Err(CoreError::Excel("workbook has no readable sheets".into()));
        }
        Ok(FileKind::Excel { sheets })
    }

    fn load_csv(path: &Path, delim: u8) -> Result<FileKind> {
        let raw = std::fs::read_to_string(path)?;
        let delim = delim as char;
        let mut lines = raw.lines();
        let columns: Vec<String> = split_csv_line(lines.next().unwrap_or(""), delim)
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let t = s.trim().to_string();
                if t.is_empty() {
                    format!("Column{}", i + 1)
                } else {
                    t
                }
            })
            .collect();
        let rows: Vec<Vec<Cell>> = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                split_csv_line(l, delim)
                    .into_iter()
                    .map(|field| {
                        let t = field.trim();
                        if t.is_empty() {
                            Cell::Empty
                        } else if let Some(d) = crate::parse_decimal_lenient(t) {
                            // only treat as number when it *looks* numeric
                            if t.chars().next().map_or(false, |c| {
                                c.is_ascii_digit() || c == '-' || c == '(' || c == '.'
                            }) {
                                Cell::Number(d)
                            } else {
                                Cell::Text(t.to_string())
                            }
                        } else {
                            Cell::Text(t.to_string())
                        }
                    })
                    .collect()
            })
            .collect();
        let mut sheets = BTreeMap::new();
        sheets.insert("Sheet1".to_string(), Table { columns, rows });
        Ok(FileKind::Excel { sheets })
    }

    fn load_pdf(path: &Path) -> Result<FileKind> {
        let bytes = std::fs::read(path)?;
        let text = pdf_extract::extract_text_from_mem(&bytes)
            .map_err(|e| CoreError::Pdf(format!(
                "{e}. If this is a scanned PDF (an image), text extraction won't work — it needs OCR (see TODO.md)."
            )))?;
        let page_count_hint = text.matches('\u{c}').count() + 1; // form-feed between pages
        Ok(FileKind::Pdf {
            text,
            page_count_hint,
        })
    }

    // ---------- lookup ----------

    pub fn file(&self, name: &str) -> Result<&LoadedFile> {
        // exact, then case-insensitive
        if let Some(f) = self.files.get(name) {
            return Ok(f);
        }
        let want = name.to_lowercase();
        self.files
            .values()
            .find(|f| f.name.to_lowercase() == want)
            .ok_or_else(|| CoreError::FileNotFound(name.to_string()))
    }

    pub fn sheet(&self, file: &str, sheet: Option<&str>) -> Result<&Table> {
        let f = self.file(file)?;
        match &f.kind {
            FileKind::Excel { sheets } => match sheet {
                Some(s) => {
                    if let Some(t) = sheets.get(s) {
                        return Ok(t);
                    }
                    let want = s.to_lowercase();
                    sheets
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == want)
                        .map(|(_, v)| v)
                        .ok_or_else(|| {
                            CoreError::SheetNotFound(
                                s.to_string(),
                                f.name.clone(),
                                sheets.keys().cloned().collect::<Vec<_>>().join(", "),
                            )
                        })
                }
                None => {
                    if sheets.len() == 1 {
                        Ok(sheets.values().next().unwrap())
                    } else {
                        Err(CoreError::BadArg(format!(
                            "'{}' has several sheets ({}) — specify one",
                            f.name,
                            sheets.keys().cloned().collect::<Vec<_>>().join(", ")
                        )))
                    }
                }
            },
            FileKind::Pdf { .. } => Err(CoreError::BadArg(format!(
                "'{}' is a PDF, not a spreadsheet — use extract_pdf_text / extract_pdf_amounts",
                f.name
            ))),
        }
    }

    /// Resolve a tool "source": either a prior result_id or file(+sheet).
    pub fn resolve_source(
        &self,
        result_id: Option<&str>,
        file: Option<&str>,
        sheet: Option<&str>,
    ) -> Result<(&Table, String)> {
        if let Some(rid) = result_id {
            let t = self
                .results
                .get(rid)
                .map(|s| &s.table)
                .ok_or_else(|| CoreError::ResultNotFound(rid.to_string()))?;
            return Ok((t, rid.to_string()));
        }
        if let Some(f) = file {
            let t = self.sheet(f, sheet)?;
            let label = match sheet {
                Some(s) => format!("{f}/{s}"),
                None => f.to_string(),
            };
            return Ok((t, label));
        }
        Err(CoreError::BadArg(
            "provide either result_id or file (with optional sheet)".into(),
        ))
    }

    pub fn store_result(&mut self, table: Table) -> String {
        self.store_result_meta(table, ResultMeta::default())
    }

    pub fn store_result_meta(&mut self, table: Table, meta: ResultMeta) -> String {
        self.next_result += 1;
        let id = format!("r{}", self.next_result);
        self.results.insert(id.clone(), StoredResult { table, meta });
        id
    }

    pub fn result(&self, id: &str) -> Result<&Table> {
        self.results
            .get(id)
            .map(|s| &s.table)
            .ok_or_else(|| CoreError::ResultNotFound(id.to_string()))
    }

    /// Clear derived results (used by "New chat"). Loaded files are kept.
    pub fn clear_results(&mut self) {
        self.results.clear();
        self.next_result = 0;
    }

    pub fn is_source_path(&self, p: &Path) -> bool {
        self.files.values().any(|f| f.path == p)
    }

    /// A renderable view of a file/sheet or a derived result, for the UI grid.
    /// Returns display strings (never raw floats), which columns are numeric
    /// (for right-alignment), and — for results — what changed vs the source.
    pub fn table_view(
        &self,
        result_id: Option<&str>,
        file: Option<&str>,
        sheet: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<serde_json::Value> {
        if let Some(rid) = result_id {
            let sr = self
                .results
                .get(rid)
                .ok_or_else(|| CoreError::ResultNotFound(rid.to_string()))?;
            let (cols, numeric, rows) = table_to_view(&sr.table, offset, limit);
            let added: Vec<String> = sr
                .table
                .columns
                .iter()
                .filter(|c| {
                    let lc = c.trim().to_lowercase();
                    !sr.meta
                        .source_columns
                        .iter()
                        .any(|s| s.trim().to_lowercase() == lc)
                })
                .cloned()
                .collect();
            return Ok(serde_json::json!({
                "kind": "result",
                "label": rid,
                "columns": cols,
                "col_numeric": numeric,
                "rows": rows,
                "total_rows": sr.table.rows.len(),
                "offset": offset,
                "change": {
                    "op": sr.meta.op,
                    "source_label": sr.meta.source_label,
                    "source_row_count": sr.meta.source_row_count,
                    "result_row_count": sr.table.rows.len(),
                    "added_columns": added,
                    "summary": sr.meta.summary,
                }
            }));
        }
        if let Some(fname) = file {
            let f = self.file(fname)?;
            match &f.kind {
                FileKind::Excel { sheets } => {
                    let t = self.sheet(fname, sheet)?;
                    let (cols, numeric, rows) = table_to_view(t, offset, limit);
                    let active = match sheet {
                        Some(s) => sheets
                            .keys()
                            .find(|k| k.to_lowercase() == s.to_lowercase())
                            .cloned()
                            .unwrap_or_else(|| s.to_string()),
                        None => sheets.keys().next().cloned().unwrap_or_default(),
                    };
                    Ok(serde_json::json!({
                        "kind": "file",
                        "label": fname,
                        "sheets": sheets.keys().cloned().collect::<Vec<_>>(),
                        "active_sheet": active,
                        "columns": cols,
                        "col_numeric": numeric,
                        "rows": rows,
                        "total_rows": t.rows.len(),
                        "offset": offset,
                    }))
                }
                FileKind::Pdf { text, page_count_hint } => {
                    let mut end = 8000.min(text.len());
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    Ok(serde_json::json!({
                        "kind": "pdf",
                        "label": fname,
                        "pages_approx": page_count_hint,
                        "text": &text[..end],
                        "truncated": text.len() > end,
                    }))
                }
            }
        } else {
            Err(CoreError::BadArg("provide result_id or file".into()))
        }
    }

    pub fn list(&self) -> serde_json::Value {
        let files: Vec<serde_json::Value> = self
            .files
            .values()
            .map(|f| describe_kind(&f.name, &f.kind))
            .collect();
        let results: Vec<serde_json::Value> = self
            .results
            .iter()
            .map(|(id, sr)| {
                serde_json::json!({
                    "result_id": id,
                    "columns": sr.table.columns,
                    "row_count": sr.table.rows.len(),
                    "op": sr.meta.op,
                    "source_label": sr.meta.source_label,
                })
            })
            .collect();
        serde_json::json!({ "files": files, "results": results })
    }
}

/// Build (columns, per-column numeric flag, display rows) for a page of a table.
fn table_to_view(t: &Table, offset: usize, limit: usize) -> (Vec<String>, Vec<bool>, Vec<Vec<String>>) {
    let rows: Vec<Vec<String>> = t
        .rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|r| {
            (0..t.columns.len())
                .map(|i| r.get(i).cloned().unwrap_or(Cell::Empty).as_display())
                .collect()
        })
        .collect();
    let mut col_numeric = vec![false; t.columns.len()];
    for (ci, flag) in col_numeric.iter_mut().enumerate() {
        let (mut num, mut nonempty) = (0usize, 0usize);
        for r in t.rows.iter().take(200) {
            let cell = r.get(ci).cloned().unwrap_or(Cell::Empty);
            if cell.is_empty() {
                continue;
            }
            nonempty += 1;
            if cell.as_number().is_some() {
                num += 1;
            }
        }
        *flag = nonempty > 0 && num * 2 >= nonempty; // majority numeric
    }
    (t.columns.clone(), col_numeric, rows)
}

fn describe_kind(name: &str, kind: &FileKind) -> serde_json::Value {
    match kind {
        FileKind::Excel { sheets } => serde_json::json!({
            "file": name,
            "type": "spreadsheet",
            "sheets": sheets.iter().map(|(s, t)| serde_json::json!({
                "sheet": s,
                "columns": t.columns,
                "row_count": t.rows.len(),
            })).collect::<Vec<_>>(),
        }),
        FileKind::Pdf {
            text,
            page_count_hint,
        } => serde_json::json!({
            "file": name,
            "type": "pdf",
            "pages_approx": page_count_hint,
            "characters": text.len(),
        }),
    }
}

/// RFC-4180-ish field splitter: honors "quoted, fields" and "" escapes.
fn split_csv_line(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == delim {
            out.push(std::mem::take(&mut field));
        } else {
            field.push(c);
        }
    }
    out.push(field);
    out
}

fn data_to_cell(d: &Data) -> Cell {
    match d {
        Data::Empty => Cell::Empty,
        Data::String(s) => {
            if s.trim().is_empty() {
                Cell::Empty
            } else {
                Cell::Text(s.clone())
            }
        }
        Data::Float(f) => float_to_cell(*f),
        Data::Int(i) => Cell::Number(Decimal::from(*i)),
        Data::Bool(b) => Cell::Bool(*b),
        Data::DateTime(dt) => {
            // Keep the serial; formatting dates is a v2 concern. Most
            // accounting math targets amount columns, not date columns.
            float_to_cell(dt.as_f64())
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => Cell::Text(s.clone()),
        Data::Error(e) => Cell::Text(format!("#ERR:{e:?}")),
    }
}

/// xlsx stores numbers as IEEE-754 doubles; convert then trim float dust
/// (e.g. 1234.4999999999998 -> 1234.5) by rounding to 10 dp and normalizing.
fn float_to_cell(f: f64) -> Cell {
    match Decimal::try_from(f) {
        Ok(d) => Cell::Number(d.round_dp(10).normalize()),
        Err(_) => Cell::Text(f.to_string()),
    }
}
