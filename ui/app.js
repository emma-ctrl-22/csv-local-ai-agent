// Ledger Local — frontend logic (no framework; talks to Rust over Tauri IPC).

const T = window.__TAURI__;
const invoke = T.core.invoke;
const listen = T.event.listen;
const dialog = T.dialog;
const opener = T.opener;

const $ = (id) => document.getElementById(id);
const el = (tag, cls, html) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
};
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) =>
  ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

// ---- app state ----
const state = {
  busy: false,
  tabs: [{ id: "welcome", kind: "welcome", label: "Welcome" }],
  active: "welcome",
  results: {},         // id -> {op}
  files: [],           // [{file, type, ...}]
  messageCount: 0,     // to detect the first (cold) message
  modelReady: false,
};

// ============================================================
// STATUS / HEALTH
// ============================================================
function setStatus(kind, text) {
  $("status-dot").className = `dot ${kind}`;
  $("status-text").textContent = text;
}

async function refreshHealth() {
  try {
    const h = await invoke("check_ollama");
    if (!h.online) { setStatus("offline", "Ollama offline"); state.modelReady = false; }
    else if (!h.model_available) { setStatus("partial", `pull ${h.configured_model}`); state.modelReady = false; }
    else { if (!state.busy && state.modelReady) setStatus("online", `${short(h.configured_model)} · ready`); state.modelReady = true; }

    const sel = $("model-select");
    if (h.models && h.models.length) {
      sel.hidden = false;
      sel.innerHTML = h.models.map((m) =>
        `<option value="${esc(m)}" ${m === h.configured_model || m.startsWith(h.configured_model + ":") ? "selected" : ""}>${esc(m)}</option>`).join("");
    } else sel.hidden = true;
    return h;
  } catch (e) { setStatus("offline", "Ollama offline"); return null; }
}
const short = (m) => m.length > 22 ? m.slice(0, 20) + "…" : m;

$("model-select").addEventListener("change", async (e) => {
  try { await invoke("set_model", { model: e.target.value }); state.modelReady = false; warmUp(); } catch {}
});

async function warmUp() {
  const h = await refreshHealth();
  if (!h || !h.online || !h.model_available) return;
  if (state.busy) return;
  setStatus("loading", `loading ${short(h.configured_model)}…`);
  try { await invoke("warm_model"); state.modelReady = true; setStatus("online", `${short(h.configured_model)} · ready`); }
  catch { setStatus("partial", "model not loaded"); }
}

// ============================================================
// NAVIGATOR
// ============================================================
async function refreshWorkspace(openNewest) {
  let ws;
  try { ws = await invoke("list_workspace"); } catch { return; }
  const prevResults = new Set(Object.keys(state.results));

  // files
  state.files = ws.files || [];
  const fl = $("file-list");
  fl.innerHTML = "";
  for (const f of state.files) {
    const isPdf = f.type === "pdf";
    const meta = isPdf ? `~${f.pages_approx}p`
      : `${(f.sheets || []).reduce((a, s) => a + (s.row_count || 0), 0)} rows`;
    const li = el("li", `nav-item ${isPdf ? "pdf" : ""}${state.active === "file:" + f.file ? " active" : ""}`,
      `<svg class="ni-ic" viewBox="0 0 16 16" aria-hidden="true"><rect x="3" y="1.5" width="10" height="13" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M5.5 5h5M5.5 8h5M5.5 11h3" stroke="currentColor" stroke-width="1.1"/></svg>` +
      `<span class="ni-name">${esc(f.file)}</span><span class="ni-badge">${esc(meta)}</span>`);
    li.onclick = () => openFileTab(f.file);
    fl.appendChild(li);
  }
  $("files-empty").style.display = state.files.length ? "none" : "";

  // results
  state.results = {};
  (ws.results || []).forEach((r) => { state.results[r.result_id] = { op: r.op, cols: r.columns, rows: r.row_count }; });
  const rl = $("result-list");
  rl.innerHTML = "";
  const ids = Object.keys(state.results);
  for (const id of ids) {
    const r = state.results[id];
    const fresh = openNewest && !prevResults.has(id);
    const li = el("li", `nav-item ${state.active === "result:" + id ? "active" : ""} ${fresh ? "fresh" : ""}`,
      `<svg class="ni-ic" viewBox="0 0 16 16" aria-hidden="true"><path d="M2 12l4-4 3 3 5-6" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>` +
      `<span class="ni-name">${esc(id)} · ${esc(r.op || "result")}</span><span class="ni-badge">${r.rows}</span>`);
    li.onclick = () => openResultTab(id);
    rl.appendChild(li);
  }
  $("results-empty").style.display = ids.length ? "none" : "";
  $("results-hint").textContent = ids.length ? `${ids.length}` : "";

  // auto-open the newest result so the user immediately sees the edit + changes
  if (openNewest) {
    const fresh = ids.filter((id) => !prevResults.has(id));
    if (fresh.length) openResultTab(fresh[fresh.length - 1]);
  }
}

