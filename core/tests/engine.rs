//! End-to-end tests of the deterministic engine: write a real xlsx, load it,
//! run the exact tool calls the model would make, and check the money math.

use ledger_core::tools;
use ledger_core::workspace::Workspace;
use serde_json::json;

fn make_sales_xlsx(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("sales.xlsx");
    let mut wb = rust_xlsxwriter::Workbook::new();
    let s = wb.add_worksheet();
    let headers = ["Vendor", "Category", "Amount", "Quantity"];
    for (c, h) in headers.iter().enumerate() {
        s.write_string(0, c as u16, *h).unwrap();
    }
    let rows: Vec<(&str, &str, f64, f64)> = vec![
        ("Kumasi Traders", "Supplies", 1250.40, 3.0),
        ("Accra Wholesale", "Supplies", 980.10, 2.0),
        ("Kumasi Traders", "Transport", 300.00, 1.0),
        ("Volta Goods", "Supplies", 45.55, 5.0),
        ("Accra Wholesale", "Transport", 120.90, 2.0),
    ];
    for (r, (v, cat, amt, qty)) in rows.iter().enumerate() {
        let r = (r + 1) as u32;
        s.write_string(r, 0, *v).unwrap();
        s.write_string(r, 1, *cat).unwrap();
        s.write_number(r, 2, *amt).unwrap();
        s.write_number(r, 3, *qty).unwrap();
    }
    wb.save(&path).unwrap();
    path
}

fn ws_with_sales() -> (Workspace, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = make_sales_xlsx(dir.path());
    let mut ws = Workspace::new(dir.path().join("exports"));
    ws.load_path(&xlsx).unwrap();
    (ws, dir)
}

#[test]
fn load_reports_schema() {
    let (mut ws, _dir) = ws_with_sales();
    let (v, _, ok) = tools::execute(&mut ws, "list_files", &json!({}));
    assert!(ok);
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["sheets"][0]["row_count"], 5);
    assert_eq!(
        files[0]["sheets"][0]["columns"],
        json!(["Vendor", "Category", "Amount", "Quantity"])
    );
}

#[test]
fn filter_then_aggregate_is_exact() {
    let (mut ws, _dir) = ws_with_sales();

    // filter: Category == Supplies
    let (v, _, ok) = tools::execute(
        &mut ws,
        "filter_rows",
        &json!({
            "file": "sales.xlsx",
            "conditions": [{ "column": "Category", "op": "eq", "value": "Supplies" }]
        }),
    );
    assert!(ok, "{v}");
    assert_eq!(v["row_count"], 3);
    let r1 = v["result_id"].as_str().unwrap().to_string();

    // aggregate: sum Amount by Vendor
    let (v, _, ok) = tools::execute(
        &mut ws,
        "aggregate",
        &json!({
            "result_id": r1,
            "group_by": ["Vendor"],
            "aggregations": [{ "column": "Amount", "fn": "sum" }]
        }),
    );
    assert!(ok, "{v}");
    let rows = v["preview_first_rows"].as_array().unwrap();
    // BTreeMap ordering: Accra, Kumasi, Volta
    assert_eq!(rows[0]["Vendor"], "Accra Wholesale");
    assert_eq!(rows[0]["sum(Amount)"], "980.1");
    assert_eq!(rows[1]["Vendor"], "Kumasi Traders");
    assert_eq!(rows[1]["sum(Amount)"], "1250.4");
    assert_eq!(rows[2]["Vendor"], "Volta Goods");
    assert_eq!(rows[2]["sum(Amount)"], "45.55");
}

#[test]
fn grand_total_and_vat_chain() {
    let (mut ws, _dir) = ws_with_sales();

    // grand total of Amount
    let (v, _, ok) = tools::execute(
        &mut ws,
        "aggregate",
        &json!({
            "file": "sales.xlsx",
            "group_by": [],
            "aggregations": [{ "column": "Amount", "fn": "sum" }]
        }),
    );
    assert!(ok, "{v}");
    let total = v["preview_first_rows"][0]["sum(Amount)"].as_str().unwrap();
    assert_eq!(total, "2696.95"); // 1250.40+980.10+300.00+45.55+120.90

    // VAT via calculate (exact decimal)
    let (v, _, ok) = tools::execute(
        &mut ws,
        "calculate",
        &json!({ "expression": format!("{total} * 0.15") }),
    );
    assert!(ok);
    assert_eq!(v["result"], "2696.95".parse::<f64>().map(|_| "404.5425").unwrap());
}

