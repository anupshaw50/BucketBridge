import { t } from "../../i18n";
import { ariaBinding, formatBinding } from "../../lib/keys";
import type { MenuContext } from "./context";
import type { ActionId, MenuId, SubmenuId } from "./ids";
import { bindingFor } from "./keymap";
import { LAYOUTS, type LayoutEntry } from "./layouts";
import { actionState, DEFAULT_DEPS, type RegistryDeps, runAction } from "./registry";

export interface ResolvedAction {
  type: "action";
  id: ActionId;
  label: string;
  /** Shortcut text, only when the shortcut works in the menu's pane. */
  shortcut: string | null;
  ariaShortcut: string | null;
  danger: boolean;
  disabled: boolean;
  /** Why the entry is disabled. */
  hint: string | null;
  run: () => void;
}

export interface ResolvedSubmenu {
  type: "submenu";
  id: SubmenuId;
  label: string;
  items: ResolvedItem[];
}

export interface ResolvedSeparator {
  type: "separator";
}

export type ResolvedItem = ResolvedAction | ResolvedSubmenu | ResolvedSeparator;

function resolveEntries(entries: readonly LayoutEntry[], ctx: MenuContext, deps: RegistryDeps): ResolvedItem[] {
  const items: ResolvedItem[] = [];
  for (const entry of entries) {
    if ("separator" in entry) {
      items.push({ type: "separator" });
    } else if ("submenu" in entry) {
      const children = tidy(resolveEntries(entry.items, ctx, deps));
      if (children.length > 0) {
        items.push({ type: "submenu", id: entry.submenu, label: t(`submenu.${entry.submenu}`), items: children });
      }
    } else {
      const state = actionState(entry.action, ctx, deps);
      if (!state.visible) {
        continue;
      }
      const binding = bindingFor(entry.action, ctx.pane);
      items.push({
        type: "action",
        id: entry.action,
        label: t(`action.${entry.action}`),
        shortcut: binding === null ? null : formatBinding(binding),
        ariaShortcut: binding === null ? null : ariaBinding(binding),
        danger: deps.defs[entry.action]?.danger === true,
        disabled: state.disabled !== null,
        hint: state.disabled,
        run: () => {
          runAction(entry.action, ctx, deps);
        },
      });
    }
  }
  return items;
}

/** Drops separators at the start, at the end and next to each other. */
function tidy(items: ResolvedItem[]): ResolvedItem[] {
  const result: ResolvedItem[] = [];
  for (const item of items) {
    const previous = result[result.length - 1];
    if (item.type === "separator" && (previous === undefined || previous.type === "separator")) {
      continue;
    }
    result.push(item);
  }
  while (result[result.length - 1]?.type === "separator") {
    result.pop();
  }
  return result;
}

/**
 * Resolves a menu against a context: the entries of the layout that are
 * available, supported by the endpoint and applicable to the target, with
 * their enabled state, label and shortcut. It reads only data that is already
 * loaded and does no network work.
 */
export function resolveMenu(
  menu: MenuId,
  ctx: MenuContext,
  deps: RegistryDeps = DEFAULT_DEPS,
  layouts: Readonly<Record<MenuId, readonly LayoutEntry[]>> = LAYOUTS,
): ResolvedItem[] {
  return tidy(resolveEntries(layouts[menu], ctx, deps));
}

/** Every action id that a layout mentions, submenus included. */
export function layoutActionIds(entries: readonly LayoutEntry[]): ActionId[] {
  return entries.flatMap((entry) => {
    if ("separator" in entry) {
      return [];
    }
    return "submenu" in entry ? layoutActionIds(entry.items) : [entry.action];
  });
}
