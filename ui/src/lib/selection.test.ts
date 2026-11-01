import { describe, expect, it } from "vitest";
import {
  EMPTY_SELECTION,
  invert,
  moveFocus,
  orderedSelection,
  prune,
  selectAll,
  selectOnly,
  selectRange,
  toggle,
} from "./selection";

const order = ["a", "b", "c", "d", "e"];
const keys = (selection: { keys: ReadonlySet<string> }) => [...selection.keys].sort();

describe("selection", () => {
  it("selects one, toggles and builds ranges from the anchor", () => {
    let selection = selectOnly("b");
    expect(keys(selection)).toEqual(["b"]);
    selection = toggle(selection, "d");
    expect(keys(selection)).toEqual(["b", "d"]);
    selection = toggle(selection, "b");
    expect(keys(selection)).toEqual(["d"]);
    selection = selectRange(selectOnly("b"), order, "d");
    expect(keys(selection)).toEqual(["b", "c", "d"]);
    selection = selectRange(selection, order, "a");
    expect(keys(selection)).toEqual(["a", "b"]);
    expect(selection.anchor).toBe("b");
  });

  it("adds ranges to the selection with ctrl/cmd held", () => {
    const base = toggle(selectOnly("a"), "e");
    const extended = selectRange({ ...base, anchor: "b" }, order, "c", true);
    expect(keys(extended)).toEqual(["a", "b", "c", "e"]);
  });

  it("uses the clicked key as anchor when the anchor is gone", () => {
    const selection = selectRange({ keys: new Set(), anchor: "zzz", focus: null }, order, "c");
    expect(keys(selection)).toEqual(["c"]);
  });

  it("selects all and inverts", () => {
    expect(keys(selectAll(order))).toEqual(order);
    expect(keys(invert(toggle(selectOnly("a"), "c"), order))).toEqual(["b", "d", "e"]);
    expect(keys(invert(selectAll(order), order))).toEqual([]);
    expect(selectAll([]).focus).toBeNull();
  });

  it("prunes keys that disappeared and keeps identity when nothing changed", () => {
    const selection = selectRange(selectOnly("b"), order, "d");
    expect(prune(selection, order)).toBe(selection);
    const pruned = prune(selection, ["a", "b", "e"]);
    expect(keys(pruned)).toEqual(["b"]);
    expect(pruned.focus).toBeNull();
  });

  it("moves the focus with arrows, clamps, and extends with shift", () => {
    expect(moveFocus(EMPTY_SELECTION, order, 1, false).focus).toBe("a");
    expect(moveFocus(EMPTY_SELECTION, order, -1, false).focus).toBe("e");
    const moved = moveFocus(selectOnly("b"), order, 1, false);
    expect(keys(moved)).toEqual(["c"]);
    expect(moveFocus(selectOnly("e"), order, 5, false).focus).toBe("e");
    expect(moveFocus(selectOnly("b"), order, Number.NEGATIVE_INFINITY, false).focus).toBe("a");
    const extended = moveFocus(moveFocus(selectOnly("b"), order, 1, true), order, 1, true);
    expect(keys(extended)).toEqual(["b", "c", "d"]);
    expect(moveFocus(selectOnly("a"), [], 1, false)).toBe(EMPTY_SELECTION);
  });

  it("starts before the first row for forward steps and after the last for backward ones", () => {
    expect(moveFocus(EMPTY_SELECTION, order, Number.NEGATIVE_INFINITY, false).focus).toBe("a");
    expect(moveFocus(EMPTY_SELECTION, order, Number.POSITIVE_INFINITY, false).focus).toBe("e");
    expect(moveFocus(EMPTY_SELECTION, order, 3, false).focus).toBe("c");
    expect(moveFocus(EMPTY_SELECTION, order, -2, false).focus).toBe("d");
  });

  it("returns selected keys in display order", () => {
    expect(orderedSelection(toggle(selectOnly("d"), "a"), order)).toEqual(["a", "d"]);
  });
});
