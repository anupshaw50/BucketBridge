import { describe, expect, it } from "vitest";
import type { TransferTask } from "../api/types";
import { overallProgress } from "./transfers";

function task(state: TransferTask["state"], done: number, total: number | null): TransferTask {
  return { state, bytes_done: done, bytes_total: total } as TransferTask;
}

describe("overallProgress", () => {
  it("is null when nothing is queued or running", () => {
    expect(overallProgress([task("completed", 10, 10), task("failed", 0, 5), task("stopped", 1, 4)])).toBeNull();
  });

  it("adds up the bytes of queued and running transfers only", () => {
    expect(overallProgress([task("running", 25, 100), task("queued", 0, 100), task("completed", 50, 50)])).toBe(12.5);
  });

  it("is 0 while the active transfers have no known size", () => {
    expect(overallProgress([task("running", 3, null)])).toBe(0);
  });
});
