import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

const STATUS_ORDER = {
  "needs-approval": 8,
  "awaiting-input": 7,
  "needs-you": 6,
  working: 5,
  thinking: 4,
  idle: 3,
  starting: 2,
  exited: 1,
};

const STATUS_META = {
  "needs-approval": { glyph: "!", label: "Needs approval", tone: "peach" },
  "awaiting-input": { glyph: "?", label: "Awaiting input", tone: "yellow" },
  "needs-you": { glyph: "!", label: "Needs you", tone: "peach" },
  working: { glyph: "◒", label: "Working", tone: "yellow" },
  thinking: { glyph: "✦", label: "Thinking", tone: "mauve" },
  idle: { glyph: "·", label: "Idle", tone: "surface2" },
  starting: { glyph: "…", label: "Starting", tone: "sapphire" },
  exited: { glyph: "×", label: "Exited", tone: "overlay0" },
};

const MODEL_SUGGESTIONS = {
  claude: ["opus", "sonnet", "haiku"],
  codex: ["gpt-5.1-codex", "gpt-5.1-codex-mini", "gpt-5.1"],
};

const state = {
  sessions: [],
  focused: null,
  presets: [],
  extraDirs: [],
  attentionOnly: false,
  terminal: null,
  fitAddon: null,
  previewTimer: null,
  attentionToasts: new Map(),
};

const $ = (id) => document.getElementById(id);

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function invokeArgs(spec) {
  return { spec, configuredPath: null };
}

function showToast(message, tone = "error") {
  const toast = document.createElement("div");
  toast.className = `toast toast-${tone}`;
  toast.textContent = message;
  $("toast-region").append(toast);
  requestAnimationFrame(() => toast.classList.add("toast-visible"));
  setTimeout(() => {
    toast.classList.remove("toast-visible");
    setTimeout(() => toast.remove(), 240);
  }, 4200);
}

function showAttentionToast(notification) {
  if (state.attentionToasts.has(notification.dedup_key)) return;
  const session = state.sessions.find((item) => item.id === notification.session_id);
  const meta = STATUS_META[notification.status] ?? STATUS_META["needs-you"];
  const toast = document.createElement("button");
  toast.type = "button";
  toast.className = "toast toast-attention toast-visible";
  toast.textContent = `${session?.name ?? notification.session_id} · ${meta.label} · ${folderLabel(notification.group_key)}`;
  toast.title = "Focus session";
  toast.addEventListener("click", () => void focusSession(notification.session_id));
  $("toast-region").append(toast);
  state.attentionToasts.set(notification.dedup_key, { toast, sessionId: notification.session_id });
}

function retractAttentionToast(dedupKey) {
  const entry = state.attentionToasts.get(dedupKey);
  if (!entry) return;
  state.attentionToasts.delete(dedupKey);
  entry.toast.classList.remove("toast-visible");
  setTimeout(() => entry.toast.remove(), 240);
}

function systemTimeMs(value) {
  if (typeof value === "number") return value;
  if (value && typeof value.secs_since_epoch === "number") {
    return value.secs_since_epoch * 1000 + Math.floor((value.nanos_since_epoch ?? 0) / 1e6);
  }
  return Date.now();
}

