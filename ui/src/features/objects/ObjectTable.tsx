import { Text, UnstyledButton } from "@mantine/core";
import { IconChevronDown, IconChevronUp, IconFile, IconFolderFilled } from "@tabler/icons-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  memo,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useState,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import type { ObjectEntry } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { getLocale, subscribeLocale, tr } from "../../i18n";
import { formatBytes, formatDateTime } from "../../lib/format";
import { isFolderEntry, type SortColumn } from "../../lib/objects";
import { displaySegment, relativeName } from "../../lib/s3keys";
import { moveFocus, selectOnly, selectRange, toggle } from "../../lib/selection";
import { filesPaneContext } from "../menus/context";
import { ContextMenu } from "../menus/ContextMenu";
import { actionForKey } from "../menus/keymap";
import { runAction } from "../menus/registry";
import { filesEmptyMenu, itemsMenu } from "../menus/surfaces";
import { isContextClick, isMenuKey, useContextMenu } from "../menus/useContextMenu";
import { openEntry } from "./actions";

export const OBJECT_ROW_HEIGHT = 28;

const COLUMNS: { id: SortColumn; label: string; sortKey: string; className: string }[] = [
  { id: "name", label: "Name", sortKey: "Sort by name", className: "bb-col-name" },
  { id: "size", label: "Size", sortKey: "Sort by size", className: "bb-col-size" },
  { id: "modified", label: "Last modified", sortKey: "Sort by last modified", className: "bb-col-modified" },
  { id: "class", label: "Storage class", sortKey: "Sort by storage class", className: "bb-col-class" },
];

interface RowProps {
  entry: ObjectEntry;
  index: number;
  rowId: string;
  bucket: string | null;
  name: string;
  selected: boolean;
  focused: boolean;
  cut: boolean;
  top: number;
  onClick: (event: MouseEvent, key: string) => void;
  onContextMenu: (event: MouseEvent, entry: ObjectEntry) => void;
  onOpen: (entry: ObjectEntry) => void;
}

const ObjectRow = memo(function ObjectRow({
  entry,
  index,
  rowId,
  bucket,
  name,
  selected,
  focused,
  cut,
  top,
  onClick,
  onContextMenu,
  onOpen,
}: RowProps) {
  // The row is memoised, so it subscribes to the language itself.
  useSyncExternalStore(subscribeLocale, getLocale);
  const folder = isFolderEntry(entry);
  return (
    <div
      id={rowId}
      role="row"
      aria-rowindex={index + 2}
      aria-selected={selected}
      className="bb-row"
      data-selected={selected || undefined}
      data-focused={focused || undefined}
      data-cut={cut || undefined}
      data-key={entry.key}
      data-drop-bucket={folder && bucket !== null ? bucket : undefined}
      data-drop-prefix={folder && bucket !== null ? entry.key : undefined}
      style={{ transform: `translateY(${top}px)`, height: OBJECT_ROW_HEIGHT }}
      onClick={(event) => (isContextClick(event) ? onContextMenu(event, entry) : onClick(event, entry.key))}
      onContextMenu={(event) => onContextMenu(event, entry)}
      onDoubleClick={() => onOpen(entry)}
    >
      <div role="gridcell" className="bb-col-name">
        {folder ? (
          <IconFolderFilled size={16} className="bb-folder-icon" aria-hidden />
        ) : (
          <IconFile size={16} className="bb-file-icon" aria-hidden />
        )}
        <span className="bb-ellipsis" data-empty-name={name === "" || undefined} title={entry.key}>
          {displaySegment(name)}
        </span>
      </div>
      <div role="gridcell" className="bb-col-size">
        {folder ? "" : formatBytes(entry.size)}
      </div>
      <div role="gridcell" className="bb-col-modified">
        {formatDateTime(entry.last_modified)}
      </div>
      <div role="gridcell" className="bb-col-class">
        {entry.storage_class ?? ""}
      </div>
    </div>
  );
});

