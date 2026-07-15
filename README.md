# Ledger Local

An offline desktop assistant for anyone who works with spreadsheets and PDFs.
It loads Excel / CSV / PDF files, and a **local** language model (via Ollama)
helps work with them — filtering, totalling, adding columns, reading invoices —
in plain language. Nothing leaves the machine.

The interface is built around **seeing what changed**: open a file and it opens
in a tabbed viewer; ask a question and the result opens beside it as a table,
with new columns highlighted and row-count changes shown, so you can inspect the
edit before exporting anything.

The single most important design decision: **the language model never does
arithmetic.** It plans; deterministic Rust does every calculation with exact
decimal math. A model that hallucinates `1,250.40 + 980.10 = 2,220.50` is
useless. So the model's only power is to *call tools*, and the tools are the
ones doing the sums.

---

## How it works

```
  ┌─────────────┐     tool calls      ┌────────────────────────┐
  │   Ollama    │ ──────────────────▶ │   ledger-core (Rust)   │
  │ (local LLM) │ ◀────────────────── │  deterministic engine  │
  └─────────────┘   tool results      └────────────────────────┘
        ▲                                        │
        │ HTTP (localhost:11434)                 │ reads files, does math
        │                                        ▼
  ┌─────┴───────────────────────────────────────────────────────┐
  │                    Tauri app (this repo)                     │
  │   Rust commands  ◀──IPC──▶  HTML/CSS/JS ledger-book UI       │
  └─────────────────────────────────────────────────────────────┘
```

One user question becomes an **agent loop**: the model reads the workspace
state, emits a tool call (e.g. `aggregate`), the engine runs it and returns a
structured result, and the loop repeats until the model has an answer. The
loop is bounded (8 steps) so it can't run away.

### The tools the model can call

| Tool | What it does |
|------|--------------|
| `list_files` | See every loaded file, its sheets/columns/rows, and derived results |
| `get_schema` | Columns + sample rows of a sheet or result |
| `filter_rows` | Keep rows matching conditions (numeric-aware) |
| `aggregate` | sum / avg / min / max / count, with optional group-by — exact decimals |
| `sort_rows` | Sort by a column, optional top-N |
| `compute_column` | Add a per-row derived column from an arithmetic expression |
| `calculate` | Evaluate one arithmetic expression in exact decimals |
| `show_rows` | Inspect actual rows |
| `export_xlsx` | Write a result to a **new** .xlsx in the export folder |
| `extract_pdf_text` | Read text from a PDF, optionally filtered |
| `extract_pdf_amounts` | Pull monetary amounts (GHS/USD, `1,234.50`, `(negatives)`) with context |

Every tool call is **validated against the real workspace first** (a typo'd
column name comes back as a readable error the model can correct), and every
call — success or failure — is written to an append-only **journal** shown in
the right rail and saved to `journal.jsonl`.

### Safety properties baked in

- **Money is `Decimal`, never `f64`.** `0.1 + 0.2` is exactly `0.3` here.
- **Source files are never modified.** Exports always create new files;
  re-exporting the same name yields ` (1)`, ` (2)`, … instead of overwriting.
- **Numbers stored as text** (`"1,200.50"`, `"GHS 1,200"`, `"(500)"`) are
  parsed leniently so real-world spreadsheets work.
- **Errors are recoverable,** not fatal — they're fed back to the model as
  structured tool results so it can fix its arguments and retry.

---

## Project layout

```
ledger-local/
├── core/               # ledger-core: the deterministic engine (pure Rust, no Tauri)
│   ├── src/
│   │   ├── lib.rs          # Cell / Table types, lenient decimal parsing, errors
│   │   ├── mathexpr.rs     # safe decimal expression evaluator (the model never does math)
│   │   ├── workspace.rs    # loaded files, derived results, source-file protection
│   │   ├── audit.rs        # append-only journal → journal.jsonl
│   │   ├── ollama.rs       # minimal blocking client for the local Ollama API
│   │   ├── orchestrator.rs # the agent loop + system prompt
│   │   └── tools/          # tool schemas + deterministic implementations
│   ├── tests/engine.rs # end-to-end tests (xlsx roundtrip, chaining, exact totals)
│   └── examples/pdf_check.rs
├── src-tauri/          # the desktop shell (commands, events, config)
└── ui/                 # the ledger-book interface (index.html, styles.css, app.js)
```

The engine is deliberately a **separate crate with no Tauri dependency**, so it
compiles and tests on any Rust toolchain and could later be reused behind a CLI
or a different shell (e.g. swapping Ollama for embedded `llama.cpp`).

---

## In the app

Three panes:

- **Left — navigator.** Your open files and the derived results (`r1`, `r2`…),
  each tagged with the operation that made it. Click any of them to open it in
  the viewer. Buttons for opening files, revealing the export folder, and the
  activity log.
- **Center — viewer.** A tabbed data grid. Files render as a spreadsheet (with a
  sheet switcher for multi-sheet workbooks); PDFs render their extracted text.
  A **result** renders with a *changes* banner — the operation, a `12 → 3 rows`
  delta chip, and `＋ column` chips — and the added columns are highlighted green
  in the grid, so the edit is visible at a glance.
- **Right — chat.** Ask in plain language. While the model works you get a live
  **timeline** of each tool call (running → ✓/✕), an elapsed timer, and — on the
  first message of a session — a note that the model is loading into memory.

**New chat** (top right) clears the conversation and derived results but keeps
your files open, so you can start a fresh line of questions on the same data.

---

## Status

**The engine is done and tested.** 14 tests pass (5 expression-evaluator unit
tests + 9 end-to-end engine tests), and PDF amount extraction has been verified
against a real generated invoice. The engine compiles on Rust 1.75.

**The Tauri shell and UI are written but not yet compiled here** — Tauri 2
needs a newer Rust than this build environment has (≥ 1.77.2) plus system
libraries and Ollama, which you install on your own machine.

Everything you need to do to run it is in **[TODO.md](./TODO.md)**.

---

## The recommended model

Qwen 2.5 7B Instruct (`qwen2.5:7b-instruct`) — a strong tool-caller that fits
in ~4.5 GB (Q4) and runs on a 16 GB machine. It's the default; you can pick any
installed model from the dropdown in the app.
