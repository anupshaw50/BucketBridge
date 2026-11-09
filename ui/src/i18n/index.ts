import type { MessageKey } from "../features/menus/ids";
import { en, type Messages } from "./en";
import { getLocale, setLocale, subscribeLocale, type Locale } from "./locale";
import { conflicts, zh } from "./zh";
import { zhMenu } from "./zh/menu";

let current: Messages = menuBundle(getLocale());

function menuBundle(locale: Locale): Messages {
  return locale === "zh-CN" ? zhMenu : en;
}

// Keep the menu bundle in step with the language; `App` subscribes and
// re-renders the tree, so the labels of a menu change with it.
subscribeLocale(() => {
  current = menuBundle(getLocale());
});

/** The menu bundle in use, i.e. the language the interface shows now. */
export function currentMessages(): Messages {
  return current;
}

/** Text of a message key. */
export function t(key: MessageKey): string {
  return current[key];
}

/** Menu labels by their English text, so `tr` answers for them as well. */
const menuText: Record<string, string> = {};
for (const key of Object.keys(en) as MessageKey[]) {
  menuText[en[key]] = zhMenu[key];
}

/**
 * English source text in the language of the interface: its Chinese text when
 * the interface is Chinese, the source itself when the interface is English or
 * when a sentence has no translation yet. `vars` fills the `{name}`
 * placeholders of a sentence, so `tr("Deleted {n} files", { n: 3 })` reads
 * "已删除 3 个文件".
 */
export function tr(source: string, vars?: Record<string, string | number>): string {
  const text = getLocale() === "zh-CN" ? (zh[source] ?? menuText[source] ?? source) : source;
  if (vars === undefined) return text;
  return text.replace(/\{(\w+)\}/g, (matched: string, name: string) =>
    Object.hasOwn(vars, name) ? String(vars[name]) : matched,
  );
}

/** Whether `source` has a registered Chinese text (menu labels count). */
export function hasTranslation(source: string): boolean {
  return Object.hasOwn(zh, source) || Object.hasOwn(menuText, source);
}

/** Sentences that two areas translated differently; a test fails on these. */
export { conflicts };

export type { Locale, MessageKey, Messages };
export { en, getLocale, setLocale, subscribeLocale, zh, zhMenu };
