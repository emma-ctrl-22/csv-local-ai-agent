//! The agent loop. One user turn may take several model rounds:
//! model -> tool_calls -> execute -> tool results -> model -> ... -> answer.
//!
//! Hard rules enforced here:
//!   * bounded rounds (no runaway loops)
//!   * tool errors are fed back as structured results, never crash the turn
//!   * everything is audited in the workspace journal

use crate::ollama::{ChatMessage, OllamaClient};
use crate::tools;
use crate::workspace::Workspace;
use crate::Result;

pub const MAX_ROUNDS: usize = 8;

/// UI progress callback: called with a short line per step
/// ("running aggregate…", "exported to …").
pub trait StepSink {
    fn step(&self, text: &str);
}

pub struct NullSink;
impl StepSink for NullSink {
    fn step(&self, _: &str) {}
}

pub fn system_prompt() -> String {
    r#"You are the assistant inside Ledger Local, an offline accounting desktop app. You help an accountant work with the Excel/CSV/PDF files loaded in the workspace.

NON-NEGOTIABLE RULES:
1. NEVER do arithmetic yourself — not even adding two numbers. Always use the `calculate` tool or a table tool (aggregate, compute_column). Your own arithmetic is not trusted.
2. NEVER invent file names, sheet names, column names, or numbers. If unsure what exists, call `list_files` or `get_schema` first.
3. All amounts you report must come verbatim from tool results.
4. Tools never modify source files. Exports create new files; tell the user the saved path.
5. If a tool returns an error, read it, fix your arguments, and retry (different column name, etc.). Do not apologize repeatedly; just correct course.
6. Chain operations through result_id values (filter -> r1, aggregate r1 -> r2, export r2).
7. Answer concisely and in plain language. Show small result tables inline; offer export_xlsx for anything bigger.
8. If the user asks for something these tools cannot do (edit a source file in place, OCR a scanned PDF, forecasting), say so plainly and suggest what you can do instead.
9. Currency: report amounts exactly as computed; don't convert currencies unless the user provides a rate — then use `calculate` with it."#
        .to_string()
}

/// Run one user turn. `history` is the persistent conversation (system prompt
/// is ensured at index 0). Returns the assistant's final text.
pub fn run_turn(
    ws: &mut Workspace,
    client: &OllamaClient,
    history: &mut Vec<ChatMessage>,
    user_text: &str,
    sink: &dyn StepSink,
) -> Result<String> {
    if history.first().map(|m| m.role.as_str()) != Some("system") {
        history.insert(0, ChatMessage::system(system_prompt()));
    }

    // Ground the model in what actually exists right now.
    let ws_state = serde_json::to_string(&ws.list()).unwrap_or_default();
    let grounded = format!("{user_text}\n\n[workspace state: {ws_state}]");
    history.push(ChatMessage::user(grounded));

    let defs = tools::definitions();

    for round in 0..MAX_ROUNDS {
        sink.step(if round == 0 { "thinking…" } else { "planning next step…" });
        let msg = client.chat(history, Some(&defs))?;

        let calls = msg.tool_calls.clone().unwrap_or_default();
        history.push(msg.clone());

        if calls.is_empty() {
            let text = msg.content.trim().to_string();
            if text.is_empty() {
                return Ok("(the model returned an empty reply — try rephrasing)".into());
            }
            return Ok(text);
        }

        for call in calls {
            let name = call.function.name.clone();
            // Ollama sends arguments as an object; some models emit a string.
            let args = match &call.function.arguments {
                serde_json::Value::String(s) => {
                    serde_json::from_str(s).unwrap_or(serde_json::Value::Object(Default::default()))
                }
                other => other.clone(),
            };
            sink.step(&format!("running {name}…"));
            let (payload, summary, _ok) = tools::execute(ws, &name, &args);
            sink.step(&summary);
            let content =
                serde_json::to_string(&payload).unwrap_or_else(|_| "{\"error\":\"serialization\"}".into());
            history.push(ChatMessage::tool(&name, content));
        }
    }

    Ok(format!(
        "I stopped after {MAX_ROUNDS} tool steps without finishing — the request may be too complex for one turn. The journal shows what completed; try breaking the task into smaller asks."
    ))
}

#[cfg(test)]
mod tests {
    // Orchestrator integration is exercised end-to-end in tests/engine.rs
    // via direct tool execution; live-Ollama tests can't run in CI.
}