type WidthColumn = "size" | "modified" | "class";
type Widths = Record<WidthColumn, number>;
const MIN_COLUMN_WIDTH = 40;
const MAX_COLUMN_WIDTH = 600;
const KEY_STEP = 8;

function clampWidth(value: number): number {
  return Math.min(MAX_COLUMN_WIDTH, Math.max(MIN_COLUMN_WIDTH, Math.round(value)));
}

interface ResizeHandleProps {
  column: WidthColumn;
  label: string;
  width: number;
  onResize: (width: number) => void;
  onCommit: (width: number) => void;
}

/**
 * Grip on the left edge of a fixed-width column. Dragging it to the left widens the column (the name column
 * gives way); the arrow keys do the same in steps. The width is saved when the drag or key press ends.
 */
function ResizeHandle({ column, label, width, onResize, onCommit }: ResizeHandleProps) {
  const start = useRef<{ x: number; width: number } | null>(null);
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={width}
      aria-valuemin={MIN_COLUMN_WIDTH}
      aria-valuemax={MAX_COLUMN_WIDTH}
      tabIndex={0}
      data-resize={column}
      className="bb-col-resize"
      onClick={(event) => event.stopPropagation()}
      onPointerDown={(event) => {
        event.preventDefault();
        try {
          event.currentTarget.setPointerCapture(event.pointerId);
        } catch {
          // Not every environment can capture the pointer; the drag still works while it stays over the grip.
        }
        start.current = { x: event.clientX, width };
      }}
      onPointerMove={(event) => {
        if (start.current !== null) {
          onResize(clampWidth(start.current.width + (start.current.x - event.clientX)));
        }
      }}
      onPointerUp={(event) => {
        if (start.current !== null) {
          const next = clampWidth(start.current.width + (start.current.x - event.clientX));
          start.current = null;
          onCommit(next);
        }
      }}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
          event.preventDefault();
          event.stopPropagation();
          const next = clampWidth(width + (event.key === "ArrowLeft" ? KEY_STEP : -KEY_STEP));
          onResize(next);
          onCommit(next);
        }
      }}
    />
  );
}

export interface ObjectTableProps {
  /** Shown instead of rows while loading, or when the folder is empty or nothing matches. */
  placeholder?: ReactNode;
  busy?: boolean;
}