// ============================================================
// TABS + VIEWER
// ============================================================
function renderTabs() {
  const strip = $("tabstrip");
  strip.innerHTML = "";
  for (const t of state.tabs) {
    const tab = el("div", `tab ${state.active === t.id ? "active" : ""}`);
    const closeable = t.kind !== "welcome";
    tab.innerHTML =
      (t.kind === "result" ? `<span class="tab-dot"></span>` : "") +
      `<span class="tab-label">${esc(t.label)}</span>` +
      (closeable ? `<span class="tab-close" title="Close">✕</span>` : "");
    tab.onclick = (e) => {
      if (e.target.classList.contains("tab-close")) { closeTab(t.id); return; }
      activate(t.id);
    };
    strip.appendChild(tab);
  }
}

function activate(id) {
  state.active = id;
  renderTabs();
  const t = state.tabs.find((x) => x.id === id);
  if (!t) return;
  if (t.kind === "welcome") renderWelcome();
  else if (t.kind === "file") loadAndRenderTable({ file: t.file, sheet: t.sheet }, t);
  else if (t.kind === "result") loadAndRenderTable({ result_id: t.result_id }, t);
  else if (t.kind === "activity") renderActivity();
  syncNavActive();
}

function syncNavActive() {
  document.querySelectorAll(".navigator .nav-item").forEach((n) => n.classList.remove("active"));
  // re-mark by re-rendering is heavy; simplest is to leave nav highlight to refreshWorkspace.
}

function upsertTab(tab) {
  const existing = state.tabs.find((t) => t.id === tab.id);
  if (existing) Object.assign(existing, tab);
  else state.tabs.push(tab);
  activate(tab.id);
}

function closeTab(id) {
  const idx = state.tabs.findIndex((t) => t.id === id);
  if (idx < 0) return;
  state.tabs.splice(idx, 1);
  if (state.active === id) activate(state.tabs[Math.max(0, idx - 1)].id);
  else renderTabs();
}

function openFileTab(file, sheet) {
  upsertTab({ id: "file:" + file, kind: "file", label: file, file, sheet });
}
function openResultTab(id) {
  const op = state.results[id]?.op || "result";
  upsertTab({ id: "result:" + id, kind: "result", label: `${id} · ${op}`, result_id: id });
}

// ---- viewer content ----
function renderWelcome() {
  const examples = [
    "List everything you can see",
    "Sum Amount by Vendor and show the totals",
    "Add a 15% VAT column and export it",
    "What's the total due on this invoice?",
  ];
  const body = $("viewer-body");
  body.innerHTML =
    `<div class="welcome">
       <h1>Your files, edited safely.</h1>
       <p>Open a spreadsheet or PDF from the left, then ask in the chat. Results appear here as a table — with the parts that changed highlighted, so you can see exactly what was edited before you export anything.</p>
       <div class="chips">${examples.map((x) => `<button class="chip-ex">${esc(x)}</button>`).join("")}</div>
       <div class="rules"><b>How the numbers stay right:</b>
         <ul>
           <li>Every calculation runs in exact decimals — the model never does the math itself.</li>
           <li>Your original files are never modified. Exports always create new files.</li>
           <li>Each result records what changed: added columns, filtered rows, group totals.</li>
         </ul>
       </div>
     </div>`;
  body.querySelectorAll(".chip-ex").forEach((c) =>
    c.onclick = () => { $("ask-input").value = c.textContent; $("ask-input").focus(); autoGrow(); });
}

