import { IconChevronRight } from "@tabler/icons-react";
import {
  type CSSProperties,
  type KeyboardEvent,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import type { SubmenuId } from "./ids";
import { ACTION_ICONS } from "./icons";
import type { ResolvedAction, ResolvedItem, ResolvedSubmenu } from "./resolve";
import type { MenuState } from "./useContextMenu";

/** A hovered submenu opens after this delay. */
export const SUBMENU_DELAY_MS = 200;
/** Typing pauses longer than this start a new type-ahead search. */
const TYPEAHEAD_RESET_MS = 700;
/** Smallest distance between a menu and the edge of the window. */
const MARGIN = 4;

export interface ContextMenuProps {
  menu: MenuState | null;
  onClose: () => void;
}

/** The enabled entries of one menu level, in order. */
function itemButtons(list: HTMLElement): HTMLButtonElement[] {
  const buttons: HTMLButtonElement[] = [];
  for (const entry of Array.from(list.children)) {
    const button = entry.firstElementChild;
    if (entry.classList.contains("bb-menu-entry") && button instanceof HTMLButtonElement && !button.disabled) {
      buttons.push(button);
    }
  }
  return buttons;
}

interface MenuListProps {
  items: ResolvedItem[];
  label: string;
  /** Closes the whole menu. */
  onCloseAll: () => void;
  /** Runs an entry; the caller closes the menu first. */
  onActivate: (item: ResolvedAction) => void;
  /** Set for a submenu: closes it and returns to the entry that opened it. */
  onBack?: () => void;
  /** Moves keyboard focus to the first entry when the list appears. */
  focusFirst?: boolean;
  className: string;
  style?: CSSProperties;
  listRef?: RefObject<HTMLDivElement | null>;
}

function MenuList({ items, label, onCloseAll, onActivate, onBack, focusFirst = false, className, style, listRef }: MenuListProps) {
  const innerRef = useRef<HTMLDivElement | null>(null);
  const [openSub, setOpenSub] = useState<{ id: SubmenuId; keyboard: boolean } | null>(null);
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const typed = useRef<{ text: string; timer: ReturnType<typeof setTimeout> | null }>({ text: "", timer: null });
  const triggers = useRef(new Map<SubmenuId, HTMLButtonElement>());

  const setNode = (node: HTMLDivElement | null) => {
    innerRef.current = node;
    if (listRef) {
      listRef.current = node;
    }
  };

  useLayoutEffect(() => {
    if (focusFirst && innerRef.current) {
      itemButtons(innerRef.current)[0]?.focus();
    }
  }, [focusFirst]);

  useEffect(
    () => () => {
      if (hoverTimer.current !== null) {
        clearTimeout(hoverTimer.current);
      }
      if (typed.current.timer !== null) {
        clearTimeout(typed.current.timer);
      }
    },
    [],
  );

  const clearHover = () => {
    if (hoverTimer.current !== null) {
      clearTimeout(hoverTimer.current);
      hoverTimer.current = null;
    }
  };
  const openNow = (id: SubmenuId, keyboard: boolean) => {
    clearHover();
    setOpenSub({ id, keyboard });
  };
  const closeSub = (id: SubmenuId) => {
    // Focus leaves the submenu before it is removed, so the menu does not see focus being lost.
    triggers.current.get(id)?.focus();
    setOpenSub(null);
  };
  const hoverOther = () => {
    clearHover();
    if (openSub !== null) {
      innerRef.current?.focus();
      setOpenSub(null);
    }
  };
  const hoverSubmenu = (id: SubmenuId) => {
    clearHover();
    if (openSub?.id === id) {
      return;
    }
    hoverTimer.current = setTimeout(() => {
      hoverTimer.current = null;
      setOpenSub({ id, keyboard: false });
    }, SUBMENU_DELAY_MS);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const list = event.currentTarget;
    const target = event.target;
    // Keys of a submenu are handled there; anything else that bubbles up is ignored.
    if (!(target instanceof HTMLButtonElement) || target.closest('[role="menu"]') !== list) {
      return;
    }
    const buttons = itemButtons(list);
    const index = buttons.indexOf(target);
    const focusAt = (position: number) => buttons[(position + buttons.length) % buttons.length]?.focus();
    const submenuId = target.dataset.submenuId as SubmenuId | undefined;
    // Type-ahead: the typed text picks the next entry whose label starts with it.
    const typeAhead = (character: string) => {
      const text = typed.current.text + character.toLowerCase();
      if (typed.current.timer !== null) {
        clearTimeout(typed.current.timer);
      }
      typed.current = {
        text,
        timer: setTimeout(() => {
          typed.current = { text: "", timer: null };
        }, TYPEAHEAD_RESET_MS),
      };
      // The same letter again cycles through the entries that start with it.
      const cycling = [...text].every((letter) => letter === text[0]);
      const search = cycling ? text.slice(0, 1) : text;
      const start = cycling ? index + 1 : Math.max(index, 0);
      const ordered = [...buttons.slice(start), ...buttons.slice(0, start)];
      ordered.find((button) => (button.dataset.label ?? "").toLowerCase().startsWith(search))?.focus();
    };
    // A space in the middle of typing is part of the text; otherwise it runs the entry like Enter.
    if (event.key === " " && typed.current.text !== "") {
      typeAhead(" ");
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        focusAt(index < 0 ? 0 : index + 1);
        break;
      case "ArrowUp":
        focusAt(index < 0 ? -1 : index - 1);
        break;
      case "Home":
        focusAt(0);
        break;
      case "End":
        focusAt(-1);
        break;
      case "ArrowRight":
      case "Enter":
      case " ":
        // Entries run on Enter and Space by themselves; submenu entries open instead.
        if (submenuId === undefined) {
          return;
        }
        openNow(submenuId, true);
        break;
      case "ArrowLeft":
        if (onBack === undefined) {
          return;
        }
        onBack();
        break;
      case "Escape":
        if (onBack === undefined) {
          onCloseAll();
        } else {
          onBack();
        }
        break;
      case "Tab":
        onCloseAll();
        break;
      default:
        if (event.key.length !== 1 || event.ctrlKey || event.metaKey || event.altKey) {
          return;
        }
        typeAhead(event.key);
    }
    event.preventDefault();
    event.stopPropagation();
  };

  return (
    <div
      ref={setNode}
      role="menu"
      aria-label={label}
      aria-orientation="vertical"
      tabIndex={-1}
      data-mantine-stop-propagation="true"
      className={className}
      style={style}
      onKeyDown={onKeyDown}
      // A right-click on the menu itself neither opens another menu nor the browser's own.
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      {items.map((item, index) => {
        if (item.type === "separator") {
          return <div key={`separator-${index}`} role="separator" className="bb-menu-separator" />;
        }
        if (item.type === "submenu") {
          return (
            <SubmenuEntry
              key={`submenu-${item.id}`}
              item={item}
              open={openSub?.id === item.id}
              focusFirst={openSub?.id === item.id && openSub.keyboard}
              registerTrigger={(button) => {
                if (button === null) {
                  triggers.current.delete(item.id);
                } else {
                  triggers.current.set(item.id, button);
                }
              }}
              onHover={() => hoverSubmenu(item.id)}
              onOpen={(keyboard) => openNow(item.id, keyboard)}
              onBack={() => closeSub(item.id)}
              onCloseAll={onCloseAll}
              onActivate={onActivate}
            />
          );
        }
        return (
          <div key={`action-${item.id}-${index}`} role="none" className="bb-menu-entry" onMouseEnter={hoverOther}>
            <ActionButton item={item} onActivate={onActivate} />
          </div>
        );
      })}
    </div>
  );
}

function ItemContent({ id, label, shortcut, chevron }: { id: ResolvedAction["id"] | null; label: string; shortcut?: string | null; chevron?: boolean }) {
  const icon = id === null ? undefined : ACTION_ICONS[id];
  return (
    <>
      <span className="bb-menu-icon" aria-hidden>
        {icon}
      </span>
      <span className="bb-ellipsis">{label}</span>
      {shortcut ? (
        <span className="bb-menu-shortcut" aria-hidden>
          {shortcut}
        </span>
      ) : null}
      {chevron ? (
        <span className="bb-menu-chevron" aria-hidden>
          <IconChevronRight size={12} />
        </span>
      ) : null}
    </>
  );
}

function ActionButton({ item, onActivate }: { item: ResolvedAction; onActivate: (item: ResolvedAction) => void }) {
  return (
    <button
      type="button"
      role="menuitem"
      tabIndex={-1}
      disabled={item.disabled}
      title={item.disabled ? (item.hint ?? undefined) : undefined}
      aria-keyshortcuts={item.ariaShortcut ?? undefined}
      data-mantine-stop-propagation="true"
      data-action-id={item.id}
      data-label={item.label}
      className={`bb-menu-item${item.danger ? " bb-menu-item-danger" : ""}`}
      // Hovering moves focus, so the mouse and the keyboard share one current entry.
      onMouseEnter={(event) => event.currentTarget.focus()}
      onClick={() => onActivate(item)}
    >
      <ItemContent id={item.id} label={item.label} shortcut={item.shortcut} />
    </button>
  );
}

interface SubmenuEntryProps {
  item: ResolvedSubmenu;
  open: boolean;
  focusFirst: boolean;
  registerTrigger: (button: HTMLButtonElement | null) => void;
  onHover: () => void;
  onOpen: (keyboard: boolean) => void;
  onBack: () => void;
  onCloseAll: () => void;
  onActivate: (item: ResolvedAction) => void;
}

function SubmenuEntry({
  item,
  open,
  focusFirst,
  registerTrigger,
  onHover,
  onOpen,
  onBack,
  onCloseAll,
  onActivate,
}: SubmenuEntryProps) {
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  return (
    <div role="none" className="bb-menu-entry" onMouseEnter={onHover}>
      <button
        ref={(button) => {
          triggerRef.current = button;
          registerTrigger(button);
        }}
        type="button"
        role="menuitem"
        tabIndex={-1}
        aria-haspopup="menu"
        aria-expanded={open}
        data-mantine-stop-propagation="true"
        data-submenu-id={item.id}
        data-label={item.label}
        className="bb-menu-item"
        onMouseEnter={(event) => event.currentTarget.focus()}
        onClick={(event) => onOpen(event.detail === 0)}
      >
        <ItemContent id={null} label={item.label} chevron />
      </button>
      {open ? (
        <SubmenuPanel
          anchor={triggerRef}
          items={item.items}
          label={item.label}
          focusFirst={focusFirst}
          onBack={onBack}
          onCloseAll={onCloseAll}
          onActivate={onActivate}
        />
      ) : null}
    </div>
  );
}

/** A submenu beside the entry that opened it, flipped and moved to stay inside the window. */
function SubmenuPanel({
  anchor,
  ...list
}: Omit<MenuListProps, "className" | "style" | "listRef"> & { anchor: RefObject<HTMLButtonElement | null> }) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const [style, setStyle] = useState<CSSProperties>({ left: 0, top: 0, visibility: "hidden" });
  useLayoutEffect(() => {
    const panel = panelRef.current;
    const trigger = anchor.current;
    if (panel === null || trigger === null) {
      return;
    }
    const from = trigger.getBoundingClientRect();
    const box = panel.getBoundingClientRect();
    const fitsRight = from.right + box.width <= window.innerWidth - MARGIN;
    setStyle({
      left: fitsRight ? from.right : Math.max(MARGIN, from.left - box.width),
      top: Math.max(MARGIN, Math.min(from.top - MARGIN, window.innerHeight - box.height - MARGIN)),
    });
  }, [anchor]);
  return <MenuList {...list} className="bb-context-menu bb-submenu" style={style} listRef={panelRef} />;
}

