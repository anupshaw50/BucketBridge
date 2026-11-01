import { useCallback, useEffect, useRef, useState } from "react";
import { isMacPlatform } from "../../lib/keys";
import type { ResolvedItem } from "./resolve";

export interface MenuState {
  /** Changes with every opening, so a menu that is replaced starts afresh. */
  id: number;
  x: number;
  y: number;
  items: ResolvedItem[];
  /** Accessible name of the menu, e.g. "File menu". */
  label: string;
  /** Element that had focus when the menu opened; focus goes back to it when the menu closes. */
  returnFocus: HTMLElement | null;
}

export interface MenuTriggerEvent {
  clientX: number;
  clientY: number;
  preventDefault(): void;
  stopPropagation(): void;
}

/** A pointer that moves further than this between press and release is a drag, not a click. */
const DRAG_TOLERANCE_PX = 4;

let lastPress = { x: 0, y: 0 };

export interface ClickLike {
  button: number;
  ctrlKey: boolean;
  clientX: number;
  clientY: number;
}

/**
 * Ctrl+click is a secondary click on macOS (trackpads), so it opens the menu
 * like a right-click. Ctrl+click and drag does not: the pointer must still be
 * where it was pressed. On other systems Ctrl+click keeps its meaning of
 * "add to the selection".
 */
export function isContextClick(event: ClickLike): boolean {
  if (!isMacPlatform() || !event.ctrlKey || event.button !== 0) {
    return false;
  }
  return Math.hypot(event.clientX - lastPress.x, event.clientY - lastPress.y) <= DRAG_TOLERANCE_PX;
}

/**
 * State of one menu surface. The caller resolves the menu when the user opens
 * it and passes the entries in, so opening a menu is a pure read of data that
 * is already loaded.
 */
export function useContextMenu() {
  const [state, setState] = useState<MenuState | null>(null);
  const opened = useRef(0);

  useEffect(() => {
    const onPress = (event: PointerEvent) => {
      lastPress = { x: event.clientX, y: event.clientY };
    };
    window.addEventListener("pointerdown", onPress, true);
    return () => window.removeEventListener("pointerdown", onPress, true);
  }, []);

  /** Opens the menu at the pointer. */
  const open = useCallback((event: MenuTriggerEvent, items: ResolvedItem[], label: string) => {
    event.preventDefault();
    event.stopPropagation();
    const active = document.activeElement;
    opened.current += 1;
    setState({
      id: opened.current,
      x: event.clientX,
      y: event.clientY,
      items,
      label,
      returnFocus: active instanceof HTMLElement ? active : null,
    });
  }, []);

  /** Opens the menu at a point, for the keyboard (the Menu key and Shift+F10). */
  const openAt = useCallback((x: number, y: number, items: ResolvedItem[], label: string) => {
    const active = document.activeElement;
    opened.current += 1;
    setState({ id: opened.current, x, y, items, label, returnFocus: active instanceof HTMLElement ? active : null });
  }, []);

  const close = useCallback(() => setState(null), []);
  return { state, open, openAt, close };
}

/** Whether a key event asks for the context menu (the Menu key, or Shift+F10). */
export function isMenuKey(event: { key: string; shiftKey: boolean }): boolean {
  return event.key === "ContextMenu" || (event.shiftKey && event.key === "F10");
}