function dwell(value) {
  const seconds = Math.max(0, Math.floor((Date.now() - systemTimeMs(value)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

function folderLabel(path) {
  const parts = String(path ?? "").split(/[\\/]/).filter(Boolean);
  return parts.at(-1) ?? path ?? "—";
}

function sortedSessions() {
  return [...state.sessions].sort((a, b) => {
    const status = (STATUS_ORDER[b.status] ?? 0) - (STATUS_ORDER[a.status] ?? 0);
    if (status !== 0) return status;
    return systemTimeMs(a.status_since) - systemTimeMs(b.status_since);
  });
}

function renderSummary() {
  const live = state.sessions.filter((session) => session.status !== "exited").length;
  const needsYou = state.sessions.filter((session) => ["needs-you", "needs-approval", "awaiting-input"].includes(session.status)).length;
  const working = state.sessions.filter((session) => ["working", "thinking"].includes(session.status)).length;
  $("fleet-summary").innerHTML = `<span class="summary-item"><b>${live}</b> live</span><span class="summary-separator">/</span><span class="summary-item summary-attention"><b>${needsYou}</b> needs you</span><span class="summary-separator">/</span><span class="summary-item"><b>${working}</b> active</span>`;
  $("fleet-count").textContent = `${state.sessions.length} tracked`;
}

function renderRows() {
  renderSummary();
  const filter = $("filter-input").value.trim().toLowerCase();
  const sessions = sortedSessions().filter((session) => {
    if (state.attentionOnly && !isAttention(session)) return false;
    if (!filter) return true;
    return [session.name, session.cwd, session.agent, session.model, session.last_line]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(filter);
  });
  const list = $("fleet-list");
  $("empty-state").classList.toggle("empty-state-hidden", state.sessions.length > 0);
  list.classList.toggle("fleet-list-hidden", state.sessions.length === 0);
  list.innerHTML = sessions.map(renderRow).join("");
  for (const row of list.querySelectorAll(".fleet-row")) {
    row.addEventListener("click", () => focusSession(row.dataset.id));
    for (const button of row.querySelectorAll("button[data-action]")) {
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        rowAction(button.dataset.action, row.dataset.id, row);
      });
    }
    const reply = row.querySelector("input[data-reply]");
    if (reply) {
      reply.addEventListener("click", (event) => event.stopPropagation());
      reply.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          rowAction("reply", row.dataset.id, row);
        }
      });
    }
  }
}

function isAttention(session) {
  return ["needs-approval", "awaiting-input", "needs-you"].includes(session.status);
}

function renderRow(session) {
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  const active = session.id === state.focused ? " row-focused" : "";
  const unread = session.unread ? " row-unread" : "";
  const agentLabel = session.agent === "codex" ? "CX" : "CC";
  const model = session.model || "default";
  const effort = session.effort || "—";
  const pinLabel = session.pinned ? "Unpin" : "Pin";
  const revive = session.status === "exited" && session.resume_id
    ? `<button type="button" data-action="revive" class="row-action" title="Revive with native resume">↻</button>`
    : "";
  const archive = session.status === "exited"
    ? `<button type="button" data-action="archive" class="row-action" title="Archive stopped session">▣</button>`
    : "";
  const stop = session.status === "exited"
    ? ""
    : `<button type="button" data-action="kill" class="row-action row-action-danger" title="Stop session">×</button>`;
  const reply = isAttention(session) ? `<div class="row-reply"><input data-reply type="text" maxlength="500" placeholder="Reply without opening terminal" aria-label="Reply to ${escapeHtml(session.name)}" /><button type="button" data-action="reply" class="row-reply-send" title="Send reply">↵</button></div>` : "";
  return `<article class="fleet-row${active}${unread}" data-id="${escapeHtml(session.id)}" role="listitem" tabindex="0" aria-label="${escapeHtml(`${session.name}, ${meta.label}`)}">
    <div class="row-identity"><span class="status-glyph tone-${meta.tone}" title="${meta.label}">${meta.glyph}</span><div class="row-name-wrap"><div class="row-name">${escapeHtml(session.name)}${session.unread ? '<span class="unread-dot" title="Unread attention"></span>' : ""}</div><div class="row-folder">${escapeHtml(folderLabel(session.cwd))}<span class="row-status-label">${meta.label}</span></div></div></div>
    <div class="row-model"><span class="agent-badge agent-${session.agent}">${agentLabel}</span><span class="model-name">${escapeHtml(model)}</span><span class="effort-chip">${escapeHtml(effort)}</span></div>
    <div class="row-dwell"><span>${dwell(session.status_since)}</span><small>${escapeHtml(session.last_line || "No output yet")}</small></div>
    <div class="row-actions"><button type="button" data-action="pin" class="row-action ${session.pinned ? "row-action-active" : ""}" title="${pinLabel}">${session.pinned ? "◆" : "◇"}</button><button type="button" data-action="focus" class="row-action" title="Focus terminal">↗</button>${revive}${archive}${stop}</div>
    ${reply}
  </article>`;
}

function updateSession(session) {
  const index = state.sessions.findIndex((item) => item.id === session.id);
  if (index === -1) state.sessions.push(session);
  else state.sessions[index] = session;
  renderRows();
  updateTerminalHeader();
}

function removeSession(id) {
  state.sessions = state.sessions.filter((session) => session.id !== id);
  for (const [key, entry] of state.attentionToasts) {
    if (entry.sessionId === id) retractAttentionToast(key);
  }
  if (state.focused === id) {
    state.focused = null;
    resetTerminal("Session exited");
  }
  renderRows();
}

