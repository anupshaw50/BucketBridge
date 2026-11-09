import { describe, expect, it } from "vitest";
import { tr } from "../i18n";
import { dropPositionToCss, isMoveDrag, moveModifier, planDrop } from "./dnd";

const source = { accountId: "a", bucket: "b", keys: ["x/one.txt", "x/two.txt"] };

describe("planDrop", () => {
  it("copies by default and moves when asked", () => {
    const target = { accountId: "a", bucket: "b", prefix: "y/" };
    expect(planDrop(source, target, false)).toMatchObject({ kind: "transfer", mode: "copy" });
    const plan = planDrop(source, target, true);
    expect(plan).toMatchObject({ kind: "transfer", mode: "move" });
    if (plan.kind === "transfer") {
      expect(plan.args).toEqual({ account_id: "a", src_bucket: "b", keys: source.keys, dst_bucket: "b", dst_prefix: "y/" });
    }
  });

  it("does nothing, silently, for the folder the items are already in", () => {
    expect(planDrop(source, { accountId: "a", bucket: "b", prefix: "x/" }, false)).toEqual({ kind: "noop" });
    expect(planDrop(source, { accountId: "a", bucket: "b", prefix: "x/" }, true)).toEqual({ kind: "noop" });
  });

  it("blocks a folder dropped into itself or a subfolder", () => {
    const folder = { accountId: "a", bucket: "b", keys: ["x/"] };
    const plan = planDrop(folder, { accountId: "a", bucket: "b", prefix: "x/deep/" }, false);
    expect(plan).toEqual({ kind: "blocked", message: tr("A folder cannot be pasted into itself or one of its subfolders.") });
  });

  it("blocks two items that would get the same name", () => {
    const clash = { accountId: "a", bucket: "b", keys: ["x/same.txt", "z/same.txt"] };
    expect(planDrop(clash, { accountId: "a", bucket: "b", prefix: "y/" }, false).kind).toBe("blocked");
  });

  it("names the destination account when it differs", () => {
    const plan = planDrop(source, { accountId: "other", bucket: "b", prefix: "x/" }, false);
    expect(plan.kind).toBe("transfer");
    if (plan.kind === "transfer") {
      expect(plan.args.account_id).toBe("a");
      expect(plan.args.dst_account_id).toBe("other");
    }
  });

  it("is a no-op for an empty drag", () => {
    expect(planDrop({ ...source, keys: [] }, { accountId: "a", bucket: "b", prefix: "y/" }, false)).toEqual({ kind: "noop" });
  });
});

describe("move modifier", () => {
  it("is Command on macOS and Shift elsewhere", () => {
    expect(moveModifier("Mozilla/5.0 (Macintosh)")).toBe("meta");
    expect(moveModifier("Mozilla/5.0 (Windows NT 10.0)")).toBe("shift");
    expect(isMoveDrag({ metaKey: true, shiftKey: false }, "Macintosh")).toBe(true);
    expect(isMoveDrag({ metaKey: false, shiftKey: true }, "Macintosh")).toBe(false);
    expect(isMoveDrag({ metaKey: false, shiftKey: true }, "Linux")).toBe(true);
  });
});

describe("dropPositionToCss", () => {
  const mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
  const windows = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/120.0";
  const linux = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15";

  it("keeps the logical points of macOS and Linux on a high-density screen", () => {
    expect(dropPositionToCss({ x: 800, y: 300 }, 2, mac)).toEqual({ x: 800, y: 300 });
    expect(dropPositionToCss({ x: 800, y: 300 }, 2, linux)).toEqual({ x: 800, y: 300 });
  });

  it("turns the device pixels of Windows into CSS pixels", () => {
    expect(dropPositionToCss({ x: 1600, y: 600 }, 2, windows)).toEqual({ x: 800, y: 300 });
    expect(dropPositionToCss({ x: 900, y: 300 }, 1.5, windows)).toEqual({ x: 600, y: 200 });
    expect(dropPositionToCss({ x: 800, y: 300 }, 0, windows)).toEqual({ x: 800, y: 300 });
  });
});
