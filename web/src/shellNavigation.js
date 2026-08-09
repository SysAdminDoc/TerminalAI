const RAIL_DIALOG_PAGES = Object.freeze({
  "projects-dialog": "projects",
  "prompt-dialog": "prompts",
  "broadcast-dialog": "broadcast",
  "approvals-dialog": "approvals",
  "search-dialog": "search",
  "working-sets-dialog": "working-sets",
  "history-dialog": "history",
  "settings-dialog": "settings",
  "explainer-dialog": "explainer",
});

/** Persistent shell navigation shared by the rail and compatibility menus. */
export function createShellNavigation({ $, document, state, setPreflightMode }) {
  function syncRailPage(page) {
    for (const item of document.querySelectorAll(".rail-item[data-rail-page]")) {
      const active = item.dataset.railPage === page;
      item.classList.toggle("rail-item-active", active);
      if (active) item.setAttribute("aria-current", "page");
      else item.removeAttribute("aria-current");
    }
  }

  function closeWorkspacePages() {
    for (const dialog of document.querySelectorAll("dialog.workspace-page[open]")) dialog.close();
  }

  /// Menu destinations are pages inside the persistent shell. They still use
  /// a dialog element for the existing focus and accessibility contracts, but
  /// open non-modally so the rail and top-level controls remain available.
  function openWorkspacePage(dialog) {
    if (!dialog) return;
    for (const other of document.querySelectorAll("dialog.workspace-page[open]")) {
      if (other !== dialog) other.close();
    }
    if (state.preflightMode) setPreflightMode(false);
    if (!dialog.open) dialog.show();
    syncRailPage(dialog.dataset.workspacePage ?? RAIL_DIALOG_PAGES[dialog.id] ?? "fleet");
  }

  /// Keep the visual navigation spine in step with the workspace pages. The
  /// overflow menu remains a compatibility path for keyboard users and tests;
  /// the rail is the persistent route through the same handlers.
  function wireRailNavigation() {
    const items = [...document.querySelectorAll(".rail-item[data-rail-page]")];
    if (!items.length) return;
    for (const item of items) {
      item.addEventListener("click", () => {
        const page = item.dataset.railPage;
        syncRailPage(page);
        const target = item.dataset.railTarget;
        if (target) $(target)?.click();
        else {
          closeWorkspacePages();
          if (state.preflightMode) setPreflightMode(false);
        }
      });
    }
    const syncFromDialogs = () => {
      if (state.preflightMode) {
        syncRailPage("preflight");
        return;
      }
      const open = [...document.querySelectorAll("dialog.workspace-page[open]")].at(-1);
      syncRailPage(open ? RAIL_DIALOG_PAGES[open.id] ?? "fleet" : "fleet");
    };
    if (typeof MutationObserver === "function") {
      const observer = new MutationObserver(syncFromDialogs);
      for (const dialog of document.querySelectorAll("dialog")) {
        observer.observe(dialog, { attributes: true, attributeFilter: ["open"] });
      }
    }
    syncFromDialogs();
  }

  return {
    closeWorkspacePages,
    openWorkspacePage,
    syncRailPage,
    wireRailNavigation,
  };
}