function updateTerminalHeader() {
  const session = state.sessions.find((item) => item.id === state.focused);
  if (!session) {
    $("terminal-name").textContent = "No focused session";
    $("terminal-path").textContent = "";
    $("terminal-status").textContent = "Waiting for a session";
    $("terminal-pulse").className = "terminal-pulse";
    return;
  }
  const meta = STATUS_META[session.status] ?? STATUS_META.exited;
  $("terminal-name").textContent = session.name;
  $("terminal-path").textContent = session.cwd;
  $("terminal-status").textContent = `${meta.label} · ${dwell(session.status_since)} · ${session.agent === "codex" ? "Codex" : "Claude Code"}`;
  $("terminal-pulse").className = `terminal-pulse pulse-${meta.tone}`;
}

function resetTerminal(status = "Waiting for a session") {
  if (state.terminal) state.terminal.reset();
  $("terminal-status").textContent = status;
  updateTerminalHeader();
}

async function loadSnapshot() {
  try {
    const snapshot = await invoke("fleet_snapshot");
    state.sessions = snapshot.sessions ?? [];
    state.focused = snapshot.focused ?? null;
    renderRows();
    updateTerminalHeader();
    if (state.focused) {
      const replay = await reattachForFocus(state.focused);
      await hydrateTerminal(state.focused, replay);
    }
  } catch (error) {
    showToast(`Could not read daemon snapshot: ${error}`);
  }
}

async function focusSession(id) {
  try {
    const replay = await reattachForFocus(id);
    state.focused = id;
    renderRows();
    await hydrateTerminal(id, replay);
  } catch (error) {
    showToast(`Could not focus session: ${error}`);
  }
}

async function reattachForFocus(id) {
  const session = state.sessions.find((item) => item.id === id);
  if (session && session.status !== "exited") return invoke("reattach_session", { id });
  await invoke("focus_session", { id });
  return null;
}

async function hydrateTerminal(id, replayData = null) {
  if (!state.terminal) return;
  state.terminal.reset();
  try {
    const data = replayData ?? (await invoke("scrollback", { id }));
    if (data) state.terminal.write(data);
    await invoke("mark_read", { id });
  } catch (error) {
    showToast(`Could not attach terminal: ${error}`);
  }
  updateTerminalHeader();
  renderRows();
}

async function rowAction(action, id, row = null) {
  try {
    if (action === "pin") await invoke("toggle_pin", { id });
    if (action === "focus") await focusSession(id);
    if (action === "reply") {
      const input = row?.querySelector("input[data-reply]");
      const reply = input?.value.trim();
      if (!reply) return;
      const bracketedPaste = `\x1b[200~${reply}\x1b[201~\r`;
      await invoke("write_session", { id, data: bracketedPaste });
      await invoke("mark_read", { id });
      input.value = "";
      showToast("Reply sent", "success");
    }
    if (action === "kill") {
      await invoke("kill_session", { id });
      showToast("Stop signal sent", "success");
    }
    if (action === "revive") {
      await invoke("revive_session", { id });
      showToast("Native session resume started", "success");
    }
    if (action === "archive") {
      await invoke("archive_session", { id });
      showToast("Stopped session archived", "success");
    }
  } catch (error) {
    showToast(String(error));
  }
}

function defaultSpec() {
  return {
    agent: "claude",
    name: null,
    cwd: "",
    model: null,
    effort: "high",
    permission: "ask",
    sandbox: null,
    profile: null,
    add_dirs: [],
    resume: { kind: "new" },
    max_budget_usd: null,
    web_search: false,
    initial_prompt: null,
    extra_args: [],
  };
}

function readSpec() {
  const agent = $("agent-input").value;
  const resumeKind = $("resume-input").value;
  const nativeId = $("resume-id-input").value.trim();
  const resume = resumeKind === "session" ? { kind: "session", id: nativeId } : resumeKind === "fork" ? { kind: "fork", id: nativeId } : { kind: resumeKind };
  const budget = $("budget-input").value.trim();
  return {
    agent,
    name: $("name-input").value.trim() || null,
    cwd: $("cwd-input").value.trim(),
    model: $("model-input").value.trim() || null,
    effort: $("effort-input").value,
    permission: $("permission-input").value,
    sandbox: agent === "codex" ? $("sandbox-input").value : null,
    profile: agent === "codex" ? $("profile-input").value.trim() || null : null,
    add_dirs: [...state.extraDirs],
    resume,
    max_budget_usd: agent === "claude" && budget ? Number(budget) : null,
    web_search: agent === "codex" && $("search-input").checked,
    initial_prompt: $("prompt-input").value.trim() || null,
    extra_args: [],
  };
}

