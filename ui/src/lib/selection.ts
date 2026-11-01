// Pure multi-selection logic for lists: click, ctrl/cmd-click, shift-click,
// keyboard focus movement, select all and invert. `order` is always the list
// of keys in display order.

export interface Selection {
  readonly keys: ReadonlySet<string>;
  /** Fixed end of a shift range. */
  readonly anchor: string | null;
  /** Row with keyboard focus (the moving end of a range). */
  readonly focus: string | null;
}

export const EMPTY_SELECTION: Selection = { keys: new Set(), anchor: null, focus: null };

/** Plain click: select only `key`. */
export function selectOnly(key: string): Selection {
  return { keys: new Set([key]), anchor: key, focus: key };
}

/** Ctrl/Cmd click: toggle `key`, keep everything else. */
export function toggle(selection: Selection, key: string): Selection {
  const keys = new Set(selection.keys);
  if (keys.has(key)) {
    keys.delete(key);
  } else {
    keys.add(key);
  }
  return { keys, anchor: key, focus: key };
}

function rangeKeys(order: readonly string[], from: string, to: string): string[] {
  const start = order.indexOf(from);
  const end = order.indexOf(to);
  if (end === -1) {
    return [];
  }
  if (start === -1) {
    return [to];
  }
  const low = Math.min(start, end);
  const high = Math.max(start, end);
  return order.slice(low, high + 1);
}

/**
 * Shift click: select the range between the anchor and `key`. With `additive`
 * (shift plus ctrl/cmd) the range is added to the existing selection.
 */
export function selectRange(
  selection: Selection,
  order: readonly string[],
  key: string,
  additive = false,
): Selection {
  const anchor = selection.anchor !== null && order.includes(selection.anchor) ? selection.anchor : key;
  const keys = new Set(additive ? selection.keys : []);
  for (const item of rangeKeys(order, anchor, key)) {
    keys.add(item);
  }
  return { keys, anchor, focus: key };
}

export function selectAll(order: readonly string[]): Selection {
  const first = order[0] ?? null;
  return { keys: new Set(order), anchor: first, focus: first };
}

export function invert(selection: Selection, order: readonly string[]): Selection {
  const keys = new Set(order.filter((key) => !selection.keys.has(key)));
  const first = order.find((key) => keys.has(key)) ?? null;
  return { keys, anchor: first, focus: first };
}

/** Drops keys that are no longer listed (after a refresh or a filter change). */
export function prune(selection: Selection, order: readonly string[]): Selection {
  const present = new Set(order);
  let changed = false;
  const keys = new Set<string>();
  for (const key of selection.keys) {
    if (present.has(key)) {
      keys.add(key);
    } else {
      changed = true;
    }
  }
  const anchor = selection.anchor !== null && present.has(selection.anchor) ? selection.anchor : null;
  const focus = selection.focus !== null && present.has(selection.focus) ? selection.focus : null;
  if (!changed && anchor === selection.anchor && focus === selection.focus) {
    return selection;
  }
  return { keys, anchor, focus };
}

/**
 * Arrow keys / Home / End: move the focus by `delta` rows (clamped). Without
 * `extend` the focused row becomes the only selection; with `extend` (shift)
 * the range from the anchor to the new focus is selected.
 */
export function moveFocus(
  selection: Selection,
  order: readonly string[],
  delta: number,
  extend: boolean,
): Selection {
  if (order.length === 0) {
    return EMPTY_SELECTION;
  }
  const current = selection.focus === null ? -1 : order.indexOf(selection.focus);
  // Without a focused row, forward steps start before the first row and
  // backward steps after the last one, so Home selects the first row and End the last.
  const start = current !== -1 ? current : delta >= 0 ? -1 : order.length;
  const next = Math.max(0, Math.min(order.length - 1, start + delta));
  const key = order[next];
  if (key === undefined) {
    return selection;
  }
  if (extend) {
    return selectRange(selection.anchor === null ? { ...selection, anchor: key } : selection, order, key);
  }
  return selectOnly(key);
}

/** Selected keys in display order. */
export function orderedSelection(selection: Selection, order: readonly string[]): string[] {
  return order.filter((key) => selection.keys.has(key));
}
