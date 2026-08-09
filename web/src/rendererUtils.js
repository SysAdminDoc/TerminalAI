/** Shared renderer boundary helpers.
 *
 * These are deliberately independent of the feature panels. The shell owns
 * their collaborators once, then injects the returned DOM helpers into pages;
 * tests can exercise the byte, escaping, invocation, and error contracts
 * without slicing the entry module.
 */

export function terminalBytes(payload) {
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  if (ArrayBuffer.isView(payload)) {
    return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  }
  if (Array.isArray(payload)) return Uint8Array.from(payload);
  return new TextEncoder().encode(String(payload ?? ""));
}

export function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

export function invokeArgs(spec) {
  return { spec, configuredPath: null };
}

export function createRendererUtils({ $, document, t, requestAnimationFrame, setTimeout }) {
  const raf = requestAnimationFrame ?? globalThis.requestAnimationFrame?.bind(globalThis);
  const timer = setTimeout ?? globalThis.setTimeout.bind(globalThis);

  function renderDataError(container, message, action, retry) {
    container.innerHTML = [
      `<div class="data-error surface-error" role="alert"><p>${escapeHtml(message)}</p>`,
      `<button type="button" class="button button-secondary" data-retry-action="${escapeHtml(action)}">`,
      `${escapeHtml(t("button-retry"))}</button></div>`,
    ].join("");
    const button = container.querySelector("[data-retry-action]");
    if (button?.dataset.retryAction === action) button.addEventListener("click", () => void retry());
  }

  /// Render into a dialog, and put the failure in that dialog if it throws.
  function renderGuarded(container, message, action, retry, render) {
    try {
      render();
    } catch (error) {
      console.error(`${action} failed to render`, error);
      renderDataError(container, `${message} ${error}`, action, retry);
    }
  }

  function showToast(message, tone = "error") {
    const toast = document.createElement("div");
    toast.className = `toast toast-${tone}`;
    toast.textContent = message;
    $("toast-region").append(toast);
    raf?.(() => toast.classList.add("toast-visible"));
    timer(() => {
      toast.classList.remove("toast-visible");
      timer(() => toast.remove(), 240);
    }, 4200);
  }

  return { renderDataError, renderGuarded, showToast };
}

export function createTerminalOutput({ state }) {
  function writeTerminalBytes(payload, id = state.focused, generation = state.focusGeneration) {
    if (id !== state.focused || generation !== state.focusGeneration) return;
    if (state.terminal) state.terminal.write(terminalBytes(payload));
  }

  return { writeTerminalBytes };
}
