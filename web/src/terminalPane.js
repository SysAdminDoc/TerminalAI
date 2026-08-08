/**
 * The focused session's terminal pane.
 *
 * Everything about the one xterm instance the app owns: constructing it, the
 * palette it paints with, following the OS theme, the WebGL renderer and its
 * fallbacks, refitting on resize, and opening a hyperlink a session emitted.
 *
 * Split out of `main.js` for the same reason `rowMarkup.js` was: this is the
 * surface that fills most of the window, and it was fused into a scope where
 * every edit risked every feature. A factory rather than plain exports, because
 * it reads the live `state` object and eleven helpers `main.js` owns; `state` is
 * held by reference, so `state.terminal` and `state.webglAddon` are the same
 * values the rest of the app sees.
 */
export function createTerminalPane(deps) {
  const {
    $,
    state,
    invoke,
    showToast,
    t,
    scheduleFit,
    renderFindCount,
    Terminal,
    FitAddon,
    SearchAddon,
    Unicode11Addon,
    WebglAddon,
    DEFAULT_COLS,
    DEFAULT_ROWS,
  } = deps;

  function observeTerminalSize() {
    const host = $("terminal-host");
    if (!host) return;
    if (typeof ResizeObserver === "function") {
      new ResizeObserver(scheduleFit).observe(host);
    }
    window.addEventListener("resize", scheduleFit);
  }

  /// Open an OSC 8 hyperlink a session emitted.
  ///
  /// The scheme allowlist lives in Rust, not here: this is agent-controlled text,
  /// and the renderer is the wrong place to be the only thing standing between it
  /// and `ShellExecute`. A refusal is shown, never swallowed.
  async function openSessionLink(uri) {
    try {
      const opened = await invoke("open_external_url", { url: uri });
    showToast(t("link-opened", { host: new URL(opened).host || opened }), "success");
    } catch (error) {
      showToast(String(error));
    }
  }

  /// Swap the DOM renderer for the WebGL one.
  ///
  /// Kept separate from `setupTerminal` because every step here can legitimately
  /// fail on a machine with no usable GPU path, and the terminal must still work
  /// when it does. WebView2 falls back to SwiftShader in some configurations, and
  /// the context can also be lost later — after a driver reset or a GPU process
  /// crash — so `onContextLoss` disposes the addon and returns the terminal to the
  /// DOM renderer rather than leaving a blank pane.
  function useWebglRenderer(terminal) {
    let addon;
    try {
      addon = new WebglAddon();
    } catch (error) {
      console.info("WebGL renderer unavailable, using the DOM renderer", error);
      return null;
    }
    addon.onContextLoss(() => {
      console.info("WebGL context lost, falling back to the DOM renderer");
      addon.dispose();
      state.webglAddon = null;
    });
    try {
      terminal.loadAddon(addon);
    } catch (error) {
      // loadAddon is where context creation actually happens.
      console.info("WebGL context could not be created, using the DOM renderer", error);
      addon.dispose();
      return null;
    }
    return addon;
  }

  /// The terminal's palette, read from the same custom properties every other
  /// surface uses.
  ///
  /// It used to be a literal here, which meant the one surface that fills most of
  /// the window ignored `prefers-color-scheme` entirely: in light mode the
  /// operator got a light panel framing a hard dark rectangle, and focusing a
  /// session flipped the pane's apparent theme. The canvas is not DOM text, so no
  /// contrast gate could see it.
  function terminalTheme() {
    const styles = getComputedStyle(document.documentElement);
    const token = (name) => styles.getPropertyValue(name).trim();
    return {
      background: token("--term-bg"),
      foreground: token("--term-fg"),
      cursor: token("--term-cursor"),
      selectionBackground: token("--term-selection"),
      black: token("--term-black"),
      red: token("--red"),
      green: token("--green"),
      yellow: token("--yellow"),
      blue: token("--blue"),
      magenta: token("--mauve"),
      cyan: token("--teal"),
      white: token("--term-white"),
    };
  }

  /// Repaint the terminal when the OS theme changes under a running window.
  ///
  /// The rest of the chrome is CSS and follows on its own; the canvas is painted
  /// from values read once, so without this the pane keeps the palette it started
  /// with and becomes the only surface out of step.
  function followColorScheme() {
    const scheme = window.matchMedia?.("(prefers-color-scheme: dark)");
    scheme?.addEventListener?.("change", () => {
      if (!state.terminal) return;
      state.terminal.options.theme = terminalTheme();
    });
  }

  function setupTerminal() {
    state.terminal = new Terminal({
      // Required by the unicode11 addon. Without it xterm measures character
      // widths against Unicode 6, while the Rust grid uses `unicode-width`
      // against a modern table — the two then disagree about where a line wraps,
      // and the row status inferred from the Rust grid stops matching the pane.
      allowProposedApi: true,
      // OSC 8 hyperlinks reach the pane already, because the focused renderer
      // replays raw PTY bytes. Without a handler xterm underlines them and
      // clicking does nothing.
      linkHandler: {
        activate: (event, uri) => {
          event.preventDefault();
          openSessionLink(uri);
        },
      },
      convertEol: false,
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily: "'Cascadia Code', 'SFMono-Regular', Consolas, monospace",
      fontSize: 13,
      lineHeight: 1.25,
      scrollback: 2000,
      screenReaderMode: false,
      theme: terminalTheme(),
    });
    followColorScheme();
    state.fitAddon = new FitAddon();
    state.terminal.loadAddon(state.fitAddon);
    // Find-in-pane. The addon reports its own match count through
    // `onDidChangeResults`, which is the number worth showing: a bare "found /
    // not found" makes the operator page through the pane to learn how much
    // there is.
    state.searchAddon = new SearchAddon();
    state.terminal.loadAddon(state.searchAddon);
    state.searchAddon.onDidChangeResults((results) => renderFindCount(results));
    const unicode11 = new Unicode11Addon();
    state.terminal.loadAddon(unicode11);
    state.terminal.unicode.activeVersion = "11";
    state.terminal.open($("terminal-host"));
    // Must follow `open`: the WebGL addon needs an attached element to create a
    // context against.
    state.webglAddon = useWebglRenderer(state.terminal);
    state.terminal.resize(DEFAULT_COLS, DEFAULT_ROWS);
    // The addon was constructed and registered but never called, so the grid
    // stayed at its hard-coded size no matter how large the pane was.
    observeTerminalSize();
    state.terminal.onData(async (data) => {
      if (!state.focused || state.demoMode) return;
      try {
        await invoke("write_session", { id: state.focused, data });
      } catch (error) {
        showToast(String(error));
      }
    });
  }

  return {
    followColorScheme,
    observeTerminalSize,
    openSessionLink,
    setupTerminal,
    terminalTheme,
    useWebglRenderer,
  };
}
