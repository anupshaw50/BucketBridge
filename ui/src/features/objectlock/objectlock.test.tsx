import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MockBackend } from "../../mock/mockBackend";
import { LOCKED_MESSAGE } from "../../mock/objectLock";
import { AVAILABLE } from "../menus/available";
import { restoreText } from "./restoreText";

const base = { account_id: "acc", bucket: "b" };

describe("object lock mock", () => {
  it("keeps governance and legal hold locks until bypassed, with the server words", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0 });
    const future = "2099-01-01T00:00:00Z";
    const set = await mock.set_object_retention({
      ...base,
      keys: ["k"],
      change: { action: "set", mode: "governance", retain_until: future },
      version_id: null,
      bypass_governance: false,
    });
    expect(set[0]?.changed).toBe(true);
    const refused = await mock.set_object_retention({
      ...base,
      keys: ["k"],
      change: { action: "clear" },
      version_id: null,
      bypass_governance: false,
    });
    expect(refused[0]?.error?.message).toContain(LOCKED_MESSAGE);
    const bypassed = await mock.set_object_retention({
      ...base,
      keys: ["k"],
      change: { action: "clear" },
      version_id: null,
      bypass_governance: true,
    });
    expect(bypassed[0]?.changed).toBe(true);
    expect((await mock.get_object_lock({ ...base, key: "k", version_id: null })).mode).toBeNull();
    const hold = await mock.set_object_legal_hold({ ...base, keys: ["k"], on: true, version_id: null });
    expect(hold[0]?.changed).toBe(true);
    const again = await mock.set_object_legal_hold({ ...base, keys: ["k"], on: true, version_id: null });
    expect(again[0]?.changed).toBe(false);
  });

  it("stores the bucket lock, ownership and restore requests", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0 });
    expect((await mock.get_bucket_object_lock(base)).enabled).toBe(false);
    await mock.put_bucket_object_lock({ ...base, default_retention: { mode: "compliance", days: null, years: 2 } });
    expect((await mock.get_bucket_object_lock(base)).default_retention?.years).toBe(2);
    await expect(
      mock.put_bucket_object_lock({ ...base, default_retention: { mode: "compliance", days: 1, years: 1 } }),
    ).rejects.toMatchObject({ code: "usage" });
    expect(await mock.get_object_ownership(base)).toBeNull();
    await mock.put_object_ownership({ ...base, ownership: "BucketOwnerPreferred" });
    expect(await mock.get_object_ownership(base)).toBe("BucketOwnerPreferred");
    const first = await mock.restore_objects({ ...base, keys: ["c"], days: 3, tier: "bulk", version_id: null });
    expect(first[0]?.changed).toBe(true);
    const second = await mock.restore_objects({ ...base, keys: ["c"], days: 3, tier: "bulk", version_id: null });
    expect(second[0]?.error?.code).toBe("conflict");
    expect(restoreText(await mock.get_restore_state({ ...base, key: "c", version_id: null }))).toBe("A restore is in progress.");
  });

  it("turns the four menu entries on", () => {
    expect([AVAILABLE["bucket.ownership"], AVAILABLE["bucket.object-lock"], AVAILABLE["file.lock"], AVAILABLE["file.restore"]]).toEqual([
      true,
      true,
      true,
      true,
    ]);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(document.body)).toBeDefined();
  });
});
