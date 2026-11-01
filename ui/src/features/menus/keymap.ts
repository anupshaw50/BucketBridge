import { type Binding, type KeyEventLike, matchesBinding } from "../../lib/keys";
import type { ActionId, Pane } from "./ids";

export interface KeyBinding extends Binding {
  id: ActionId;
  /** The pane that has to be focused for the key to work. */
  pane: Pane;
}

/**
 * The keymap. It is the only place that decides which shortcut an action has:
 * menus show the first binding of an action in their pane, and the key
 * handlers of the panes resolve key events through `actionForKey`, so a
 * shortcut that is shown always works without opening a menu.
 */
export const KEYMAP: readonly KeyBinding[] = [
  // Files pane (the object list has keyboard focus).
  { id: "open", pane: "files", key: "Enter" },
  { id: "item.rename", pane: "files", key: "F2" },
  { id: "file.preview", pane: "files", key: "F3" },
  { id: "item.delete", pane: "files", key: "Delete" },
  { id: "item.delete", pane: "files", key: "Backspace", meta: true },
  { id: "clipboard.copy", pane: "files", key: "c", mod: true },
  { id: "clipboard.cut", pane: "files", key: "x", mod: true },
  { id: "clipboard.paste", pane: "files", key: "v", mod: true },
  { id: "list.select-all", pane: "files", key: "a", mod: true },
  { id: "list.invert", pane: "files", key: "i", mod: true },
  { id: "list.refresh", pane: "files", key: "F5" },
  // Buckets pane (the bucket tree has keyboard focus).
  { id: "bucket.open", pane: "buckets", key: "Enter" },
  { id: "open", pane: "buckets", key: "Enter" },
  { id: "bucket.delete", pane: "buckets", key: "Delete" },
  { id: "bucket.refresh", pane: "buckets", key: "F5" },
];

/** The binding an action shows in a pane, or null when it has none there. */
export function bindingFor(id: ActionId, pane: Pane): KeyBinding | null {
  return KEYMAP.find((binding) => binding.id === id && binding.pane === pane) ?? null;
}

/** The action a key event triggers in a pane, or null. */
export function actionForKey(event: KeyEventLike, pane: Pane): ActionId | null {
  return KEYMAP.find((binding) => binding.pane === pane && matchesBinding(event, binding))?.id ?? null;
}
