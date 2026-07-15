//! Deterministic table operations. All money math is `Decimal`.

use super::{arg_str, arg_usize, source_of};
use crate::workspace::Workspace;
use crate::{Cell, CoreError, Result, Table};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const PREVIEW: usize = 10;
const SHOW_MAX: usize = 50;

pub fn get_schema(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (table, label) = source_of(ws, args)?;
    Ok((
        table.schema_summary(5),
        format!("read schema of {label} ({} rows)", table.rows.len()),
    ))
}

pub fn show_rows(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (table, label) = source_of(ws, args)?;
    let offset = arg_usize(args, "offset").unwrap_or(0);
    let limit = arg_usize(args, "limit").unwrap_or(20).min(SHOW_MAX);
    let rows: Vec<Value> = table
        .rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|r| crate::row_to_json(&table.columns, r))
        .collect();
    let n = rows.len();
    Ok((
        json!({ "columns": table.columns, "rows": rows, "offset": offset, "total_rows": table.rows.len() }),
        format!("showed {n} rows of {label}"),
    ))
}

// ---------------- filter ----------------

struct Cond {
    col_idx: usize,
    col_name: String,
    op: String,
    value: Option<Value>,
}

fn parse_conditions(table: &Table, args: &Value) -> Result<Vec<Cond>> {
    let raw = args
        .get("conditions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CoreError::BadArg("'conditions' must be an array".into()))?;
    if raw.is_empty() {
        return Err(CoreError::BadArg("'conditions' is empty".into()));
    }
    raw.iter()
        .map(|c| {
            let col = c
                .get("column")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CoreError::BadArg("condition missing 'column'".into()))?;
            let idx = table
                .col_index(col)
                .ok_or_else(|| col_err(table, col))?;
            let op = c
                .get("op")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CoreError::BadArg("condition missing 'op'".into()))?
                .to_string();
            Ok(Cond {
                col_idx: idx,
                col_name: col.to_string(),
                op,
                value: c.get("value").cloned(),
            })
        })
        .collect()
}

fn col_err(table: &Table, col: &str) -> CoreError {
    CoreError::ColumnNotFound(col.to_string(), table.columns.join(", "))
}

fn cond_matches(cell: &Cell, cond: &Cond) -> Result<bool> {
    match cond.op.as_str() {
        "empty" => return Ok(cell.is_empty()),
        "not_empty" => return Ok(!cell.is_empty()),
        _ => {}
    }
    let value = cond.value.as_ref().ok_or_else(|| {
        CoreError::BadArg(format!(
            "condition on '{}' with op '{}' needs a 'value'",
            cond.col_name, cond.op
        ))
    })?;

    // Numeric comparison when both sides are numeric; else string comparison.
    let rhs_num: Option<Decimal> = match value {
        Value::Number(n) => n.as_f64().and_then(|f| Decimal::try_from(f).ok()),
        Value::String(s) => crate::parse_decimal_lenient(s),
        _ => None,
    };
    if let (Some(l), Some(r)) = (cell.as_number(), rhs_num) {
        return Ok(match cond.op.as_str() {
            "eq" => l == r,
            "ne" => l != r,
            "gt" => l > r,
            "gte" => l >= r,
            "lt" => l < r,
            "lte" => l <= r,
            "contains" => l.to_string().contains(&r.to_string()),
            other => return Err(CoreError::BadArg(format!("unknown op '{other}'"))),
        });
    }

    let l = cell.as_display().to_lowercase();
    let r = match value {
        Value::String(s) => s.to_lowercase(),
        other => other.to_string().to_lowercase(),
    };
    Ok(match cond.op.as_str() {
        "eq" => l == r,
        "ne" => l != r,
        "contains" => l.contains(&r),
        "gt" => l > r,
        "gte" => l >= r,
        "lt" => l < r,
        "lte" => l <= r,
        other => return Err(CoreError::BadArg(format!("unknown op '{other}'"))),
    })
}

