import { describe, expect, it } from "vitest";
import { passwordProblem } from "./problems";
import { BackendError } from "../../api/errors";
import { MockBackend } from "../../mock/mockBackend";

describe("client-side encryption (T07)", () => {
  it("tells the password states apart", () => {
    expect(passwordProblem("s3://b/k: password not found: x")).toMatch(/^Password not found/);
    expect(passwordProblem("password not valid: it does not open the data")).toMatch(/^Password not valid/);
    expect(passwordProblem("something else")).toBeNull();
  });

  it("encrypts and decrypts in the mock with the password states", async () => {
    const backend = new MockBackend();
    const base = { account_id: "a", bucket: "b", key: "k.txt" };
    await backend.cse_encrypt_object({ ...base, password: "pw", compression: "zstd" });
    expect((await backend.cse_probe_object(base)).compression).toBe("zstd");
    await expect(backend.cse_encrypt_object({ ...base, password: "pw", compression: null })).rejects.toMatchObject({
      code: "conflict",
    });
    await expect(backend.cse_decrypt_object({ ...base, password: "bad" })).rejects.toBeInstanceOf(BackendError);
    await expect(backend.cse_check_password({ ...base, password: null })).rejects.toMatchObject({
      message: expect.stringContaining("password not found") as string,
    });
    await expect(backend.cse_check_password({ ...base, password: "bad" })).rejects.toMatchObject({
      message: expect.stringContaining("password not valid") as string,
    });
    await backend.cse_decrypt_object({ ...base, password: "pw" });
    expect((await backend.cse_probe_object(base)).encrypted).toBe(false);
    await expect(backend.cse_probe_object({ ...base, key: "dir/" })).rejects.toMatchObject({ code: "usage" });
  });
});