#[test]
fn compute_column_line_totals() {
    let (mut ws, _dir) = ws_with_sales();
    let (v, _, ok) = tools::execute(
        &mut ws,
        "compute_column",
        &json!({
            "file": "sales.xlsx",
            "new_column": "Line Total",
            "expression": "Amount * Quantity",
            "round_dp": 2
        }),
    );
    assert!(ok, "{v}");
    let rows = v["preview_first_rows"].as_array().unwrap();
    assert_eq!(rows[0]["Line Total"], "3751.2"); // 1250.40 * 3
    assert_eq!(rows[3]["Line Total"], "227.75"); // 45.55 * 5
}

#[test]
fn sort_top_n() {
    let (mut ws, _dir) = ws_with_sales();
    let (v, _, ok) = tools::execute(
        &mut ws,
        "sort_rows",
        &json!({ "file": "sales.xlsx", "column": "Amount", "descending": true, "limit": 2 }),
    );
    assert!(ok, "{v}");
    let rows = v["preview_first_rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["Amount"], "1250.4");
    assert_eq!(rows[1]["Amount"], "980.1");
}

#[test]
fn bad_column_is_a_recoverable_error_and_audited() {
    let (mut ws, _dir) = ws_with_sales();
    let (v, _, ok) = tools::execute(
        &mut ws,
        "aggregate",
        &json!({
            "file": "sales.xlsx",
            "group_by": ["Vendor"],
            "aggregations": [{ "column": "Amout", "fn": "sum" }] // typo
        }),
    );
    assert!(!ok);
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("Amout"));
    assert!(err.contains("Amount"), "error should list real columns: {err}");
    // audited as a failure
    let last = ws.audit.entries().last().unwrap();
    assert!(!last.ok);
}

#[test]
fn export_roundtrip_and_no_overwrite() {
    let (mut ws, dir) = ws_with_sales();
    let (v, _, ok) = tools::execute(
        &mut ws,
        "aggregate",
        &json!({
            "file": "sales.xlsx",
            "group_by": ["Vendor"],
            "aggregations": [{ "column": "Amount", "fn": "sum" }]
        }),
    );
    assert!(ok);
    let rid = v["result_id"].as_str().unwrap().to_string();

    let (v, _, ok) = tools::execute(
        &mut ws,
        "export_xlsx",
        &json!({ "result_id": rid, "filename": "vendor_totals.xlsx" }),
    );
    assert!(ok, "{v}");
    let saved = v["saved_to"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&saved).exists());

    // read the export back and verify the numbers survived
    let mut ws2 = Workspace::new(dir.path().join("exports2"));
    let loaded = ws2.load_path(std::path::Path::new(&saved)).unwrap();
    assert_eq!(loaded["sheets"][0]["row_count"], 3);
    let (v, _, ok) = tools::execute(
        &mut ws2,
        "show_rows",
        &json!({ "file": std::path::Path::new(&saved).file_name().unwrap().to_str().unwrap() }),
    );
    assert!(ok);
    // Kumasi Traders across both categories: 1250.40 + 300.00
    assert_eq!(v["rows"][1]["sum(Amount)"], "1550.4");

    // exporting again with the same name must NOT overwrite: gets " (1)"
    let (v, _, ok) = tools::execute(
        &mut ws,
        "export_xlsx",
        &json!({ "result_id": "r1", "filename": "vendor_totals.xlsx" }),
    );
    assert!(ok);
    assert!(v["saved_to"].as_str().unwrap().contains("(1)"));
}