function skeleton() {
  const rows = Array.from({ length: 9 }).map(() =>
    `<div class="sk-row">${Array.from({ length: 5 }).map(() => `<div class="sk-cell"></div>`).join("")}</div>`).join("");
  return `<div class="skeleton">${rows}</div>`;
}

async function loadAndRenderTable(src, tab) {
  const body = $("viewer-body");
  body.innerHTML = skeleton();
  let view;
  try { view = await invoke("get_table", { ...src, offset: 0, limit: 400 }); }
  catch (e) { body.innerHTML = `<div class="err-note">Couldn't open that. ${esc(e)}</div>`; return; }
  if (state.active !== tab.id) return; // user switched away while loading

  if (view.kind === "pdf") { body.innerHTML = renderPdf(view); return; }
  body.innerHTML = (view.kind === "result" ? renderChanges(view.change) : "") +
    renderGridMeta(view, tab) + renderGrid(view);
  // wire sheet switching
  body.querySelectorAll(".sheet-tab").forEach((st) =>
    st.onclick = () => { tab.sheet = st.dataset.sheet; loadAndRenderTable({ file: tab.file, sheet: tab.sheet }, tab); });
}

function renderChanges(ch) {
  if (!ch) return "";
  const delta = ch.source_row_count !== ch.result_row_count
    ? `<span class="pill delta">${ch.source_row_count} → ${ch.result_row_count} rows</span>`
    : `<span class="pill">${ch.result_row_count} rows</span>`;
  const added = (ch.added_columns || []).map((c) => `<span class="pill add">＋ ${esc(c)}</span>`).join("");
  const src = ch.source_label ? `<span class="src">from ${esc(ch.source_label)}</span>` : "";
  return `<div class="changes"><span class="op">${esc(ch.op || "result")}</span>${delta}${added}${src}</div>`;
}

function renderGridMeta(view, tab) {
  let sheets = "";
  if (view.sheets && view.sheets.length > 1) {
    sheets = `<div class="sheet-tabs">` + view.sheets.map((s) =>
      `<button class="sheet-tab ${s === view.active_sheet ? "active" : ""}" data-sheet="${esc(s)}">${esc(s)}</button>`).join("") + `</div>`;
  }
  return `<div class="grid-meta">${sheets}<span>${view.columns.length} columns · ${view.total_rows} rows</span></div>`;
}

function renderGrid(view) {
  const added = new Set((view.change?.added_columns || []).map((c) => c.toLowerCase()));
  const head = "<th class='rownum'></th>" + view.columns.map((c, i) => {
    const cls = (view.col_numeric[i] ? "n " : "") + (added.has(c.toLowerCase()) ? "added" : "");
    return `<th class="${cls.trim()}">${esc(c)}</th>`;
  }).join("");
  const rows = view.rows.map((r, ri) => {
    const cells = r.map((cell, i) => {
      const cls = (view.col_numeric[i] ? "n " : "") + (added.has(view.columns[i]?.toLowerCase()) ? "added" : "");
      return `<td class="${cls.trim()}">${esc(cell)}</td>`;
    }).join("");
    return `<tr><td class="rownum">${view.offset + ri + 1}</td>${cells}</tr>`;
  }).join("");
  const foot = view.rows.length < view.total_rows
    ? `<div class="grid-foot">Showing ${view.rows.length} of ${view.total_rows} rows. Ask me to export for the full file.</div>` : "";
  return `<div class="grid-wrap"><table class="grid"><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>${foot}`;
}

function renderPdf(view) {
  return `<div class="grid-meta"><span>PDF · ~${view.pages_approx} page(s)${view.truncated ? " · preview truncated" : ""}</span></div>` +
    `<div class="pdf-view"><pre>${esc(view.text)}</pre></div>`;
}

async function renderActivity() {
  const body = $("viewer-body");
  body.innerHTML = skeleton();
  let entries;
  try { entries = await invoke("get_journal"); } catch { entries = []; }
  if (state.active !== "activity") return;
  if (!entries.length) { body.innerHTML = `<div class="welcome"><p class="muted">No operations yet. Every calculation and export will be logged here — and saved to journal.jsonl in your export folder.</p></div>`; return; }
  body.innerHTML = `<div class="activity">` + entries.map((e) => {
    const sum = e.summary.replace(/^FAILED:\s*/, "");
    return `<div class="act-row ${e.ok ? "" : "failed"}"><div class="act-seq">${e.seq}</div>` +
      `<div><div class="act-tool">${esc(e.tool)}</div><div class="act-sum">${esc(sum)}</div></div>` +
      `<div class="act-time">${esc(e.time)}</div></div>`;
  }).join("") + `</div>`;
}

