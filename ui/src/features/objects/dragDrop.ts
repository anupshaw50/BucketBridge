import type { DragIncoming, DragItems } from "../../api/types";
import { tr } from "../../i18n";
import { formatCount } from "../../lib/format";
import { type DragSource, type DropTarget, isMoveDrag, moveModifier, planDrop } from "../../lib/dnd";
import { orderedSelection, selectOnly } from "../../lib/selection";
import type { AppModel } from "../../state/model";
import { transferKeys, uploadPaths } from "./actions";

/** Pixels the pointer must travel before a press on a row becomes a drag. */
export const DRAG_THRESHOLD = 5;

const OVER_ATTRIBUTE = "data-drop-over";
const FEEDBACK_ID = "bb-drag-feedback";

type ElementAt = (x: number, y: number) => Element | null;

function defaultElementAt(x: number, y: number): Element | null {
  return typeof document.elementFromPoint === "function" ? document.elementFromPoint(x, y) : null;
}

/** A folder target under the pointer: the closest marked element, or the list of the open folder. */
export function targetAt(model: AppModel, element: Element | null): { target: DropTarget; element: Element } | null {
  const accountId = model.session.store.getState().activeAccountId;
  if (element === null || accountId === null) {
    return null;
  }
  const marked = element.closest("[data-drop-bucket]");
  if (marked instanceof HTMLElement && marked.dataset.dropBucket !== undefined) {
    return {
      target: { accountId, bucket: marked.dataset.dropBucket, prefix: marked.dataset.dropPrefix ?? "" },
      element: marked,
    };
  }
  const grid = element.closest(".bb-grid");
  const listing = model.browser.state.listing;
  if (grid !== null && listing.bucket !== null && listing.accountId === accountId) {
    return { target: { accountId, bucket: listing.bucket, prefix: listing.prefix }, element: grid };
  }
  return null;
}

/** A row of the dragged items themselves: dropping there is not a drop anywhere. */
function isOwnRow(element: Element, source: DragSource): boolean {
  return element instanceof HTMLElement && element.dataset.key !== undefined && source.keys.includes(element.dataset.key);
}

function placeLabel(target: DropTarget): string {
  return `${target.bucket}/${target.prefix}`;
}

function whatLabel(count: number): string {
  return count === 1 ? tr("1 item") : tr("{n} items", { n: formatCount(count) });
}

/** The little label that follows the pointer, and the highlight of the folder it is over. */
class Feedback {
  private box: HTMLDivElement | null = null;
  private over: Element | null = null;

  show(x: number, y: number, text: string, blocked: boolean): void {
    if (this.box === null) {
      const box = document.createElement("div");
      box.id = FEEDBACK_ID;
      box.className = "bb-drag-feedback";
      box.setAttribute("role", "status");
      document.body.appendChild(box);
      this.box = box;
    }
    this.box.textContent = text;
    this.box.dataset.blocked = blocked ? "true" : "false";
    this.box.style.left = `${x + 14}px`;
    this.box.style.top = `${y + 14}px`;
  }

  highlight(element: Element | null): void {
    if (this.over === element) {
      return;
    }
    this.over?.removeAttribute(OVER_ATTRIBUTE);
    this.over = element;
    element?.setAttribute(OVER_ATTRIBUTE, "");
  }

  hideLabel(): void {
    this.box?.remove();
    this.box = null;
  }

  clear(): void {
    this.hideLabel();
    this.highlight(null);
  }
}

/** The text shown while the pointer is over `target`; the key to hold for a move is named while copying. */
function feedbackText(source: DragSource, target: DropTarget, move: boolean): { text: string; blocked: boolean } | null {
  const plan = planDrop(source, target, move);
  if (plan.kind === "noop") {
    return null;
  }
  if (plan.kind === "blocked") {
    return { text: plan.message, blocked: true };
  }
  const what = whatLabel(source.keys.length);
  const where = placeLabel(target);
  const key = moveModifier() === "meta" ? tr("Command") : tr("Shift");
  return {
    text:
      plan.mode === "copy"
        ? `${tr("Copy {what} to {where}", { what, where })} (${tr("hold {key} to move", { key })})`
        : tr("Move {what} to {where}", { what, where }),
    blocked: false,
  };
}

