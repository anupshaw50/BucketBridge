/** Languages the interface can be shown in. This build ships Chinese first. */
export type Locale = "zh-CN" | "en";

const STORAGE_KEY = "bucketbridge.language";

const listeners = new Set<() => void>();

let current: Locale = read();

function read(): Locale {
  try {
    const stored = globalThis.localStorage.getItem(STORAGE_KEY);
    if (stored === "zh-CN" || stored === "en") return stored;
  } catch {
    // Storage can be unavailable (private mode); the default stands.
  }
  return "zh-CN";
}

/** The language the interface is currently shown in. */
export function getLocale(): Locale {
  return current;
}

/**
 * Switches the language, remembers the choice and notifies every subscriber,
 * which re-renders the tree that subscribed (see `App`).
 */
export function setLocale(next: Locale): void {
  if (next === current) return;
  current = next;
  try {
    globalThis.localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // The language still changes for this session, the choice is just lost.
  }
  if (typeof document !== "undefined") document.documentElement.lang = next;
  for (const listener of [...listeners]) listener();
}

/** Re-renders on a language change; the return value unsubscribes. */
export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

if (typeof document !== "undefined") document.documentElement.lang = current;