function writeSpec(spec) {
  $("agent-input").value = spec.agent ?? "claude";
  $("name-input").value = spec.name ?? "";
  $("cwd-input").value = spec.cwd ?? "";
  $("model-input").value = spec.model ?? "";
  $("effort-input").value = spec.effort ?? "high";
  $("permission-input").value = spec.permission ?? "ask";
  $("sandbox-input").value = spec.sandbox ?? "workspace-write";
  $("profile-input").value = spec.profile ?? "";
  $("resume-input").value = spec.resume?.kind ?? "new";
  $("resume-id-input").value = spec.resume?.id ?? "";
  $("budget-input").value = spec.max_budget_usd ?? "";
  $("search-input").checked = Boolean(spec.web_search);
  $("prompt-input").value = spec.initial_prompt ?? "";
  state.extraDirs = spec.add_dirs ?? [];
  $("extra-dirs-input").value = state.extraDirs.join("; ");
  syncAgentFields();
  schedulePreview();
}

function syncAgentFields() {
  const codex = $("agent-input").value === "codex";
  document.querySelectorAll(".codex-only").forEach((element) => element.classList.toggle("field-hidden", !codex));
  document.querySelectorAll(".claude-only").forEach((element) => element.classList.toggle("field-hidden", codex));
  const suggestions = $("model-suggestions");
  suggestions.innerHTML = (MODEL_SUGGESTIONS[codex ? "codex" : "claude"] ?? []).map((model) => `<option value="${model}"></option>`).join("");
  if (!codex && $("permission-input").value === "plan") $("permission-input").value = "ask";
  document.querySelectorAll(".resume-id-field").forEach((element) => element.classList.toggle("field-hidden", $("resume-input").value === "new" || $("resume-input").value === "last"));
}

function schedulePreview() {
  clearTimeout(state.previewTimer);
  state.previewTimer = setTimeout(updatePreview, 180);
}

async function updatePreview() {
  const spec = readSpec();
  if (!spec.cwd) {
    $("preview-output").textContent = "Choose a project folder to preview the exact command vector.";
    $("preview-state").textContent = "Waiting for a valid folder";
    return;
  }
  $("preview-state").textContent = "Resolving native binary…";
  try {
    const command = await invoke("preview_launch", invokeArgs(spec));
    $("preview-output").textContent = command;
    $("preview-state").textContent = "Exact argv preview";
  } catch (error) {
    $("preview-output").textContent = String(error);
    $("preview-state").textContent = "Launch refused";
  }
}

async function launchCurrentSpec() {
  const spec = readSpec();
  if (!spec.cwd) {
    showToast("Choose a project folder first");
    return false;
  }
  try {
    await invoke("launch_session", invokeArgs(spec));
    $("launcher-dialog").close();
    showToast(`${spec.agent === "codex" ? "Codex" : "Claude Code"} session launched`, "success");
    return true;
  } catch (error) {
    showToast(String(error));
    return false;
  }
}

async function loadPresets() {
  try {
    state.presets = await invoke("list_presets");
    $("preset-select").innerHTML = `<option value="">Presets</option>${state.presets.map((preset) => `<option value="${escapeHtml(preset.name)}">${escapeHtml(preset.name)}</option>`).join("")}`;
  } catch (error) {
    showToast(`Could not load presets: ${error}`);
  }
}

async function saveCurrentPreset() {
  const name = $("preset-name-input").value.trim();
  if (!name) {
    showToast("Enter a preset name first");
    $("preset-name-input").focus();
    return;
  }
  try {
    await invoke("save_preset", { preset: { name, spec: readSpec(), configured_path: null } });
    await loadPresets();
    $("preset-name-input").value = "";
    showToast(`Preset “${name}” saved`, "success");
  } catch (error) {
    showToast(String(error));
  }
}

function loadSelectedPreset() {
  const preset = state.presets.find((entry) => entry.name === $("preset-select").value);
  if (!preset) return;
  writeSpec(preset.spec);
  $("launcher-dialog").showModal();
}

