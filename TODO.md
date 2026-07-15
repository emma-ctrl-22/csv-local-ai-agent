# TODO — your side of the bargain

The engine is built and tested. This file is everything left to get the app
running on your machine and, eventually, onto your friend's. It's ordered:
do **Part 1** to see it run, **Part 2** to give it to your friend, **Part 3**
when you want to close the known gaps.

Times assume you've used a terminal before but haven't shipped a Tauri app.

---

## Part 1 — Get it running on your machine (~1 hour, mostly downloads)

### 1.1 Install Rust ≥ 1.77.2 (this environment only had 1.75)

Tauri 2 won't build on 1.75. Use rustup, not your OS package:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# restart your shell, then:
rustc --version    # must be 1.77.2 or newer
```

> The `core/` crate is pinned to compile on 1.75, but once you have a newer
> toolchain you can delete `core/Cargo.lock` and let cargo pick fresher deps if
> you like. Not required.

### 1.2 Install the Tauri prerequisites for your OS

- **Windows:** install the **WebView2 runtime** (often already present on
  Win10/11) and the **Microsoft C++ Build Tools** (the "Desktop development
  with C++" workload).
- **macOS:** `xcode-select --install`.
- **Linux (Debian/Ubuntu):** `webkit2gtk`, `libgtk-3-dev`, `libayatana-appindicator3-dev`,
  `librsvg2-dev`, `build-essential`, `curl`, `wget`, `file`, `libssl-dev`.

The current, authoritative list is on the Tauri site under *Prerequisites* —
check it, since package names drift.

### 1.3 Install the Tauri CLI

```bash
cargo install tauri-cli --version "^2"
cargo tauri --version    # confirm
```

### 1.4 Install Ollama and pull the model

Install Ollama for your OS, then:

```bash
ollama pull qwen2.5:7b-instruct    # ~4.5 GB download, needs ~16 GB RAM to run comfortably
ollama serve                       # if it isn't already running as a service
```

You can pull other tool-calling models (e.g. `llama3.1:8b-instruct`) and pick
them from the dropdown in the app. Whatever you pick must support **function
calling**, or the model can't drive the tools.

### 1.5 Run it

From the repo root:

```bash
cargo tauri dev
```

First compile is slow (Tauri is large). After that it's fast. The status dot
top-right tells you if Ollama is reachable and the model is present. Open a
spreadsheet or PDF from the left pane and start asking.

**Sanity checks to try** (make a small test spreadsheet first):
- "List everything you can see."
- "Sum Amount by Vendor and show me the totals."
- "Add a 15% VAT column and export it."
- With a PDF invoice: "What amounts are on invoice.pdf? What's the total due?"

---

## Part 2 — Package it for your friend (~1–2 hours the first time)

### 2.1 Real app icons

Right now `src-tauri/icons/` only has a placeholder note. Make one square PNG
(1024×1024) and run:

```bash
cargo tauri icon path/to/your-icon.png
```

That generates every icon size/format the installer needs.

### 2.2 Build the installer

```bash
cargo tauri build
```

This produces a native installer in `src-tauri/target/release/bundle/`
(`.msi`/`.exe` on Windows, `.dmg` on macOS, `.deb`/`.AppImage` on Linux).

### 2.3 The Ollama question (important — decide this before shipping)

The installer above bundles **the app**, not the model. As written, your friend
also needs Ollama installed and the model pulled. Two honest options:

- **Simplest:** on their machine, install Ollama + `ollama pull qwen2.5:7b-instruct`
  once, same as you did. The app just talks to it. Totally fine for one friend.
- **"True one-click offline":** bundle Ollama (or an embedded `llama.cpp`) and
  the model file as a Tauri **sidecar** so there's nothing else to install. This
  is more work (external binaries, per-OS packaging, ~5 GB installer) and is
  listed in Part 3. Don't do this for v1 unless you need it.

### 2.4 Check the hardware — and expect a slow CPU

The target machine is **20 GB RAM, i3 CPU**. Good news: 20 GB is plenty for the
7B model (it wants ~16 GB). The constraint is the **i3 CPU** — with no dedicated
GPU, tokens are generated on the processor, so replies will be *slow* (think
tens of seconds, not instant). Nothing is broken; that's just local inference on
a modest CPU.

The app is built to make that bearable: it **preloads the model** on launch so
the first message isn't cold, shows a **live step timeline + elapsed timer**
while working, and warns on the first message that it's loading into memory. If
it's still too slow for comfort, drop to a smaller tool-calling model
(e.g. `llama3.2:3b-instruct` or `qwen2.5:3b-instruct`) and pick it from the
dropdown — you trade some reasoning quality for noticeably faster replies. Make
sure whatever you choose lists `tools` in its capabilities.

---

## Part 3 — Known gaps to close later

These are real limitations of v1, roughly in priority order. None of them block
Parts 1–2.

### 3.1 Scanned PDFs need OCR
`extract_pdf_text` / `extract_pdf_amounts` only work on **digitally generated**
PDFs (ones with a real text layer). A scanned/photographed invoice is just an
image — extraction returns nothing, and the error already says so. To handle
those you'll need OCR: bundle **Tesseract** (call it on the PDF's rendered
pages) or route the image through a local **vision model**. Sizeable feature;
scope it on its own.

### 3.2 Editing an .xlsx *in place* (preserving formatting) — the real gap
There is **no Rust crate** that edits an existing workbook while preserving its
formatting, formulas, and styles. Ledger Local sidesteps this by only ever
*writing new* .xlsx files (a clean data table). If your friend needs "change
these cells in *this* workbook and keep everything else exactly as it was," the
practical path is a small **Python sidecar using `openpyxl`**, called from Rust
for that one operation. Flagged here so it's a conscious choice, not a surprise.

### 3.3 Streaming the final answer token-by-token
You already get **live progress** while the model works — a step timeline (each
tool call, running → done), an elapsed timer, and a cold-start note on the first
message. What you don't yet get is the final answer *text* streaming in
word-by-word; it arrives whole. If you want that, switch `ollama.rs` to
`stream: true` for the final (no-tool-call) response and emit chunks over the
same `agent-step` event channel the UI already listens on. On a slow i3 this is
a real perceived-speed win, so it's worth doing early.

### 3.4 Bundle Ollama as a sidecar
See 2.3. This is what turns "install two things" into "run one installer."
Involves shipping platform-specific binaries and the model weights, and wiring
them as a Tauri sidecar that the app starts and stops.

### 3.5 More tools as you find gaps
The tool set is deliberately small and concrete. When you hit a recurring task
it can't express (VAT reports across sheets, reconciliation between two files,
date-range filtering), add a tool in `core/src/tools/` — the pattern is:
schema in `tools/mod.rs`, implementation next to the others, wire it into
`dispatch`, add a test in `core/tests/engine.rs`. The tests are the spec.

### 3.6 Test with real files early
The engine is tested against synthetic spreadsheets/invoices. Your friend's
actual files will have quirks (merged header cells, multi-row headers, totals
baked into the data, odd date formats, mixed currencies). Get five real
(anonymised) files in front of it as soon as it runs — that's where the next
round of fixes will come from, and it's worth doing before you package anything.

---

## Quick reference: verify the engine yourself

Even without Tauri, you can run the tested engine right now on any Rust:

```bash
cd core
cargo test                       # 14 tests: expression eval + end-to-end engine
cargo run --example pdf_check    # PDF amount extraction on a sample invoice
```
