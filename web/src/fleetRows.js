/**
 * Reconcile keyed row elements without replacing the container or the row
 * object that owns an active editor.
 *
 * `shouldPreserve` is used for rows that disappear from a filtered view. A
 * draft or focused row is hidden instead of removed so its input state remains
 * available when the filter changes again.
 */
export function reconcileKeyedRows(container, items, keyOf, createRow, updateRow, shouldPreserve) {
  const existing = new Map();
  for (const row of Array.from(container.children)) existing.set(keyOfRow(row), row);

  const wanted = new Set();
  let cursor = container.firstElementChild;
  for (const item of items) {
    const key = keyOf(item);
    wanted.add(key);
    let row = existing.get(key);
    if (!row) {
      row = createRow(item);
      existing.set(key, row);
    }
    updateRow(row, item);
    row.hidden = false;
    if (row !== cursor) {
      container.insertBefore(row, cursor);
    } else {
      cursor = cursor?.nextElementSibling ?? null;
    }
  }

  for (const [key, row] of existing) {
    if (wanted.has(key)) continue;
    if (shouldPreserve?.(row)) {
      row.hidden = true;
    } else {
      row.remove();
    }
  }
}

/**
 * Keep the optional grouping label in a keyed row aligned with the current
 * grouping mode. The row is intentionally reused, so its chip needs the same
 * reconciliation treatment as the row's status and reply controls.
 */
export function reconcileGroupChip(row, group, title) {
  const folder = row.querySelector(".row-folder");
  if (!folder) return;
  const portBadge = folder.querySelector(".row-ports");
  let chip = folder.querySelector(".row-group");
  if (!group) {
    chip?.remove();
    return;
  }
  if (!chip) {
    chip = row.ownerDocument.createElement("span");
    chip.className = "row-group";
    folder.insertBefore(chip, portBadge);
  }
  chip.title = title;
  chip.textContent = group;
}

function keyOfRow(row) {
  return row.dataset.id;
}