/** Carries out a drop of objects. Returns whether anything was queued. */
export async function dropObjects(model: AppModel, source: DragSource, target: DropTarget, move: boolean): Promise<boolean> {
  const plan = planDrop(source, target, move);
  if (plan.kind === "noop") {
    return false;
  }
  if (plan.kind === "blocked") {
    model.status.error(plan.message, { notify: tr("Drop not started") });
    return false;
  }
  return transferKeys(model, plan.mode, plan.args);
}

function toSource(items: DragItems): DragSource {
  return { accountId: items.account_id, bucket: items.bucket, keys: items.keys };
}

export interface DragOptions {
  elementAt?: ElementAt;
}

/**
 * Pointer-event drag and drop of objects, deliberately not HTML5 drag and drop
 * (the desktop host's file-drop handling breaks that inside the page on
 * Windows). A press on a row followed by movement drags the selected rows
 * (or the pressed row when it was not selected). The drop target is the
 * folder under the pointer; a drag that leaves the window is relayed to the
 * window under the pointer with `relay_drag`. Returns the cleanup.
 */
export function installObjectDrag(model: AppModel, options: DragOptions = {}): () => void {
  const elementAt = options.elementAt ?? defaultElementAt;
  const feedback = new Feedback();
  let press: { x: number; y: number; key: string; pointerId: number } | null = null;
  let source: DragSource | null = null;
  let relaying = false;

  const stopListening = () => {
    window.removeEventListener("pointermove", onMove, true);
    window.removeEventListener("pointerup", onUp, true);
    window.removeEventListener("pointercancel", onCancel, true);
    window.removeEventListener("keydown", onKey, true);
  };

  const finish = () => {
    if (relaying) {
      relaying = false;
      void model.raw.relay_drag({ phase: "leave", items: null, move_items: false }).catch(() => undefined);
    }
    feedback.clear();
    stopListening();
    if (source !== null) {
      // The release that ends a drag must not count as a click on the row.
      const swallow = (event: Event) => event.stopPropagation();
      window.addEventListener("click", swallow, true);
      setTimeout(() => window.removeEventListener("click", swallow, true), 0);
    }
    press = null;
    source = null;
  };

  const begin = (key: string): DragSource | null => {
    const location = model.browser.state.listing;
    if (location.accountId === null || location.bucket === null) {
      return null;
    }
    const state = model.browser.state;
    let keys: string[];
    if (state.selection.keys.has(key)) {
      keys = orderedSelection(state.selection, state.visibleKeys);
    } else {
      model.browser.setSelection(selectOnly(key));
      keys = [key];
    }
    return keys.length === 0 ? null : { accountId: location.accountId, bucket: location.bucket, keys };
  };

  const outside = (x: number, y: number) => x < 0 || y < 0 || x >= window.innerWidth || y >= window.innerHeight;

  function onMove(event: PointerEvent): void {
    if (press?.pointerId !== event.pointerId) {
      return;
    }
    if (source === null) {
      if (Math.hypot(event.clientX - press.x, event.clientY - press.y) < DRAG_THRESHOLD) {
        return;
      }
      source = begin(press.key);
      if (source === null) {
        finish();
        return;
      }
      try {
        document.documentElement.setPointerCapture(event.pointerId);
      } catch {
        // Capture keeps events coming outside the window; without it the drag stays in the window.
      }
    }
    const move = isMoveDrag(event);
    if (outside(event.clientX, event.clientY)) {
      feedback.clear();
      relaying = true;
      const items: DragItems = { account_id: source.accountId, bucket: source.bucket, keys: source.keys };
      void model.raw.relay_drag({ phase: "over", items, move_items: move }).catch(() => undefined);
      return;
    }
    if (relaying) {
      relaying = false;
      void model.raw.relay_drag({ phase: "leave", items: null, move_items: false }).catch(() => undefined);
    }
    const hit = targetAt(model, elementAt(event.clientX, event.clientY));
    if (hit === null || isOwnRow(hit.element, source)) {
      feedback.clear();
      return;
    }
    const shown = feedbackText(source, hit.target, move);
    if (shown === null) {
      feedback.clear();
      return;
    }
    feedback.highlight(shown.blocked ? null : hit.element);
    feedback.show(event.clientX, event.clientY, shown.text, shown.blocked);
  }

  function onUp(event: PointerEvent): void {
    if (press?.pointerId !== event.pointerId) {
      return;
    }
    const dragged = source;
    const move = isMoveDrag(event);
    if (dragged === null) {
      finish();
      return;
    }
    if (outside(event.clientX, event.clientY)) {
      const items: DragItems = { account_id: dragged.accountId, bucket: dragged.bucket, keys: dragged.keys };
      relaying = false;
      void model.raw.relay_drag({ phase: "drop", items, move_items: move }).catch(() => undefined);
    } else {
      const hit = targetAt(model, elementAt(event.clientX, event.clientY));
      if (hit !== null && !isOwnRow(hit.element, dragged)) {
        void dropObjects(model, dragged, hit.target, move);
      }
    }
    finish();
  }

  function onCancel(): void {
    finish();
  }

  function onKey(event: KeyboardEvent): void {
    if (event.key === "Escape" && press !== null) {
      event.preventDefault();
      event.stopPropagation();
      finish();
    }
  }

  const onDown = (event: PointerEvent) => {
    if (event.button !== 0 || press !== null || !(event.target instanceof Element)) {
      return;
    }
    const row = event.target.closest(".bb-grid .bb-row[data-key]");
    if (!(row instanceof HTMLElement) || row.dataset.key === undefined) {
      return;
    }
    press = { x: event.clientX, y: event.clientY, key: row.dataset.key, pointerId: event.pointerId };
    window.addEventListener("pointermove", onMove, true);
    window.addEventListener("pointerup", onUp, true);
    window.addEventListener("pointercancel", onCancel, true);
    window.addEventListener("keydown", onKey, true);
  };

  document.addEventListener("pointerdown", onDown);

  // A drag that comes from another window of the app, and files from the file manager.
  const incoming = new IncomingDrag(model, elementAt, feedback);
  const stopIncoming = model.raw.listen("drag://incoming", (payload) => incoming.handle(payload));
  const stopFiles = model.platform.onFileDrop((event) => {
    if (event.phase === "leave") {
      feedback.clear();
      return;
    }
    const hit = targetAt(model, elementAt(event.x, event.y));
    if (event.phase === "over") {
      feedback.highlight(hit?.element ?? null);
      if (hit === null) {
        feedback.hideLabel();
      } else {
        feedback.show(event.x, event.y, tr("Drop to upload to {where}", { where: placeLabel(hit.target) }), false);
      }
      return;
    }
    feedback.clear();
    if (event.paths.length === 0) {
      return;
    }
    if (hit === null) {
      model.status.info(tr("Open a bucket and drop the files on its list to upload them."));
      return;
    }
    void uploadPaths(model, event.paths, { bucket: hit.target.bucket, prefix: hit.target.prefix });
  });

  return () => {
    document.removeEventListener("pointerdown", onDown);
    stopListening();
    stopIncoming();
    stopFiles();
    feedback.clear();
  };
}

/** The receiving side of a drag that started in another window. */
class IncomingDrag {
  private readonly model: AppModel;
  private readonly elementAt: ElementAt;
  private readonly feedback: Feedback;

  constructor(model: AppModel, elementAt: ElementAt, feedback: Feedback) {
    this.model = model;
    this.elementAt = elementAt;
    this.feedback = feedback;
  }

  handle(incoming: DragIncoming): void {
    if (incoming.phase === "leave" || incoming.items === null) {
      this.feedback.clear();
      return;
    }
    const source = toSource(incoming.items);
    const hit = targetAt(this.model, this.elementAt(incoming.x, incoming.y));
    if (incoming.phase === "over") {
      const shown = hit === null ? null : feedbackText(source, hit.target, incoming.move);
      if (hit === null || shown === null) {
        this.feedback.clear();
        return;
      }
      this.feedback.highlight(shown.blocked ? null : hit.element);
      this.feedback.show(incoming.x, incoming.y, shown.text, shown.blocked);
      return;
    }
    this.feedback.clear();
    if (hit !== null) {
      void dropObjects(this.model, source, hit.target, incoming.move);
    }
  }
}
