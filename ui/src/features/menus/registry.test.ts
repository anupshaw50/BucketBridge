import { afterEach, describe, expect, it, vi } from "vitest";
import type { Account, Bucket } from "../../api/types";
import { en, type Messages } from "../../i18n/en";
import { currentMessages, t, tr } from "../../i18n";
import { zhMenu } from "../../i18n/zh/menu";
import { formatBinding, isMacPlatform, matchesBinding } from "../../lib/keys";
import { MockBackend, type MockOptions } from "../../mock/mockBackend";
import { AppModel } from "../../state/model";
import { immediateScheduler } from "../../state/scheduler";
import { countCommands } from "../../test/helpers";
import { TestPlatform } from "../../test/renderApp";
import { EMPTY_SELECTION } from "../../lib/selection";
import { mountSupported } from "../mount/support";
import { SEARCH_INPUT_ID } from "../objects/actions";
import { AVAILABLE, isAvailable } from "./available";
import {
  accountContext,
  bucketContext,
  bucketsEmptyContext,
  capsFor,
  commonCaps,
  filesEmptyContext,
  itemsContext,
  type MenuContext,
} from "./context";
import { type ActionId, LIVE_ACTION_IDS, type MenuId, MENU_IDS, PLANNED_ACTION_IDS } from "./ids";
import { actionForKey, bindingFor, KEYMAP } from "./keymap";
import { LAYOUTS } from "./layouts";
import { ACTIONS, bucketUrl } from "./registry";
import { layoutActionIds, type ResolvedItem, resolveMenu } from "./resolve";
import { folderEntry } from "../../lib/objects";

const live: { stop: () => void; mock: MockBackend }[] = [];

afterEach(() => {
  while (live.length > 0) {
    const item = live.pop();
    item?.stop();
    item?.mock.dispose();
  }
});

/** An application model on the mock backend with the demo account loaded and no UI. */
async function loadedModel(extra: MockOptions = {}) {
  const mock = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 5, manyFiles: 0, ...extra });
  const platform = new TestPlatform();
  const model = new AppModel({ backend: mock, platform, scheduler: immediateScheduler });
  live.push({ stop: model.start(), mock });
  await vi.waitFor(() => {
    expect(model.app.getState().ready).toBe(true);
    expect(model.browser.state.bucketList.status).toBe("ready");
  });
  return { model, mock, platform };
}

function bucketNamed(model: AppModel, name: string): Bucket {
  const found = model.browser.state.bucketList.buckets.find((bucket) => bucket.name === name);
  if (found === undefined) {
    throw new Error(`no bucket ${name}`);
  }
  return found;
}

/** Entries as "Label (Shortcut)", "Label [off]" when disabled and "-" for separators. */
function shape(items: ResolvedItem[]): string[] {
  return items.map((item) => {
    if (item.type === "separator") {
      return "-";
    }
    if (item.type === "submenu") {
      return `${item.label} > ${shape(item.items).join(", ")}`;
    }
    return `${item.label}${item.shortcut === null ? "" : ` (${item.shortcut})`}${item.disabled ? " [off]" : ""}`;
  });
}

/** The "Copy as" submenu as `shape` shows it. */
function copyAsEntry(key: boolean, virtual = true): string {
  const names = [tr("Name"), ...(key ? [tr("Key")] : []), tr("S3 URI (s3://)"), ...(virtual ? [tr("Virtual-hosted URL")] : []), tr("Path-style URL")];
  return `${tr("Copy as")} > ${names.join(", ")}`;
}

/** The actions of a resolved menu, those inside submenus included. */
function actionsOf(items: ResolvedItem[]): Extract<ResolvedItem, { type: "action" }>[] {
  return items.flatMap((item) => {
    if (item.type === "action") {
      return [item];
    }
    return item.type === "submenu" ? actionsOf(item.items) : [];
  });
}

/** A submenu entry of the shape the resolver produces: "Label > Entry, Entry". */
function submenu(prefix: string, entries: string[]): string {
  return `${tr(prefix)} > ${entries.map((entry) => tr(entry)).join(", ")}`;
}

const EM_DASH = String.fromCharCode(0x2014);

