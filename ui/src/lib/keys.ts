// Keyboard shortcut helpers: platform detection, matching a key event against a
// binding, and the text shown in menus and tooltips.

/** True on macOS (and iPadOS), where Cmd is the command key and Ctrl+click is a secondary click. */
export function isMacPlatform(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);
}

export interface Binding {
  /** `KeyboardEvent.key`; letters are compared case-insensitively. */
  key: string;
  /** Ctrl or Cmd (either one counts, on every platform). */
  mod?: boolean;
  /** Cmd only. */
  meta?: boolean;
  shift?: boolean;
}

export interface KeyEventLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/** Whether a key event is exactly this binding (Alt is never part of a binding). */
export function matchesBinding(event: KeyEventLike, binding: Binding): boolean {
  if (event.altKey || event.key.toLowerCase() !== binding.key.toLowerCase()) {
    return false;
  }
  if (binding.meta === true) {
    return event.metaKey && !event.ctrlKey;
  }
  const modifier = event.ctrlKey || event.metaKey;
  if ((binding.mod === true) !== modifier) {
    return false;
  }
  return binding.shift === undefined || binding.shift === event.shiftKey;
}

const KEY_NAMES: Readonly<Record<string, string>> = { Delete: "Del", Backspace: "Backspace" };

/** Text for menus: "Ctrl+C" (or "Cmd+C" on macOS), "F5", "Del". */
export function formatBinding(binding: Binding): string {
  const parts: string[] = [];
  if (binding.mod === true) {
    parts.push(isMacPlatform() ? "Cmd" : "Ctrl");
  }
  if (binding.meta === true) {
    parts.push("Cmd");
  }
  if (binding.shift === true) {
    parts.push("Shift");
  }
  parts.push(KEY_NAMES[binding.key] ?? (binding.key.length === 1 ? binding.key.toUpperCase() : binding.key));
  return parts.join("+");
}

/** Value for `aria-keyshortcuts`: "Control+C" (or "Meta+C" on macOS), "Delete". */
export function ariaBinding(binding: Binding): string {
  const parts: string[] = [];
  if (binding.mod === true) {
    parts.push(isMacPlatform() ? "Meta" : "Control");
  }
  if (binding.meta === true) {
    parts.push("Meta");
  }
  if (binding.shift === true) {
    parts.push("Shift");
  }
  parts.push(binding.key.length === 1 ? binding.key.toUpperCase() : binding.key);
  return parts.join("+");
}
