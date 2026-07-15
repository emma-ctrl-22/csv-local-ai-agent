use ledger_core::tools;
use ledger_core::workspace::Workspace;
use serde_json::json;

fn main() {
    let mut ws = Workspace::new("/tmp/exports".into());
    ws.load_path(std::path::Path::new("/tmp/invoice.pdf")).expect("load pdf");
    let (v, s, ok) = tools::execute(&mut ws, "extract_pdf_amounts", &json!({"file":"invoice.pdf"}));
    println!("ok={ok} summary={s}\n{}", serde_json::to_string_pretty(&v).unwrap());
    let (v, _, ok) = tools::execute(&mut ws, "extract_pdf_text", &json!({"file":"invoice.pdf","query":"total"}));
    println!("text-query ok={ok}\n{}", serde_json::to_string_pretty(&v).unwrap());
}