describe("message bundle", () => {
  it("has a label for every action, menu and hint, in both languages, and no em dash anywhere", () => {
    for (const text of [...Object.values(en), ...Object.values(zhMenu)]) {
      expect(text.trim()).not.toBe("");
      expect(text).not.toContain(EM_DASH);
    }
    const bundle = currentMessages();
    for (const id of [...LIVE_ACTION_IDS, ...PLANNED_ACTION_IDS]) {
      expect(t(`action.${id}`)).toBe(bundle[`action.${id}`]);
    }
  });

  it("fails to compile when a key is missing or unknown", () => {
    // @ts-expect-error an unknown key is a type error, so a typo in a menu cannot reach the runtime
    expect(t("action.no-such-action")).toBeUndefined();
    // @ts-expect-error a bundle without every key is a type error, so a translation cannot miss a label
    const incomplete: Messages = { "menu.bucket": "Bucket menu" };
    expect(Object.keys(incomplete)).toHaveLength(1);
  });
});

/** Planned entries whose feature exists (bucket configuration, object metadata, versions, preview, tools, export). */
const BUILT: ReadonlySet<string> = new Set([
  "window.open-new",
  "bookmark.add",
  "bucket.mount",
  "cloudfront.toggle",
  "cloudfront.invalidate",
  "cloudfront.history",
  "cloudfront.attach",
  "cloudfront.edit",
  "cloudfront.detach",
  "tools.cloudfront",
  "file.encrypt",
  "file.decrypt",
  "list.filter",
  "bucket.add-external",
  "bucket.paste",
  "folder.copy-all",
  "folder.move-all",
  "file.copy-to",
  "file.move-to",
  "bucket.sharing-wizard",
  "bucket.versioning",
  "bucket.logging",
  "bucket.policy",
  "bucket.public-access",
  "bucket.ownership",
  "bucket.object-lock",
  "file.lock",
  "file.restore",
  "bucket.website",
  "bucket.website-routing",
  "bucket.acceleration",
  "bucket.cors",
  "bucket.lifecycle",
  "bucket.tiering",
  "bucket.replication",
  "file.preview",
  "file.tags",
  "file.storage-class",
  "file.sse",
  "file.http-headers",
  "file.acl",
  "file.export",
  "tools.iam",
  "tools.diagnostics",
  "tools.security-scan",
  "tools.multipart",
  "account.test",
  "bucket.open-browser",
  "file.open-with",
  "file.edit",
  "file.website-redirect",
]);

