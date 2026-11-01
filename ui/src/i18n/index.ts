import type { MessageKey } from "../features/menus/ids";
import { en, type Messages } from "./en";

let current: Messages = en;

/** Switches the bundle used by `t` (a translated `Messages` object). */
export function setMessages(messages: Messages): void {
  current = messages;
}

/** Text of a message key. */
export function t(key: MessageKey): string {
  return current[key];
}

export type { MessageKey, Messages };
