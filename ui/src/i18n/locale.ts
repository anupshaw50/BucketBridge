/** Languages the interface can be shown in. */
export type Locale = "zh-CN" | "en";

const STORAGE_KEY = "bucketbridge.language";

const listeners = new Set<() => void>();

let current: Locale = read();

/**
 * The language a browser language tag asks for: Chinese for `zh…`, English for
 * everything else, which is also the default when no tag is available.
 */
export function localeFromLanguage(language: string): Locale {
  return language.trim().toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

/** A language stored by the switch wins; otherwise the system language decides. */
export function resolveLocale(stored: string | null, language: string): Locale {
  if (stored === "zh-CN" || stored === "en") return stored;
  return localeFromLanguage(language);
}

function read(): Locale {
  let stored: string | null = null;
  try {
    stored = globalThis.localStorage.getItem(STORAGE_KEY);
  } catch {
    // Storage can be unavailable (private mode); the system language stands.
  }
  const language = typeof navigator === "undefined" ? "" : navigator.language;
  return resolveLocale(stored, language);
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
