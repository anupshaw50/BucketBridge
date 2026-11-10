import { describe, expect, it } from "vitest";
import type { Account, AccountFolder } from "../api/types";
import {
  addFolder,
  folderNameTaken,
  folderOf,
  groupAccounts,
  moveAccount,
  moveFolder,
  orderedAccounts,
  removeFolder,
  renameFolder,
  setAccountFolder,
} from "./accountGroups";

function account(id: string): Account {
  return {
    id,
    label: id.toUpperCase(),
    access_key_id: "AKIA",
    secret_ref: `${id}/secret/1`,
    session_token_ref: null,
    region: "us-east-1",
    endpoint: null,
    path_style: false,
    request_payer: false,
    options: { accelerate: false, dual_stack: false, compat: { storage_classes: [], regions: [], single_deletes: false, no_multipart_copy: false, list_all_regions: false }, cannot_list_buckets: false, known_buckets: [], proxy: { type: "global" } },
    mfa_serial: null,
    source: { type: "static_keys" },
  };
}

const accounts = ["a", "b", "c", "d"].map(account);
const folders: AccountFolder[] = [
  { name: "Work", account_ids: ["c", "a"] },
  { name: "Empty", account_ids: [] },
];

describe("account groups", () => {
  it("lists folders first, then the accounts in no folder, and ignores unknown or doubled ids", () => {
    const groups = groupAccounts(accounts, [...folders, { name: "Ghosts", account_ids: ["zzz", "a"] }]);
    expect(groups.map((group) => [group.folder, group.accounts.map((item) => item.id)])).toEqual([
      ["Work", ["c", "a"]],
      ["Empty", []],
      ["Ghosts", []],
      [null, ["b", "d"]],
    ]);
    expect(orderedAccounts(accounts, folders).map((item) => item.id)).toEqual(["c", "a", "b", "d"]);
    expect(groupAccounts(accounts, []).map((group) => group.folder)).toEqual([null]);
    expect(folderOf(folders, "a")).toBe("Work");
    expect(folderOf(folders, "b")).toBeNull();
  });

  it("moves an account inside its folder or among the accounts in no folder, and stops at the ends", () => {
    expect(moveAccount(accounts, folders, "a", "up").folders[0]?.account_ids).toEqual(["a", "c"]);
    expect(moveAccount(accounts, folders, "c", "up").folders[0]?.account_ids).toEqual(["c", "a"]);
    expect(moveAccount(accounts, folders, "a", "down").folders[0]?.account_ids).toEqual(["c", "a"]);
    const loose = moveAccount(accounts, folders, "d", "up");
    expect(loose.accounts).toEqual(["a", "d", "c", "b"]);
    expect(moveAccount(accounts, folders, "b", "up").accounts).toEqual(["a", "b", "c", "d"]);
    // The input is never changed.
    expect(folders[0]?.account_ids).toEqual(["c", "a"]);
  });

  it("moves folders, and creates, renames and removes them", () => {
    expect(moveFolder(accounts, folders, "Empty", "up").folders.map((folder) => folder.name)).toEqual(["Empty", "Work"]);
    expect(moveFolder(accounts, folders, "Work", "up").folders.map((folder) => folder.name)).toEqual(["Work", "Empty"]);
    expect(addFolder(accounts, folders, "New").folders.map((folder) => folder.name)).toEqual(["Work", "Empty", "New"]);
    const renamed = renameFolder(accounts, folders, "Work", "Job");
    expect(renamed.folders[0]).toEqual({ name: "Job", account_ids: ["c", "a"] });
    const removed = removeFolder(accounts, folders, "Work");
    expect(removed.folders.map((folder) => folder.name)).toEqual(["Empty"]);
    expect(removed.accounts).toEqual(["a", "b", "c", "d"]);
  });

  it("puts an account in a folder, a new folder, or none", () => {
    expect(setAccountFolder(accounts, folders, "b", "Work").folders[0]?.account_ids).toEqual(["c", "a", "b"]);
    const created = setAccountFolder(accounts, folders, "b", "Fresh");
    expect(created.folders.at(-1)).toEqual({ name: "Fresh", account_ids: ["b"] });
    const out = setAccountFolder(accounts, folders, "a", null);
    expect(out.folders[0]?.account_ids).toEqual(["c"]);
    const moved = setAccountFolder(accounts, folders, "a", "Empty");
    expect(moved.folders.map((folder) => folder.account_ids)).toEqual([["c"], ["a"]]);
  });

  it("compares folder names without case and lets a rename keep its own name", () => {
    expect(folderNameTaken(folders, " work ", null)).toBe(true);
    expect(folderNameTaken(folders, "Work", "Work")).toBe(false);
    expect(folderNameTaken(folders, "Other", null)).toBe(false);
  });
});