/**
 * Right-click menu in a portal at the pointer, kept inside the window. It
 * closes on Escape, on a press outside, when focus or the window is lost and on
 * scroll or resize; focus returns to the element that had it. It renders the
 * entries it is given and does no network work.
 */
export function ContextMenu({ menu, onClose }: ContextMenuProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [placed, setPlaced] = useState<{ id: number; x: number; y: number } | null>(null);

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (menu === null || root === null) {
      return undefined;
    }
    const box = root.getBoundingClientRect();
    setPlaced({
      id: menu.id,
      x: Math.max(MARGIN, Math.min(menu.x, window.innerWidth - box.width - MARGIN)),
      y: Math.max(MARGIN, Math.min(menu.y, window.innerHeight - box.height - MARGIN)),
    });
    const inside = (target: EventTarget | null) => target instanceof Node && root.contains(target);
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      // Escape inside the menu is handled by the menu; anywhere else it closes the menu before a dialog sees it.
      if (event.key === "Escape" && !inside(event.target)) {
        event.preventDefault();
        event.stopPropagation();
        onClose();
      }
    };
    const onPointerDown = (event: PointerEvent) => {
      if (!inside(event.target)) {
        onClose();
      }
    };
    const onScroll = (event: Event) => {
      if (!inside(event.target)) {
        onClose();
      }
    };
    const onFocusOut = (event: FocusEvent) => {
      if (event.relatedTarget instanceof Node && !root.contains(event.relatedTarget)) {
        onClose();
      }
    };
    const onWindowBlur = () => onClose();
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onWindowBlur);
    root.addEventListener("focusout", onFocusOut);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onWindowBlur);
      root.removeEventListener("focusout", onFocusOut);
      // Focus goes back to where it was, unless the entry that ran has already moved it somewhere else
      // (for example into the filter box).
      const active = document.activeElement;
      const movedAway = active !== null && active !== document.body && !root.contains(active);
      const back = menu.returnFocus;
      if (back?.isConnected === true && !movedAway) {
        back.focus();
      }
    };
  }, [menu, onClose]);

  if (menu === null) {
    return null;
  }
  const position = placed?.id === menu.id ? placed : { x: menu.x, y: menu.y };
  return createPortal(
    <MenuList
      key={menu.id}
      items={menu.items}
      label={menu.label}
      focusFirst
      listRef={rootRef}
      className="bb-context-menu"
      style={{ left: position.x, top: position.y }}
      onCloseAll={onClose}
      onActivate={(item) => {
        onClose();
        item.run();
      }}
    />,
    document.body,
  );
}