/** Virtualised, sortable, multi-select table of the loaded objects. */
export function ObjectTable({ placeholder = null, busy = false }: ObjectTableProps) {
  const model = useModel();
  const visible = useSlice(model.browser.store, (state) => state.visible);
  const visibleKeys = useSlice(model.browser.store, (state) => state.visibleKeys);
  const selection = useSlice(model.browser.store, (state) => state.selection);
  const sort = useSlice(model.browser.store, (state) => state.sort);
  const prefix = useSlice(model.browser.store, (state) => state.listing.prefix);
  const bucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const clipboard = useSlice(model.clipboard, (state) => state.contents);
  const savedWidths = useSlice(model.settings.store, (state) => state.settings.interface.column_widths);
  const [widths, setWidths] = useState<Widths>(savedWidths);
  useEffect(() => {
    setWidths(savedWidths);
  }, [savedWidths]);
  const saveWidth = (column: WidthColumn, width: number) => {
    void model.settings
      .update((draft) => {
        draft.interface.column_widths = { ...draft.interface.column_widths, [column]: width };
      })
      .catch((error: unknown) => model.status.error(error));
  };
  const scrollRef = useRef<HTMLDivElement>(null);
  const gridId = useId();
  const virtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => OBJECT_ROW_HEIGHT,
    overscan: 12,
  });
  const cutKeys = useMemo(
    () =>
      clipboard !== null && clipboard.mode === "cut" && clipboard.bucket === bucket
        ? new Set(clipboard.keys)
        : new Set<string>(),
    [clipboard, bucket],
  );
  const focusIndex = selection.focus === null ? -1 : visibleKeys.indexOf(selection.focus);

  const onRowClick = useCallback(
    (event: MouseEvent, key: string) => {
      const additive = event.ctrlKey || event.metaKey;
      const current = model.browser.state.selection;
      if (event.shiftKey) {
        model.browser.setSelection(selectRange(current, model.browser.state.visibleKeys, key, additive));
      } else if (additive) {
        model.browser.setSelection(toggle(current, key));
      } else {
        model.browser.setSelection(selectOnly(key));
      }
    },
    [model],
  );
  const onOpen = useCallback(
    (entry: ObjectEntry) => {
      void openEntry(model, entry);
    },
    [model],
  );

  const menu = useContextMenu();
  // The stable opener keeps the memoised rows from rendering again whenever the table does.
  const open = menu.open;
  /** The menu of the rows the user pointed at: an unselected row becomes the selection first. */
  const openRowMenu = useCallback(
    (event: MouseEvent, entry: ObjectEntry) => {
      const state = model.browser.state;
      if (state.listing.bucket === null) {
        return;
      }
      if (!state.selection.keys.has(entry.key)) {
        model.browser.setSelection(selectOnly(entry.key));
      }
      const built = itemsMenu(model, state.listing.bucket, model.browser.selectedEntries(), "files");
      open(event, built.items, built.label);
    },
    [model, open],
  );
  const openEmptyMenu = useCallback(
    (event: MouseEvent) => {
      const built = filesEmptyMenu(model);
      open(event, built.items, built.label);
    },
    [model, open],
  );

  const move = (delta: number, extend: boolean) => {
    const state = model.browser.state;
    const next = moveFocus(state.selection, state.visibleKeys, delta, extend);
    model.browser.setSelection(next);
    const index = next.focus === null ? -1 : state.visibleKeys.indexOf(next.focus);
    if (index >= 0) {
      virtualizer.scrollToIndex(index, { align: "auto" });
    }
  };

  /** Opens the menu of the focused row (or of the empty area) from the keyboard, beside that row. */
  const openMenuFromKeyboard = () => {
    const state = model.browser.state;
    const row = focusIndex >= 0 ? document.getElementById(`${gridId}-row-${focusIndex}`) : null;
    const box = (row ?? scrollRef.current)?.getBoundingClientRect();
    const x = (box?.left ?? 0) + 24;
    const y = row === null ? (box?.top ?? 0) : (box?.bottom ?? 0);
    const focused = state.visible.find((entry) => entry.key === state.selection.focus);
    if (focused !== undefined && state.listing.bucket !== null) {
      if (!state.selection.keys.has(focused.key)) {
        model.browser.setSelection(selectOnly(focused.key));
      }
      const built = itemsMenu(model, state.listing.bucket, model.browser.selectedEntries(), "files");
      menu.openAt(x, y, built.items, built.label);
    } else {
      const built = filesEmptyMenu(model);
      menu.openAt(x, y, built.items, built.label);
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    // Keys pressed on buttons inside the table (sorting, upload) keep their own meaning.
    if (event.target !== event.currentTarget) {
      return;
    }
    const key = event.key;
    const pageRows = Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 400) / OBJECT_ROW_HEIGHT) - 1);
    const state = model.browser.state;
    const shortcut = actionForKey(event, "files");
    if (key === "ArrowDown") {
      move(1, event.shiftKey);
    } else if (key === "ArrowUp" && event.altKey) {
      model.browser.goUp();
    } else if (key === "ArrowUp") {
      move(-1, event.shiftKey);
    } else if (key === "PageDown") {
      move(pageRows, event.shiftKey);
    } else if (key === "PageUp") {
      move(-pageRows, event.shiftKey);
    } else if (key === "Home") {
      move(-Infinity, event.shiftKey);
    } else if (key === "End") {
      move(Infinity, event.shiftKey);
    } else if (key === " ") {
      if (state.selection.focus !== null) {
        model.browser.setSelection(toggle(state.selection, state.selection.focus));
      }
    } else if (isMenuKey(event)) {
      openMenuFromKeyboard();
    } else if (shortcut !== null) {
      // Every other shortcut is an action of the registry, the same one the menus and the toolbar run.
      // Enter acts on the focused row, the others on the selection.
      const focused = state.visible.find((entry) => entry.key === state.selection.focus);
      runAction(shortcut, filesPaneContext(model, shortcut === "open" ? (focused ? [focused] : []) : undefined));
    } else if (key === "Backspace") {
      model.browser.goUp();
    } else if (key === "Escape" && state.selection.keys.size > 0) {
      model.browser.setSelection({ keys: new Set(), anchor: null, focus: state.selection.focus });
    } else {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
  };

  const items = virtualizer.getVirtualItems();
  return (
    <div
      className="bb-grid"
      role="grid"
      aria-label={tr("Objects")}
      aria-rowcount={placeholder === null ? visible.length + 1 : 2}
      aria-multiselectable
      aria-busy={busy || undefined}
      style={
        {
          "--bb-col-size": `${widths.size}px`,
          "--bb-col-modified": `${widths.modified}px`,
          "--bb-col-class": `${widths.class}px`,
        } as CSSProperties
      }
      aria-activedescendant={placeholder === null && focusIndex >= 0 ? `${gridId}-row-${focusIndex}` : undefined}
      tabIndex={0}
      onKeyDown={onKeyDown}
      onContextMenu={openEmptyMenu}
      onClick={(event) => {
        if (isContextClick(event)) {
          openEmptyMenu(event);
        }
      }}
    >
      <div role="rowgroup" className="bb-grid-head">
        <div role="row" aria-rowindex={1} className="bb-row bb-head-row">
          {COLUMNS.map((column) => {
            const active = sort.column === column.id;
            const resizable: WidthColumn | null = column.id === "name" ? null : column.id;
            return (
              <div
                key={column.id}
                role="columnheader"
                className={column.className}
                aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
              >
                <UnstyledButton
                  className="bb-sort-button"
                  onClick={() => model.browser.setSort(column.id)}
                  aria-label={tr(column.sortKey)}
                >
                  <Text size="xs" fw={600} span>
                    {tr(column.label)}
                  </Text>
                  {active ? (
                    sort.direction === "asc" ? (
                      <IconChevronUp size={12} aria-hidden />
                    ) : (
                      <IconChevronDown size={12} aria-hidden />
                    )
                  ) : null}
                </UnstyledButton>
                {resizable !== null ? (
                  <ResizeHandle
                    column={resizable}
                    label={tr("Resize the {column} column", { column: tr(column.label) })}
                    width={widths[resizable]}
                    onResize={(width) => setWidths((current) => ({ ...current, [resizable]: width }))}
                    onCommit={(width) => saveWidth(resizable, width)}
                  />
                ) : null}
              </div>
            );
          })}
        </div>
      </div>
      <div ref={scrollRef} className="bb-grid-scroll" role="rowgroup">
        {placeholder !== null ? (
          <div role="row" aria-rowindex={2} className="bb-state-row">
            <div role="gridcell" aria-colspan={COLUMNS.length}>
              {placeholder}
            </div>
          </div>
        ) : (
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((item) => {
            const entry = visible[item.index];
            if (!entry) {
              return null;
            }
            return (
              <ObjectRow
                key={entry.key}
                entry={entry}
                index={item.index}
                rowId={`${gridId}-row-${item.index}`}
                bucket={bucket}
                name={relativeName(entry.key, prefix)}
                selected={selection.keys.has(entry.key)}
                focused={item.index === focusIndex}
                cut={cutKeys.has(entry.key)}
                top={item.start}
                onClick={onRowClick}
                onContextMenu={openRowMenu}
                onOpen={onOpen}
              />
            );
          })}
        </div>
        )}
      </div>
      <ContextMenu menu={menu.state} onClose={menu.close} />
    </div>
  );
}