#[test]
fn csv_loading_and_lenient_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("ledger.csv");
    std::fs::write(
        &csv,
        "Description,Amount\nOffice rent,\"1,200.50\"\nRefund,(300.25)\nStationery,45\n",
    )
    .unwrap();
    let mut ws = Workspace::new(dir.path().join("exports"));
    ws.load_path(&csv).unwrap();
    let (v, _, ok) = tools::execute(
        &mut ws,
        "aggregate",
        &json!({
            "file": "ledger.csv",
            "group_by": [],
            "aggregations": [{ "column": "Amount", "fn": "sum" }]
        }),
    );
    assert!(ok, "{v}");
    // 1200.50 - 300.25 + 45 = 945.25
    assert_eq!(v["preview_first_rows"][0]["sum(Amount)"], "945.25");
}

#[test]
fn table_view_reports_changes() {
    let (mut ws, _dir) = ws_with_sales();

    // compute_column → added column should show up in the view's change info
    let (v, _, ok) = tools::execute(
        &mut ws,
        "compute_column",
        &json!({ "file": "sales.xlsx", "new_column": "Line Total", "expression": "Amount * Quantity", "round_dp": 2 }),
    );
    assert!(ok, "{v}");
    let rid = v["result_id"].as_str().unwrap().to_string();

    let view = ws.table_view(Some(&rid), None, None, 0, 100).unwrap();
    assert_eq!(view["kind"], "result");
    assert_eq!(view["change"]["op"], "compute_column");
    let added = view["change"]["added_columns"].as_array().unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0], "Line Total");
    // same rows in/out for a per-row computation
    assert_eq!(view["change"]["source_row_count"], view["change"]["result_row_count"]);
    // display rows are strings, numeric column flagged for right-alignment
    assert_eq!(view["rows"][0].as_array().unwrap().last().unwrap(), "3751.2");

    // filter → row delta should differ
    let (v, _, ok) = tools::execute(
        &mut ws,
        "filter_rows",
        &json!({ "file": "sales.xlsx", "conditions": [{ "column": "Category", "op": "eq", "value": "Supplies" }] }),
    );
    assert!(ok);
    let fid = v["result_id"].as_str().unwrap().to_string();
    let fview = ws.table_view(Some(&fid), None, None, 0, 100).unwrap();
    assert_eq!(fview["change"]["source_row_count"], 5);
    assert_eq!(fview["change"]["result_row_count"], 3);
    assert!(fview["change"]["added_columns"].as_array().unwrap().is_empty());

    // a plain file view has no change block
    let file_view = ws.table_view(None, Some("sales.xlsx"), None, 0, 100).unwrap();
    assert_eq!(file_view["kind"], "file");
    assert!(file_view.get("change").is_none());
}

#[test]
fn new_chat_clears_results_keeps_files() {
    let (mut ws, _dir) = ws_with_sales();
    tools::execute(&mut ws, "filter_rows", &json!({
        "file": "sales.xlsx", "conditions": [{ "column": "Category", "op": "eq", "value": "Supplies" }]
    }));
    assert_eq!(ws.list()["results"].as_array().unwrap().len(), 1);
    ws.clear_results();
    assert_eq!(ws.list()["results"].as_array().unwrap().len(), 0);
    assert_eq!(ws.list()["files"].as_array().unwrap().len(), 1); // files kept
    // result ids restart at r1 after a clear
    let (v, _, _) = tools::execute(&mut ws, "sort_rows", &json!({ "file": "sales.xlsx", "column": "Amount" }));
    assert_eq!(v["result_id"], "r1");
}

#[test]
fn unknown_tool_and_missing_file_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut ws = Workspace::new(dir.path().join("exports"));
    let (v, _, ok) = tools::execute(&mut ws, "delete_everything", &json!({}));
    assert!(!ok);
    assert!(v["error"].as_str().unwrap().contains("unknown tool"));

    let (v, _, ok) = tools::execute(
        &mut ws,
        "get_schema",
        &json!({ "file": "ghost.xlsx" }),
    );
    assert!(!ok);
    assert!(v["error"].as_str().unwrap().contains("not found"));
}
