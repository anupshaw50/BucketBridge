// Account folders and their order, as the account manager and the account switcher show them. The pure
// functions here build the layout that `set_account_layout` stores (docs/desktop-api.md, "Account
// folders, order, export and import").

import type { Account, AccountFolder, AccountLayout } from "../api/types";

export interface AccountGroup {
  /** The folder name, or `null` for the accounts that are in no folder. */
  folder: string | null;
  accounts: Account[];
}

/** Folders first, in their order, each with its accounts; then the accounts in no folder (when there are any). */
export function groupAccounts(accounts: readonly Account[], folders: readonly AccountFolder[]): AccountGroup[] {
  const byId = new Map(accounts.map((account) => [account.id, account]));
  const placed = new Set<string>();
  const groups: AccountGroup[] = folders.map((folder) => ({
    folder: folder.name,
    accounts: folder.account_ids.flatMap((id) => {
      const account = byId.get(id);
      if (account === undefined || placed.has(id)) {
        return [];
      }
      placed.add(id);
      return [account];
    }),
  }));
  const loose = accounts.filter((account) => !placed.has(account.id));
  if (loose.length > 0) {
    groups.push({ folder: null, accounts: loose });
  }
  return groups;
}

/** The accounts in the order the account manager lists them. */
export function orderedAccounts(accounts: readonly Account[], folders: readonly AccountFolder[]): Account[] {
  return groupAccounts(accounts, folders).flatMap((group) => group.accounts);
}

/** The folder an account is in, if any. */
export function folderOf(folders: readonly AccountFolder[], id: string): string | null {
  return folders.find((folder) => folder.account_ids.includes(id))?.name ?? null;
}

function cloneLayout(accounts: readonly Account[], folders: readonly AccountFolder[]): AccountLayout {
  return {
    folders: folders.map((folder) => ({ name: folder.name, account_ids: [...folder.account_ids] })),
    accounts: accounts.map((account) => account.id),
  };
}

function swap<T>(list: T[], a: number, b: number): void {
  const first = list[a];
  const second = list[b];
  if (first !== undefined && second !== undefined) {
    list[a] = second;
    list[b] = first;
  }
}

/** Moves an account one place up or down inside its folder, or among the accounts in no folder. */
export function moveAccount(
  accounts: readonly Account[],
  folders: readonly AccountFolder[],
  id: string,
  direction: "up" | "down",
): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  const step = direction === "up" ? -1 : 1;
  const folder = layout.folders.find((item) => item.account_ids.includes(id));
  if (folder !== undefined) {
    const at = folder.account_ids.indexOf(id);
    swap(folder.account_ids, at, at + step);
    return layout;
  }
  const members = new Set(layout.folders.flatMap((item) => item.account_ids));
  const loose = layout.accounts.filter((accountId) => !members.has(accountId));
  const neighbour = loose[loose.indexOf(id) + step];
  if (neighbour !== undefined) {
    swap(layout.accounts, layout.accounts.indexOf(id), layout.accounts.indexOf(neighbour));
  }
  return layout;
}

/** Moves a folder one place up or down among the folders. */
export function moveFolder(
  accounts: readonly Account[],
  folders: readonly AccountFolder[],
  name: string,
  direction: "up" | "down",
): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  const at = layout.folders.findIndex((folder) => folder.name === name);
  swap(layout.folders, at, at + (direction === "up" ? -1 : 1));
  return layout;
}

/** Puts an account in a folder (created when it is new at the end), or in none (`null`). */
export function setAccountFolder(
  accounts: readonly Account[],
  folders: readonly AccountFolder[],
  id: string,
  folder: string | null,
): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  for (const item of layout.folders) {
    item.account_ids = item.account_ids.filter((member) => member !== id);
  }
  if (folder !== null) {
    let target = layout.folders.find((item) => item.name === folder);
    if (target === undefined) {
      target = { name: folder, account_ids: [] };
      layout.folders.push(target);
    }
    target.account_ids.push(id);
  }
  return layout;
}

/** Adds an empty folder. */
export function addFolder(accounts: readonly Account[], folders: readonly AccountFolder[], name: string): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  layout.folders.push({ name, account_ids: [] });
  return layout;
}

/** Renames a folder; its accounts stay in it. */
export function renameFolder(
  accounts: readonly Account[],
  folders: readonly AccountFolder[],
  name: string,
  newName: string,
): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  const folder = layout.folders.find((item) => item.name === name);
  if (folder !== undefined) {
    folder.name = newName;
  }
  return layout;
}

/** Removes a folder; its accounts are kept and are in no folder any more. */
export function removeFolder(accounts: readonly Account[], folders: readonly AccountFolder[], name: string): AccountLayout {
  const layout = cloneLayout(accounts, folders);
  layout.folders = layout.folders.filter((folder) => folder.name !== name);
  return layout;
}

/** Whether a folder name is taken (case-insensitively), ignoring the folder being renamed. */
export function folderNameTaken(folders: readonly AccountFolder[], name: string, except: string | null): boolean {
  const wanted = name.trim().toLowerCase();
  return folders.some((folder) => folder.name !== except && folder.name.toLowerCase() === wanted);
}