function openLauncher() {
  writeSpec(defaultSpec());
  $("launcher-dialog").showModal();
  $("cwd-input").focus();
}

function setupTerminal() {
  state.terminal = new Terminal({
    allowProposedApi: false,
    convertEol: false,
    cursorBlink: true,
    cursorStyle: "bar",
    fontFamily: "'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
    fontSize: 13,
    lineHeight: 1.25,
    scrollback: 2000,
    theme: {
      background: "#11111b",
      foreground: "#cdd6f4",
      cursor: "#f5e0e6",
      selectionBackground: "#585b70",
      black: "#181825",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#cba6f7",
      cyan: "#94e2d5",
      white: "#bac2de",
    },
  });
  state.fitAddon = new FitAddon();
  state.terminal.loadAddon(state.fitAddon);
  state.terminal.open($("terminal-host"));
  state.terminal.resize(120, 40);
  state.terminal.onData(async (data) => {
    if (!state.focused) return;
    try {
      await invoke("write_session", { id: state.focused, data });
    } catch (error) {
      showToast(String(error));
    }
  });
}

async function handleDaemonEvent(event) {
  switch (event.kind) {
    case "session-updated":
      updateSession(event.session);
      break;
    case "session-removed":
      removeSession(event.id);
      break;
    case "notification":
      if (event.event?.kind === "raised") showAttentionToast(event.event.notification);
      if (event.event?.kind === "retracted") retractAttentionToast(event.event.dedup_key);
      break;
    case "output":
      if (event.id === state.focused && state.terminal) state.terminal.write(event.data);
      break;
    default:
      break;
  }
}

function bindEvents() {
  $("new-session-button").addEventListener("click", openLauncher);
  $("empty-new-button").addEventListener("click", openLauncher);
  $("refresh-button").addEventListener("click", loadSnapshot);
  $("filter-input").addEventListener("input", renderRows);
  $("attention-filter").addEventListener("click", () => {
    state.attentionOnly = !state.attentionOnly;
    $("attention-filter").setAttribute("aria-pressed", String(state.attentionOnly));
    $("attention-filter").classList.toggle("attention-filter-active", state.attentionOnly);
    renderRows();
  });
  $("agent-input").addEventListener("change", () => {
    syncAgentFields();
    schedulePreview();
  });
  ["cwd-input", "model-input", "name-input", "effort-input", "permission-input", "sandbox-input", "profile-input", "resume-input", "resume-id-input", "budget-input", "prompt-input", "search-input"].forEach((id) => {
    $(id).addEventListener("input", () => {
      if (id === "resume-input") syncAgentFields();
      schedulePreview();
    });
    $(id).addEventListener("change", () => {
      if (id === "resume-input") syncAgentFields();
      schedulePreview();
    });
  });
  $("pick-folder-button").addEventListener("click", async () => {
    const folder = await invoke("pick_folder");
    if (folder) {
      $("cwd-input").value = folder;
      schedulePreview();
    }
  });
  $("pick-extra-button").addEventListener("click", async () => {
    const folders = await invoke("pick_extra_dirs");
    if (folders?.length) {
      state.extraDirs = folders;
      $("extra-dirs-input").value = folders.join("; ");
      schedulePreview();
    }
  });
  $("save-preset-button").addEventListener("click", saveCurrentPreset);
  $("launch-preset-button").addEventListener("click", loadSelectedPreset);
  $("cancel-launch-button").addEventListener("click", () => $("launcher-dialog").close());
  $("launcher-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    await launchCurrentSpec();
  });
  $("terminal-clear").addEventListener("click", () => state.terminal?.clear());
  $("terminal-resize").addEventListener("click", async () => {
    if (!state.focused) return;
    try {
      await invoke("resize_session", { id: state.focused, rows: 40, cols: 120 });
      state.terminal?.resize(120, 40);
      showToast("Terminal reset to canonical 120 × 40 grid", "success");
    } catch (error) {
      showToast(String(error));
    }
  });
}

async function start() {
  setupTerminal();
  bindEvents();
  syncAgentFields();
  try {
    await listen("terminalai:event", ({ payload }) => handleDaemonEvent(payload));
  } catch (error) {
    showToast(`Event stream unavailable: ${error}`);
  }
  await Promise.all([loadSnapshot(), loadPresets()]);
  setInterval(() => {
    renderRows();
    updateTerminalHeader();
  }, 1000);
}

start();