pub fn filter_rows(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (out, desc) = {
        let (table, label) = source_of(ws, args)?;
        let conds = parse_conditions(table, args)?;
        let mut rows = Vec::new();
        for row in &table.rows {
            let mut keep = true;
            for c in &conds {
                let cell = row.get(c.col_idx).cloned().unwrap_or(Cell::Empty);
                if !cond_matches(&cell, c)? {
                    keep = false;
                    break;
                }
            }
            if keep {
                rows.push(row.clone());
            }
        }
        let desc = conds
            .iter()
            .map(|c| format!("{} {} {}", c.col_name, c.op, c.value.as_ref().map(|v| v.to_string()).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(" AND ");
        (
            Table { columns: table.columns.clone(), rows },
            format!("filter [{desc}] on {label}"),
        )
    };
    finish_result(ws, out, desc, "filter_rows", args)
}

// ---------------- aggregate ----------------

pub fn aggregate(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (out, desc) = {
        let (table, label) = source_of(ws, args)?;

        let group_by: Vec<usize> = args
            .get("group_by")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|g| {
                        let name = g.as_str().ok_or_else(|| {
                            CoreError::BadArg("group_by entries must be strings".into())
                        })?;
                        table.col_index(name).ok_or_else(|| col_err(table, name))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        let aggs_raw = args
            .get("aggregations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| CoreError::BadArg("'aggregations' must be an array".into()))?;
        if aggs_raw.is_empty() {
            return Err(CoreError::BadArg("'aggregations' is empty".into()));
        }
        struct Agg { idx: usize, name: String, f: String }
        let aggs: Vec<Agg> = aggs_raw
            .iter()
            .map(|a| {
                let col = a.get("column").and_then(|v| v.as_str()).ok_or_else(|| {
                    CoreError::BadArg("aggregation missing 'column'".into())
                })?;
                let f = a.get("fn").and_then(|v| v.as_str()).ok_or_else(|| {
                    CoreError::BadArg("aggregation missing 'fn'".into())
                })?;
                if !["sum", "avg", "min", "max", "count"].contains(&f) {
                    return Err(CoreError::BadArg(format!(
                        "unknown aggregation fn '{f}' (use sum, avg, min, max, count)"
                    )));
                }
                Ok(Agg {
                    idx: table.col_index(col).ok_or_else(|| col_err(table, col))?,
                    name: col.to_string(),
                    f: f.to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // group key -> (key cells, per-agg accumulator)
        #[derive(Default, Clone)]
        struct Acc { sum: Decimal, count: u64, min: Option<Decimal>, max: Option<Decimal> }
        let mut groups: BTreeMap<Vec<String>, (Vec<Cell>, Vec<Acc>)> = BTreeMap::new();

        for row in &table.rows {
            let key: Vec<String> = group_by
                .iter()
                .map(|&i| row.get(i).cloned().unwrap_or(Cell::Empty).as_display())
                .collect();
            let entry = groups.entry(key.clone()).or_insert_with(|| {
                (
                    group_by
                        .iter()
                        .map(|&i| row.get(i).cloned().unwrap_or(Cell::Empty))
                        .collect(),
                    vec![Acc::default(); aggs.len()],
                )
            });
            for (ai, agg) in aggs.iter().enumerate() {
                let cell = row.get(agg.idx).cloned().unwrap_or(Cell::Empty);
                let acc = &mut entry.1[ai];
                if agg.f == "count" {
                    if !cell.is_empty() {
                        acc.count += 1;
                    }
                    continue;
                }
                if let Some(n) = cell.as_number() {
                    acc.sum += n;
                    acc.count += 1;
                    acc.min = Some(acc.min.map_or(n, |m| m.min(n)));
                    acc.max = Some(acc.max.map_or(n, |m| m.max(n)));
                }
            }
        }

        let mut columns: Vec<String> = group_by
            .iter()
            .map(|&i| table.columns[i].clone())
            .collect();
        for a in &aggs {
            columns.push(format!("{}({})", a.f, a.name));
        }
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        for (_, (key_cells, accs)) in groups {
            let mut row = key_cells;
            for (ai, agg) in aggs.iter().enumerate() {
                let acc = &accs[ai];
                let cell = match agg.f.as_str() {
                    "sum" => Cell::Number(acc.sum),
                    "count" => Cell::Number(Decimal::from(acc.count)),
                    "avg" => {
                        if acc.count == 0 {
                            Cell::Empty
                        } else {
                            Cell::Number(acc.sum / Decimal::from(acc.count))
                        }
                    }
                    "min" => acc.min.map(Cell::Number).unwrap_or(Cell::Empty),
                    "max" => acc.max.map(Cell::Number).unwrap_or(Cell::Empty),
                    _ => Cell::Empty,
                };
                row.push(cell);
            }
            rows.push(row);
        }

        let desc = format!(
            "{} grouped by [{}] on {label}",
            aggs.iter().map(|a| format!("{}({})", a.f, a.name)).collect::<Vec<_>>().join(", "),
            group_by.iter().map(|&i| table.columns[i].clone()).collect::<Vec<_>>().join(", "),
        );
        (Table { columns, rows }, desc)
    };
    finish_result(ws, out, desc, "aggregate", args)
}

// ---------------- sort ----------------

pub fn sort_rows(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (out, desc) = {
        let (table, label) = source_of(ws, args)?;
        let col = arg_str(args, "column")
            .ok_or_else(|| CoreError::BadArg("'column' is required".into()))?;
        let idx = table.col_index(col).ok_or_else(|| col_err(table, col))?;
        let descending = args.get("descending").and_then(|v| v.as_bool()).unwrap_or(false);
        let limit = arg_usize(args, "limit");

        let mut rows = table.rows.clone();
        rows.sort_by(|a, b| {
            let ca = a.get(idx).cloned().unwrap_or(Cell::Empty);
            let cb = b.get(idx).cloned().unwrap_or(Cell::Empty);
            let ord = match (ca.as_number(), cb.as_number()) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => ca.as_display().to_lowercase().cmp(&cb.as_display().to_lowercase()),
            };
            if descending { ord.reverse() } else { ord }
        });
        if let Some(l) = limit {
            rows.truncate(l);
        }
        let desc = format!(
            "sort by {col} {}{} on {label}",
            if descending { "desc" } else { "asc" },
            limit.map(|l| format!(", top {l}")).unwrap_or_default()
        );
        (Table { columns: table.columns.clone(), rows }, desc)
    };
    finish_result(ws, out, desc, "sort_rows", args)
}

// ---------------- compute_column ----------------

pub fn compute_column(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let (out, desc) = {
        let (table, label) = source_of(ws, args)?;
        let new_col = arg_str(args, "new_column")
            .ok_or_else(|| CoreError::BadArg("'new_column' is required".into()))?;
        let expr = arg_str(args, "expression")
            .ok_or_else(|| CoreError::BadArg("'expression' is required".into()))?;
        let round_dp = args.get("round_dp").and_then(|v| v.as_u64()).map(|v| v as u32);

        if table.col_index(new_col).is_some() {
            return Err(CoreError::BadArg(format!(
                "column '{new_col}' already exists — choose a new name"
            )));
        }

        let mut columns = table.columns.clone();
        columns.push(new_col.to_string());
        let mut rows = Vec::with_capacity(table.rows.len());
        let mut blanks = 0usize;
        for row in &table.rows {
            let lookup = |name: &str| -> Option<Decimal> {
                let idx = table.col_index(name)?;
                row.get(idx).and_then(|c| c.as_number())
            };
            let mut new_row = row.clone();
            match crate::mathexpr::eval_with(expr, &lookup) {
                Ok(v) => {
                    let v = match round_dp { Some(dp) => v.round_dp(dp), None => v };
                    new_row.push(Cell::Number(v));
                }
                Err(_) => {
                    // Row had a non-numeric/blank input — leave the cell empty
                    // rather than poisoning the whole operation.
                    blanks += 1;
                    new_row.push(Cell::Empty);
                }
            }
            rows.push(new_row);
        }

        // Validate the expression itself against the schema (catch typo'd
        // column names) — if every single row failed, the expression is bad.
        if blanks == table.rows.len() && !table.rows.is_empty() {
            let probe = crate::mathexpr::eval_with(expr, &|name| {
                table.col_index(name).map(|_| Decimal::ONE)
            });
            if let Err(e) = probe {
                return Err(e);
            }
        }

        let desc = format!(
            "computed {new_col} = {expr} on {label}{}",
            if blanks > 0 { format!(" ({blanks} rows blank: non-numeric inputs)") } else { String::new() }
        );
        (Table { columns, rows }, desc)
    };
    finish_result(ws, out, desc, "compute_column", args)
}

// ---------------- export ----------------

pub fn export_xlsx(ws: &mut Workspace, args: &Value) -> Result<(Value, String)> {
    let filename = arg_str(args, "filename")
        .ok_or_else(|| CoreError::BadArg("'filename' is required".into()))?;

    // sanitize: strip path components, force .xlsx
    let base = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect::<String>();
    let mut path = ws.export_dir.join(format!("{base}.xlsx"));
    let mut n = 1;
    while path.exists() {
        path = ws.export_dir.join(format!("{base} ({n}).xlsx"));
        n += 1;
    }
    if ws.is_source_path(&path) {
        return Err(CoreError::OverwriteRefused(path.display().to_string()));
    }

    let (row_count, label) = {
        let (table, label) = source_of(ws, args)?;
        std::fs::create_dir_all(&ws.export_dir)?;
        let mut wb = rust_xlsxwriter::Workbook::new();
        let sheet = wb.add_worksheet();
        let bold = rust_xlsxwriter::Format::new().set_bold();
        for (c, name) in table.columns.iter().enumerate() {
            sheet
                .write_string_with_format(0, c as u16, name, &bold)
                .map_err(|e| CoreError::ExcelWrite(e.to_string()))?;
        }
        for (r, row) in table.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let (r, c) = ((r + 1) as u32, c as u16);
                match cell {
                    Cell::Empty => {}
                    Cell::Text(s) => {
                        sheet.write_string(r, c, s).map_err(|e| CoreError::ExcelWrite(e.to_string()))?;
                    }
                    Cell::Number(d) => {
                        use rust_decimal::prelude::ToPrimitive;
                        let f = d.to_f64().unwrap_or(0.0);
                        sheet.write_number(r, c, f).map_err(|e| CoreError::ExcelWrite(e.to_string()))?;
                    }
                    Cell::Bool(b) => {
                        sheet.write_boolean(r, c, *b).map_err(|e| CoreError::ExcelWrite(e.to_string()))?;
                    }
                }
            }
        }
        wb.save(&path).map_err(|e| CoreError::ExcelWrite(e.to_string()))?;
        (table.rows.len(), label)
    };

    let shown = path.display().to_string();
    Ok((
        json!({ "saved_to": shown, "rows_written": row_count }),
        format!("exported {label} ({row_count} rows) -> {shown}"),
    ))
}

// ---------------- shared ----------------

fn finish_result(
    ws: &mut Workspace,
    table: Table,
    desc: String,
    op: &str,
    args: &Value,
) -> Result<(Value, String)> {
    // Capture the source (columns + row count) before we store the new table,
    // so the UI can show what changed. Source is untouched by any operation.
    let meta = match source_of(ws, args) {
        Ok((src, label)) => crate::workspace::ResultMeta {
            op: op.to_string(),
            source_id: arg_str(args, "result_id").map(|s| s.to_string()),
            source_label: label,
            source_columns: src.columns.clone(),
            source_row_count: src.rows.len(),
            summary: desc.clone(),
        },
        Err(_) => crate::workspace::ResultMeta {
            op: op.to_string(),
            summary: desc.clone(),
            ..Default::default()
        },
    };

    let row_count = table.rows.len();
    let columns = table.columns.clone();
    let preview = table.preview(PREVIEW);
    let id = ws.store_result_meta(table, meta);
    Ok((
        json!({
            "result_id": id,
            "columns": columns,
            "row_count": row_count,
            "preview_first_rows": preview,
            "note": if row_count > PREVIEW { "preview truncated — use show_rows or export_xlsx for full data" } else { "preview shows all rows" },
        }),
        format!("{desc} -> {id} ({row_count} rows)"),
    ))
}