$("show-activity").onclick = () => upsertTab({ id: "activity", kind: "activity", label: "Activity log" });

// ============================================================
// FILE OPEN / EXPORT
// ============================================================
$("open-files").onclick = async () => {
  if (state.busy) return;
  const btn = $("open-files");
  let sel;
  try {
    sel = await dialog.open({ multiple: true, filters: [{ name: "Files", extensions: ["xlsx", "xls", "xlsm", "csv", "tsv", "pdf"] }] });
  } catch { return; }
  if (!sel) return;
  const paths = Array.isArray(sel) ? sel : [sel];
  btn.disabled = true; btn.querySelector("span").textContent = "Opening…";
  try {
    const res = await invoke("load_files", { paths });
    await refreshWorkspace(false);
    const first = (res.loaded || [])[0];
    if (first && first.file) openFileTab(first.file);
  } catch (e) {
    addMessage("assistant", `I couldn't open that file. ${e}`);
  } finally {
    btn.disabled = false; btn.querySelector("span").textContent = "Open files";
  }
};

$("reveal-exports").onclick = async () => {
  try { await opener.openPath(await invoke("export_dir")); }
  catch { addMessage("assistant", "The export folder is created after your first export."); }
};

// ============================================================
// NEW CHAT
// ============================================================
$("new-chat").onclick = async () => {
  if (state.busy) return;
  try { await invoke("new_chat"); } catch {}
  // clear chat
  $("messages").innerHTML =
    `<div class="welcome-chat"><h2>New chat</h2><p class="muted">Your files are still open on the left. Previous results were cleared — ask something to begin again.</p></div>`;
  // close result + activity tabs, keep files
  state.tabs = state.tabs.filter((t) => t.kind === "file" || t.kind === "welcome");
  if (!state.tabs.find((t) => t.id === state.active)) state.active = state.tabs[0]?.id || "welcome";
  state.messageCount = 0;
  await refreshWorkspace(false);
  activate(state.active);
};

// ============================================================
// CHAT
// ============================================================
function addMessage(who, text) {
  const w = $("welcome-chat"); if (w) w.remove();
  const m = el("div", `msg ${who}`);
  m.innerHTML = `<div class="who">${who === "user" ? "You" : "Ledger"}</div><div class="bubble">${renderBody(text)}</div>`;
  $("messages").appendChild(m);
  scrollChat();
  return m;
}
function scrollChat() { const c = $("messages"); c.scrollTop = c.scrollHeight; }

// light markdown: paragraphs + pipe tables
function renderBody(text) {
  const lines = String(text).split("\n");
  let out = "", i = 0;
  while (i < lines.length) {
    if (/^\s*\|.*\|\s*$/.test(lines[i]) && i + 1 < lines.length && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1])) {
      const header = row(lines[i]); i += 2; const body = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) { body.push(row(lines[i])); i++; }
      out += `<table><thead><tr>${header.map((h) => `<th class="${numish(h) ? "n" : ""}">${esc(h)}</th>`).join("")}</tr></thead><tbody>${
        body.map((r) => `<tr>${r.map((c) => `<td class="${numish(c) ? "n" : ""}">${esc(c)}</td>`).join("")}</tr>`).join("")}</tbody></table>`;
      continue;
    }
    let line = esc(lines[i]).replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>");
    out += `<p>${line}</p>`; i++;
  }
  return out;
}
const row = (l) => l.trim().replace(/^\||\|$/g, "").split("|").map((s) => s.trim());
const numish = (s) => /^[-(]?\s*(?:GHS|USD|\$|€|£)?\s*[\d,]+(?:\.\d+)?\s*\)?%?$/.test(String(s).trim());

// ---- the working card (loading states) ----
let work = null; // { card, timeline, current, elapsedEl, timer, t0 }

