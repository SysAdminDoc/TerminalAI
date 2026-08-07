/**
 * The two overflow menus in the chrome.
 *
 * The toolbar used to carry every dialog launcher inline, which overflowed the
 * panel and painted on top of the terminal pane at the documented default
 * window size. The controls did not go away — they moved behind a trigger, so
 * the row that is always on screen holds only what is used while scanning the
 * fleet.
 *
 * Closing on outside click and on Escape is what makes a menu feel like a menu;
 * a menu that only closes by re-clicking its trigger reads as broken.
 *
 * These are **disclosure panels**, not ARIA menus, and the markup says so. They
 * used to claim `role="menu"` with `role="menuitem"` children, which is a
 * promise neither the markup nor this file kept: the app panel holds a `<select>`
 * and a heading, which are invalid children of a menu and make a screen reader
 * announce the wrong item count, and nothing here implements the arrow-key
 * movement the menu pattern requires once the role is claimed. Tab, Escape and
 * outside-click — what is actually implemented — are exactly right for a
 * disclosure, so the roles went rather than the behaviour.
 */
export function wireOverflowMenus(doc = document) {
  const $ = (id) => doc.getElementById(id);
  const menus = [
    { button: "app-menu-button", panel: "app-menu" },
    { button: "tools-menu-button", panel: "tools-menu" },
  ].map(({ button, panel }) => ({ button: $(button), panel: $(panel) }));

  const close = (menu) => {
    menu.panel.hidden = true;
    menu.button.setAttribute("aria-expanded", "false");
  };
  const closeAll = () => menus.forEach(close);

  for (const menu of menus) {
    menu.button.addEventListener("click", (event) => {
      event.stopPropagation();
      const wasOpen = !menu.panel.hidden;
      closeAll();
      if (wasOpen) return;
      menu.panel.hidden = false;
      menu.button.setAttribute("aria-expanded", "true");
      // Focus the first item so the menu is usable without a pointer.
      menu.panel.querySelector("button, select")?.focus();
    });
    // An item that opens a dialog must not leave the menu hanging open behind
    // it; the click still reaches the item's own handler first.
    menu.panel.addEventListener("click", (event) => {
      if (event.target.closest(".menu-item, .menu-inline .button")) closeAll();
    });
  }

  doc.addEventListener("click", (event) => {
    if (!event.target.closest(".menu-wrap")) closeAll();
  });
  doc.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    const open = menus.find((menu) => !menu.panel.hidden);
    if (!open) return;
    closeAll();
    open.button.focus();
  });
}