describe("action registry", () => {
  it("has a handler for every action in use", () => {
    for (const id of LIVE_ACTION_IDS) {
      expect(typeof ACTIONS[id]?.run, id).toBe("function");
    }
  });

  it("has no entry that is available without a handler (a flag turned on too early fails here)", () => {
    for (const id of [...LIVE_ACTION_IDS, ...PLANNED_ACTION_IDS]) {
      if (isAvailable(id)) {
        expect(typeof ACTIONS[id]?.run, `${id} is available but has no handler`).toBe("function");
      }
    }
  });

  it("flags every planned feature: built ones have a handler and are on, the rest stay hidden", () => {
    for (const id of PLANNED_ACTION_IDS) {
      expect(AVAILABLE[id], id).toBe(BUILT.has(id));
      expect(ACTIONS[id] !== undefined, id).toBe(BUILT.has(id));
    }
    expect(isAvailable("file.website-redirect")).toBe(true);
  });

  it("lays out each action at most once per menu, and only known actions", () => {
    const known = new Set<string>([...LIVE_ACTION_IDS, ...PLANNED_ACTION_IDS]);
    for (const menu of MENU_IDS) {
      const ids = layoutActionIds(LAYOUTS[menu]);
      expect(new Set(ids).size, menu).toBe(ids.length);
      for (const id of ids) {
        expect(known.has(id), `${menu}: ${id}`).toBe(true);
      }
    }
  });

  it("only offers bulk-capable actions in the selection menu", () => {
    for (const id of layoutActionIds(LAYOUTS.selection)) {
      const def = ACTIONS[id];
      if (def !== undefined) {
        expect(def.many, id).toBe(true);
      }
    }
  });

  it("gives every shortcut to an action in use, once per pane and shortcut", () => {
    const seen = new Set<string>();
    for (const binding of KEYMAP) {
      expect(ACTIONS[binding.id], binding.id).toBeDefined();
      const key = `${binding.pane}:${formatBinding(binding)}:${binding.id}`;
      expect(seen.has(key), key).toBe(false);
      seen.add(key);
    }
    expect(actionForKey({ key: "c", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }, "files")).toBe(
      "clipboard.copy",
    );
    expect(actionForKey({ key: "c", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false }, "files")).toBeNull();
    expect(actionForKey({ key: "F5", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false }, "buckets")).toBe(
      "bucket.refresh",
    );
    expect(bindingFor("item.delete", "files")?.key).toBe("Delete");
    expect(bindingFor("item.delete", "accounts")).toBeNull();
  });

  it("matches modifier keys exactly and formats shortcuts for the platform", () => {
    const copy = { key: "c", mod: true };
    expect(matchesBinding({ key: "C", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }, copy)).toBe(true);
    expect(matchesBinding({ key: "c", ctrlKey: false, metaKey: true, altKey: false, shiftKey: false }, copy)).toBe(true);
    expect(matchesBinding({ key: "c", ctrlKey: true, metaKey: false, altKey: true, shiftKey: false }, copy)).toBe(false);
    expect(matchesBinding({ key: "Enter", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }, { key: "Enter" })).toBe(
      false,
    );
    expect(formatBinding(copy)).toBe(isMacPlatform() ? "Cmd+C" : "Ctrl+C");
    expect(formatBinding({ key: "Delete" })).toBe("Del");
    vi.spyOn(window.navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    expect(formatBinding(copy)).toBe("Cmd+C");
  });
});

describe("resolved menus", () => {
  it("shows the live entries of the bucket menu for an AWS bucket and for an external one", async () => {
    const { model, mock } = await loadedModel();
    const expected = [
      `${tr("Open")} (Enter)`,
      tr("Open in new window"),
      "-",
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      tr("Download all files…"),
      tr("Paste") + " [off]",
      "-",
      submenu("Properties", [
        "Versioning…",
        "Server access logging…",
        "Bucket policy…",
        "Public access block…",
        "CORS…",
        "Lifecycle rules…",
        "Intelligent-Tiering…",
        "Replication…",
        "Static website…",
        "Website routing rules…",
        "Transfer acceleration…",
        "Object ownership…",
        "Object lock…",
      ]),
      tr("Sharing wizard…"),
      tr("Folder sync…"),
      tr("Sync jobs…"),
      "-",
      submenu("CloudFront", [
        "Attach distribution…",
        "Edit distribution…",
        "Enable or disable",
        "Create invalidation…",
        "Invalidation history…",
        "Detach distribution",
      ]),
      tr("Copy bucket name"),
      tr("Copy https:// URL"),
      copyAsEntry(false),
      tr("Open in browser"),
      tr("Bookmark…"),
      "-",
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ];
    const aws = resolveMenu("bucket", bucketContext(model, [bucketNamed(model, "demo-photos")]));
    expect(shape(aws)).toEqual(expected);
    // The external account has a custom endpoint: the entries the endpoint cannot do (versioning, website,
    // acceleration) are hidden, and the rest of the menu keeps its shape.
    const accounts = (await mock.list_accounts()).accounts;
    const external = accounts.find((account) => account.endpoint !== null);
    expect(external).toBeDefined();
    const archive: Bucket = { name: "archive-2025", region: "eu-central-1", creation_date: null, owner: null, kind: "external" };
    const ctx = { ...bucketContext(model, [archive]), account: external ?? null };
    expect(ctx.caps.external_endpoint).toBe(true);
    expect(shape(resolveMenu("bucket", ctx))).toEqual(
      expected
        .map((entry) =>
          entry.startsWith(`${tr("Properties")} >`)
            ? submenu("Properties", [
                "Server access logging…",
                "Bucket policy…",
                "Public access block…",
                "CORS…",
                "Lifecycle rules…",
                "Intelligent-Tiering…",
                "Replication…",
                "Object ownership…",
                "Object lock…",
              ])
            : entry,
        )
        .map((entry) => (entry.startsWith(`${tr("Copy as")} >`) ? copyAsEntry(false, false) : entry))
        .filter((entry) => !entry.startsWith(`${tr("CloudFront")} >`)),
    );
  });

  it("shows only the entries that apply to every bucket when several are selected", async () => {
    const { model } = await loadedModel();
    const both = [bucketNamed(model, "demo-photos"), bucketNamed(model, "demo-backups")];
    const items = resolveMenu("bucket", bucketContext(model, both));
    expect(shape(items)).toEqual([
      tr("Copy bucket name"),
      tr("Copy https:// URL"),
      copyAsEntry(false),
      "-",
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
  });

  it("shows folder, file, selection and empty-space menus with their state-driven entries disabled", async () => {
    const { model } = await loadedModel();
    await model.browser.openLocation("demo-photos", "");
    await vi.waitFor(() => expect(model.browser.state.listing.status).toBe("ready"));
    const entries = model.browser.state.visible;
    const folder = entries.find((entry) => entry.key === "photos/");
    const file = entries.find((entry) => entry.key === "readme.md");
    expect(folder && file).toBeTruthy();
    if (folder === undefined || file === undefined) {
      return;
    }
    expect(shape(resolveMenu("folder", itemsContext(model, "demo-photos", [folder])))).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open in new window"),
      "-",
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      tr("Download…"),
      "-",
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("Copy all files…"),
      tr("Move all files…"),
      "-",
      `${tr("Rename…")} (F2)`,
      tr("Folder sync…"),
      tr("Sync jobs…"),
      "-",
      tr("Properties"),
      tr("Versions…"),
      tr("Search here"),
      tr("Filter…"),
      tr("Copy path"),
      copyAsEntry(true),
      tr("Bookmark…"),
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    expect(shape(resolveMenu("file", itemsContext(model, "demo-photos", [file])))).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open with…"),
      tr("Edit"),
      `${tr("Preview")} (F3)`,
      tr("Versions…"),
      "-",
      tr("Download…"),
      "-",
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("Move to…"),
      tr("Copy to…"),
      `${tr("Rename…")} (F2)`,
      "-",
      submenu("Metadata", ["Tags…", "Storage class…", "Server-side encryption…", "HTTP headers…", "Website redirect…"]),
      tr("ACL…"),
      tr("Share…"),
      copyAsEntry(true),
      "-",
      tr("Encrypt…"),
      tr("Decrypt…"),
      tr("Lock"),
      tr("Restore from Glacier…"),
      "-",
      tr("Properties"),
      tr("Export…"),
      tr("Bookmark…"),
      "-",
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    expect(shape(resolveMenu("selection", itemsContext(model, "demo-photos", [folder, file])))).toEqual([
      tr("Download…"),
      "-",
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      "-",
      tr("Export…"),
      copyAsEntry(true),
      "-",
      `${tr("Select all")} (Ctrl+A)`,
      `${tr("Invert selection")} (Ctrl+I)`,
      "-",
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    // Several objects (no folder): the metadata editors that work on a batch join the selection menu.
    expect(
      shape(resolveMenu("selection", itemsContext(model, "demo-photos", [file, { ...file, key: "other.md" }]))),
    ).toContain(submenu("Metadata", ["Storage class…", "Tags…"]));
    expect(shape(resolveMenu("files-empty", filesEmptyContext(model)))).toEqual([
      `${tr("Paste")} (Ctrl+V) [off]`,
      "-",
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      "-",
      `${tr("Select all")} (Ctrl+A)`,
      `${tr("Invert selection")} (Ctrl+I)`,
      "-",
      `${tr("Refresh")} (F5)`,
      tr("Versions…"),
      tr("Filter…"),
      tr("Search"),
    ]);
    expect(shape(resolveMenu("buckets-empty", bucketsEmptyContext(model)))).toEqual([
      tr("New bucket…"),
      tr("Add external bucket…"),
      "-",
      `${tr("Refresh")} (F5)`,
      "-",
      tr("IAM manager…"),
      tr("CloudFront manager…"),
      tr("Diagnostics…"),
      tr("Security scan…"),
      tr("Uncompleted multipart uploads…"),
    ]);
  });

  it("shows a folder of the bucket tree with the same entries, acting on that folder and showing only shortcuts that work in the tree", async () => {
    const { model } = await loadedModel();
    const items = resolveMenu("folder", itemsContext(model, "demo-backups", [folderEntry("db/")], "buckets"));
    expect(shape(items)).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open in new window"),
      "-",
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      tr("Download…"),
      "-",
      tr("Copy"),
      tr("Cut"),
      tr("Paste") + " [off]",
      tr("Copy all files…"),
      tr("Move all files…"),
      "-",
      tr("Rename…"),
      tr("Folder sync…"),
      tr("Sync jobs…"),
      "-",
      tr("Properties"),
      tr("Versions…"),
      tr("Search here"),
      tr("Filter…"),
      tr("Copy path"),
      copyAsEntry(true),
      tr("Bookmark…"),
      tr("Delete…"),
    ]);
  });

  it("enables paste once the clipboard holds items that can go to the target", async () => {
    const { model } = await loadedModel();
    await model.browser.openLocation("demo-photos", "");
    await vi.waitFor(() => expect(model.browser.state.listing.status).toBe("ready"));
    const accountId = model.session.store.getState().activeAccountId ?? "";
    model.clipboard.setState({ contents: { accountId, bucket: "demo-photos", keys: ["readme.md"], mode: "copy" } });
    const folder = model.browser.state.visible.find((entry) => entry.key === "documents/");
    expect(folder).toBeDefined();
    const pasteInto = (ctx: MenuContext) => {
      const found = resolveMenu("folder", ctx).find((item) => item.type === "action" && item.id === "clipboard.paste");
      return found?.type === "action" ? found : null;
    };
    expect(pasteInto(itemsContext(model, "demo-photos", [folder ?? folderEntry("documents/")]))?.disabled).toBe(false);
    // A folder cannot be pasted into itself: the entry is disabled and the reason is its hint.
    model.clipboard.setState({ contents: { accountId, bucket: "demo-photos", keys: ["documents/"], mode: "cut" } });
    const into = pasteInto(itemsContext(model, "demo-photos", [folderEntry("documents/")]));
    expect(into?.disabled).toBe(true);
    expect(into?.hint).toBe(tr("A folder cannot be pasted into itself or one of its subfolders."));
  });

  it("has the account menu entries, with set-as-default disabled for the default account", async () => {
    const { model } = await loadedModel();
    const { accounts, defaultAccountId } = model.session.store.getState();
    const defaultAccount = accounts.find((account) => account.id === defaultAccountId)!;
    const other = accounts.find((account) => account.id !== defaultAccountId)!;
    expect(shape(resolveMenu("account", accountContext(model, defaultAccount)))).toEqual([
      tr("Edit account…"),
      tr("Set as default") + " [off]",
      "-",
      tr("Change master password…"),
      tr("MFA device…"),
      "-",
      tr("Test connection"),
      tr("Remove account…"),
    ]);
    const second = resolveMenu("account", accountContext(model, other));
    expect(shape(second)).toContain(tr("Set as default"));
  });

  it("never produces a separator at an edge or two in a row, and every entry has a handler and a label", async () => {
    const { model } = await loadedModel();
    await model.browser.openLocation("demo-photos", "");
    await vi.waitFor(() => expect(model.browser.state.listing.status).toBe("ready"));
    const folder = folderEntry("photos/");
    const file = model.browser.state.visible.find((entry) => entry.key === "readme.md");
    if (file === undefined) {
      throw new Error("readme.md is not listed");
    }
    const { accounts } = model.session.store.getState();
    const contexts: [(typeof MENU_IDS)[number], MenuContext][] = [
      ["bucket", bucketContext(model, [bucketNamed(model, "demo-backups")])],
      ["bucket", bucketContext(model, [bucketNamed(model, "demo-backups"), bucketNamed(model, "demo-logs")])],
      ["folder", itemsContext(model, "demo-photos", [folder])],
      ["folder", itemsContext(model, "demo-photos", [folder], "buckets")],
      ["file", itemsContext(model, "demo-photos", [file])],
      ["selection", itemsContext(model, "demo-photos", [folder, folderEntry("archive/")])],
      ["files-empty", filesEmptyContext(model)],
      ["buckets-empty", bucketsEmptyContext(model)],
      ...accounts.map((account): [(typeof MENU_IDS)[number], MenuContext] => ["account", accountContext(model, account)]),
    ];
    for (const [menu, ctx] of contexts) {
      const items = resolveMenu(menu, ctx);
      expect(items.length, menu).toBeGreaterThan(0);
      expect(items[0]?.type, menu).not.toBe("separator");
      expect(items[items.length - 1]?.type, menu).not.toBe("separator");
      items.forEach((item, index) => {
        if (item.type === "separator") {
          expect(items[index - 1]?.type, menu).not.toBe("separator");
        } else if (item.type === "action") {
          expect(typeof item.run, `${menu}: ${item.id}`).toBe("function");
          expect(item.label.length, item.id).toBeGreaterThan(0);
          expect(typeof ACTIONS[item.id]?.run, `${menu}: ${item.id}`).toBe("function");
        }
      });
    }
  });
});

describe("capabilities and availability", () => {
  const aws: Account = {
    id: "a",
    label: "aws",
    access_key_id: "k",
    secret_ref: "s",
    session_token_ref: null,
    region: "us-east-1",
    endpoint: null,
    path_style: false,
    request_payer: false,
    mfa_serial: null,
    source: { type: "static_keys" },
  };
  const external: Account = { ...aws, id: "e", endpoint: "https://storage.example.com", path_style: true };

  it("derives capabilities from loaded data: a custom endpoint is an external provider", () => {
    expect(capsFor(aws, null)).toMatchObject({ external_endpoint: false, versioning_supported: true, cloudfront_attached: false });
    expect(capsFor(external, null)).toMatchObject({ external_endpoint: true, versioning_supported: false, website_supported: false });
    expect(capsFor({ ...aws, request_payer: true }, null).requester_pays).toBe(true);
    const fronted: Bucket = { name: "w", region: null, creation_date: null, owner: null, kind: "cloudfront" };
    expect(capsFor(aws, fronted).cloudfront_attached).toBe(true);
    expect(commonCaps([capsFor(aws, fronted), capsFor(aws, null)]).cloudfront_attached).toBe(false);
  });

  it("hides an entry the endpoint cannot do and drops a submenu without visible entries", async () => {
    const { model } = await loadedModel();
    const archive: Bucket = { name: "archive-2025", region: null, creation_date: null, owner: null, kind: "external" };
    const awsCtx = bucketContext(model, [bucketNamed(model, "demo-photos")]);
    const externalCtx = { ...bucketContext(model, [archive]), caps: capsFor(null, archive) };
    const propertiesOf = (ctx: MenuContext) =>
      shape(resolveMenu("bucket", ctx)).find((entry) => entry.startsWith(`${tr("Properties")} >`)) ?? "";
    expect(propertiesOf(awsCtx)).toContain(tr("Versioning…"));
    expect(propertiesOf(awsCtx)).toContain(tr("Static website…"));
    expect(propertiesOf(awsCtx)).toContain(tr("Transfer acceleration…"));
    // A provider that cannot do versioning, websites or acceleration keeps the settings it can show.
    expect(propertiesOf(externalCtx)).toBe(
      submenu("Properties", [
        "Server access logging…",
        "Bucket policy…",
        "Public access block…",
        "CORS…",
        "Lifecycle rules…",
        "Intelligent-Tiering…",
        "Replication…",
        "Object ownership…",
        "Object lock…",
      ]),
    );
    // When every entry of a submenu is hidden, the submenu goes with them.
    const none = { defs: ACTIONS, available: (id: ActionId) => isAvailable(id) && !id.startsWith("bucket.") };
    const withoutBuckets = shape(resolveMenu("bucket", awsCtx, none));
    expect(withoutBuckets.some((entry) => entry.startsWith(tr("Properties")))).toBe(false);
  });

  it("keeps an available entry hidden until its handler exists", async () => {
    const { model } = await loadedModel();
    const ctx = bucketContext(model, [bucketNamed(model, "demo-photos")]);
    const items = resolveMenu("bucket", ctx, { defs: ACTIONS, available: () => true });
    expect(items.some((item) => item.type === "action" && item.id === "bucket.mount")).toBe(false);
  });
});

describe("bucket URLs", () => {
  it("uses virtual-hosted style on AWS, path style for a dotted name, and the account's style on custom endpoints", () => {
    expect(bucketUrl({ endpoint: null, path_style: false }, "demo-photos", "us-east-1")).toBe(
      "https://demo-photos.s3.us-east-1.amazonaws.com/",
    );
    expect(bucketUrl({ endpoint: null, path_style: false }, "my.bucket", "eu-west-1")).toBe(
      "https://s3.eu-west-1.amazonaws.com/my.bucket",
    );
    expect(bucketUrl({ endpoint: "https://s3.example.com/", path_style: true }, "demo", null)).toBe(
      "https://s3.example.com/demo",
    );
    expect(bucketUrl({ endpoint: "http://s3.example.com:9000", path_style: false }, "demo", null)).toBe(
      "http://demo.s3.example.com:9000/",
    );
  });
});

describe("every entry of every menu", () => {
  it("has a handler whose effect can be seen: a dialog, a queued task, a status message, a copy, a selection or a new place", async () => {
    const { model, mock, platform } = await loadedModel({ mountSupported: true });
    await vi.waitFor(() => expect(mountSupported(model)).toBe(true));
    const calls = countCommands(mock);
    // The filter box the real UI renders above the list, so that "Search" has something to focus.
    const filterBox = document.createElement("input");
    filterBox.id = SEARCH_INPUT_ID;
    document.body.append(filterBox);
    try {
      const accountId = model.session.store.getState().activeAccountId ?? "";
      const accounts = model.session.store.getState().accounts;
      // An external bucket pinned to the account, so that its entries (remove) show up in the bucket menu.
      await mock.add_external_bucket({ account_id: accountId, name: "partner-exchange" });
      await model.browser.loadBuckets(accountId);
      await model.browser.openLocation("demo-photos", "");
      const entry = (key: string) => {
        const found = model.browser.state.visible.find((candidate) => candidate.key === key);
        if (found === undefined) {
          throw new Error(`${key} is not listed`);
        }
        return found;
      };
      const scenarios: [MenuId, () => MenuContext][] = [
        ["bucket", () => bucketContext(model, [bucketNamed(model, "demo-backups")])],
        ["bucket", () => bucketContext(model, [bucketNamed(model, "demo-backups"), bucketNamed(model, "demo-logs")])],
        ["bucket", () => bucketContext(model, [bucketNamed(model, "partner-exchange")])],
        ["folder", () => itemsContext(model, "demo-photos", [entry("documents/")])],
        ["folder", () => itemsContext(model, "demo-photos", [folderEntry("photos/")], "buckets")],
        ["file", () => itemsContext(model, "demo-photos", [entry("readme.md")])],
        ["selection", () => itemsContext(model, "demo-photos", [entry("readme.md"), entry("documents/")])],
        [
          "selection",
          () =>
            itemsContext(model, "demo-photos", [
              { ...entry("readme.md"), key: "documents/notes.txt" },
              { ...entry("readme.md"), key: "documents/report-q3.pdf" },
            ]),
        ],
        ["files-empty", () => filesEmptyContext(model)],
        ["buckets-empty", () => bucketsEmptyContext(model)],
        ...accounts.map((account): [MenuId, () => MenuContext] => ["account", () => accountContext(model, account)]),
      ];
      const reset = async () => {
        model.dialogs.closeAll();
        model.status.clear();
        filterBox.blur();
        platform.fileAnswers.length = 0;
        platform.folderAnswers.length = 0;
        platform.fileAnswers.push(["/home/demo/Documents/report.pdf"]);
        platform.folderAnswers.push("/home/demo/Downloads");
        await model.browser.openLocation("demo-photos", "");
        model.browser.setSelection(EMPTY_SELECTION);
        model.clipboard.setState({ contents: { accountId, bucket: "demo-photos", keys: ["readme.md"], mode: "copy" } });
      };
      const snapshot = () =>
        JSON.stringify({
          calls: calls(),
          dialogs: model.dialogs.store.getState().open.length,
          message: model.status.store.getState().message?.id ?? 0,
          copied: platform.copied.length,
          clipboard: model.clipboard.getState().contents,
          selected: [...model.browser.state.selection.keys].sort(),
          where: [model.browser.state.listing.bucket, model.browser.state.listing.prefix],
          searching: document.activeElement === filterBox,
        });

      const walked = new Set<ActionId>();
      for (const [menu, makeContext] of scenarios) {
        await reset();
        const ids = actionsOf(resolveMenu(menu, makeContext())).flatMap((item) => (item.disabled ? [] : [item.id]));
        expect(ids.length, menu).toBeGreaterThan(0);
        for (const id of ids) {
          await reset();
          const item = actionsOf(resolveMenu(menu, makeContext())).find((candidate) => candidate.id === id);
          if (item === undefined) {
            throw new Error(`${menu}: ${id} vanished`);
          }
          const before = snapshot();
          item.run();
          await vi.waitFor(() => expect(snapshot(), `${menu}: ${id} had no effect`).not.toBe(before));
          expect(model.status.store.getState().message?.level, `${menu}: ${id} ended in an error`).not.toBe("error");
          walked.add(id);
        }
      }
      // Every action that is available (in use, or planned with its feature built) is an entry of at least
      // one menu, and was run above.
      const available = [...LIVE_ACTION_IDS, ...PLANNED_ACTION_IDS].filter((id) => isAvailable(id));
      expect([...walked].sort()).toEqual(available.sort());
    } finally {
      filterBox.remove();
    }
  });
});