function startWorking() {
  const w = $("welcome-chat"); if (w) w.remove();
  const card = el("div", "working");
  const firstMsg = state.messageCount === 0;
  card.innerHTML =
    `<div class="working-head"><span class="spinner"></span><span class="working-title">Working…</span><span class="working-elapsed">0:00</span></div>` +
    `<div class="working-substatus">thinking…</div>` +
    (firstMsg ? `<div class="cold-hint">First message loads the model into memory — this reply is slower. Later replies are quicker.</div>` : "") +
    `<ul class="timeline"></ul>`;
  $("messages").appendChild(card);
  scrollChat();
  const t0 = Date.now();
  const elapsedEl = card.querySelector(".working-elapsed");
  const timer = setInterval(() => {
    const s = Math.floor((Date.now() - t0) / 1000);
    elapsedEl.textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  }, 500);
  work = { card, timeline: card.querySelector(".timeline"), sub: card.querySelector(".working-substatus"), current: null, timer };
}

function workStep(text) {
  if (!work) return;
  if (text === "thinking…" || text === "planning next step…") { work.sub.textContent = text; return; }
  if (/^running\s+/.test(text)) {
    // finalize any open row, then open a new in-progress row
    finalizeCurrent(true);
    const li = el("li", "tl-row pending", `<span class="tl-ic tl-run"></span><span class="tl-text">${esc(text.replace(/…$/, ""))}</span>`);
    work.timeline.appendChild(li); work.current = li; scrollChat(); return;
  }
  // a summary line — attach to the current row and finalize it
  if (work.current) {
    const failed = /^FAILED/i.test(text);
    work.current.querySelector(".tl-text").textContent = text.replace(/^FAILED:\s*/, "");
    work.current.querySelector(".tl-ic").className = `tl-ic ${failed ? "tl-fail" : "tl-done"}`;
    work.current.querySelector(".tl-ic").innerHTML = failed ? "✕" : "✓";
    work.current.classList.remove("pending");
    work.current = null; scrollChat();
  } else {
    work.sub.textContent = text;
  }
}
function finalizeCurrent(ok) {
  if (work && work.current) {
    work.current.querySelector(".tl-ic").className = `tl-ic ${ok ? "tl-done" : "tl-fail"}`;
    work.current.querySelector(".tl-ic").innerHTML = ok ? "✓" : "✕";
    work.current.classList.remove("pending");
    work.current = null;
  }
}
function stopWorking() {
  if (!work) return;
  clearInterval(work.timer);
  finalizeCurrent(true);
  work.card.remove();
  work = null;
}

listen("agent-step", (ev) => workStep((ev.payload && ev.payload.text) || ""));

// ---- send ----
$("composer").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (state.busy) return;
  const text = $("ask-input").value.trim();
  if (!text) return;
  addMessage("user", text);
  $("ask-input").value = ""; autoGrow();
  setBusy(true);
  setStatus("loading", "thinking…");
  startWorking();
  try {
    const reply = await invoke("send_message", { text });
    stopWorking();
    addMessage("assistant", reply);
    state.messageCount++;
    await refreshWorkspace(true); // open newest result → shows the edit + changes
  } catch (e) {
    stopWorking();
    const note = el("div", "err-note");
    note.innerHTML = `${esc(String(e))}<div class="retry"><button class="btn" id="retry-btn">Try again</button></div>`;
    $("messages").appendChild(note);
    note.querySelector("#retry-btn").onclick = () => { $("ask-input").value = text; autoGrow(); $("ask-input").focus(); note.remove(); };
    scrollChat();
  } finally {
    setBusy(false);
    refreshHealth();
  }
});

function setBusy(v) {
  state.busy = v;
  $("ask-input").disabled = v;
  $("send").disabled = v;
  $("new-chat").disabled = v;
  $("open-files").disabled = v;
  if (!v) { $("ask-input").focus(); if (state.modelReady) refreshHealth(); }
}

// textarea auto-grow + Enter-to-send
const ask = $("ask-input");
function autoGrow() { ask.style.height = "auto"; ask.style.height = Math.min(ask.scrollHeight, 160) + "px"; }
ask.addEventListener("input", autoGrow);
ask.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("composer").requestSubmit(); }
});

// ============================================================
// BOOT
// ============================================================
(async function boot() {
  renderTabs();
  activate("welcome");
  await refreshWorkspace(false);
  await warmUp();           // preload the model so the first ask is faster
  ask.focus();
  setInterval(() => { if (!state.busy) refreshHealth(); }, 12000);
})();
