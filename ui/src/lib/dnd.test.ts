import { describe, expect, it } from "vitest";
import { tr } from "../i18n";
import { isMoveDrag, moveModifier, planDrop } from "./dnd";

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
