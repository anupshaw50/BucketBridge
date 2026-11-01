import { beforeEach, describe, expect, it, vi } from "vitest";
import { BackendError } from "./errors";
import { createTauriPlatform } from "./tauriPlatform";

const writeText = vi.fn<(text: string) => Promise<void>>();
const writeSecret = vi.fn<(text: string) => Promise<void>>();

vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: (text: string) => writeText(text),
  writeSecret: (text: string) => writeSecret(text),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), save: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(), revealItemInDir: vi.fn() }));

describe("tauri platform clipboard", () => {
  beforeEach(() => {
    writeText.mockReset().mockResolvedValue(undefined);
    writeSecret.mockReset().mockResolvedValue(undefined);
  });

  it("writes sensitive values with writeSecret and others with writeText", async () => {
    const platform = createTauriPlatform();
    await platform.copyText("https://example.com/presigned", { sensitive: true });
    await platform.copyText("s3://bucket/key");
    expect(writeSecret).toHaveBeenCalledWith("https://example.com/presigned");
    expect(writeText).toHaveBeenCalledWith("s3://bucket/key");
    expect(writeText).toHaveBeenCalledTimes(1);
  });

  it("reports a failed writeSecret instead of falling back to a plain copy", async () => {
    writeSecret.mockRejectedValueOnce("clipboard-manager.write_secret not allowed");
    const platform = createTauriPlatform();
    await expect(platform.copyText("generated-password", { sensitive: true })).rejects.toBeInstanceOf(BackendError);
    expect(writeText).not.toHaveBeenCalled();
  });
});
