import { t } from "../../i18n";
import { isFolderEntry } from "../../lib/objects";
import { bucketUrl } from "../../lib/bucketUrl";
import { s3Uri } from "../../lib/s3keys";
import type { BucketPropertiesTab, BucketRulesTab, CloudFrontView, ObjectPropertiesTab } from "../../state/dialogs";
import type { AppModel } from "../../state/model";
import { makeDefault, removeAccount } from "../accounts/accountActions";
import { removeExternalBucket, showAddExternalBucket, showBucketProperties } from "../bucketcfg/actions";
import {
  copySelection,
  deleteSelected,
  downloadAll,
  downloadSelected,
  focusSearch,
  invertSelectedEntries,
  newFolder,
  openEntry,
  openLocation,
  paste,
  pasteBlockerFor,
  renameSelected,
  selectAllEntries,
  shareSelected,
  showProperties,
  type Subject,
  uploadFiles,
  uploadFolder,
} from "../objects/actions";
import { openInNewWindow, showAddBookmark } from "../bookmarks/actions";
import { mountSupported } from "../mount/support";
import { openExportDialog } from "../tools/actions";
import { chooseDestination, folderSubject } from "../picker/actions";
import { showCloudFront } from "../cloudfront/actions";
import { showEncryption } from "../cse/actions";
import { previewSelected, showVersions } from "../versions/actions";
import { isAvailable } from "./available";
import type { BucketCaps, MenuContext } from "./context";
import type { ActionId, LiveActionId, PlannedActionId } from "./ids";

/**
 * One action of the registry. The context menus, the toolbar and the keyboard
 * handlers all resolve actions through this table, so an action has one
 * implementation, one label (in the message bundle, keyed by its id) and one
 * shortcut (in keymap.ts).
 */
export interface ActionDef {
  /** Also works on several targets (the multi-selection rules). */
  many?: boolean;
  /** Destructive: its handler asks for the typed or counted confirmation before it does anything. */
  danger?: boolean;
  /** Capability gate: the entry is hidden when the endpoint cannot do this. */
  requires?: (caps: BucketCaps) => boolean;
  /** Hidden when false (the entry does not apply to this target). */
  visible?: (ctx: MenuContext) => boolean;
  /** Shown greyed out with this reason when the current state does not allow it. */
  disabled?: (ctx: MenuContext) => string | null;
  /** When a key triggers the action while it is disabled, the reason is reported under this title instead of being ignored. */
  blockedTitle?: string;
  run: (ctx: MenuContext) => void | Promise<void>;
}

function subjectOf(ctx: MenuContext): Subject | null {
  if (ctx.bucket === null || ctx.accountId === null || ctx.items.length === 0) {
    return null;
  }
  return { accountId: ctx.accountId, bucket: ctx.bucket, entries: ctx.items };
}

const hasItems = (ctx: MenuContext) => ctx.items.length > 0;
const isSingle = (ctx: MenuContext) => ctx.items.length === 1;
const isSingleFolder = (ctx: MenuContext) => {
  const [entry] = ctx.items;
  return ctx.items.length === 1 && entry !== undefined && isFolderEntry(entry);
};
const isSingleFile = (ctx: MenuContext) => isSingle(ctx) && !isSingleFolder(ctx);
/** One or more objects and no folder: what the metadata editors work on. */
const onlyFiles = (ctx: MenuContext) => hasItems(ctx) && ctx.items.every((entry) => !isFolderEntry(entry));
const isSingleBucket = (ctx: MenuContext) => ctx.buckets.length === 1;
const needsPlace = (ctx: MenuContext) => (ctx.place === null ? t("hint.no-bucket") : null);
const needsAccount = (ctx: MenuContext) => (ctx.accountId === null ? t("hint.no-account") : null);
const needsRows = (ctx: MenuContext) => (ctx.visibleCount === 0 ? t("hint.empty-list") : null);

/** Where Paste lands, as the paste check expects it. */
function pasteLocation(ctx: MenuContext) {
  return ctx.place === null || ctx.accountId === null ? null : { accountId: ctx.accountId, ...ctx.place };
}

/** Shows the folder a menu was opened on (when it is not the open one) before an action that works on the open folder. */
async function showFolderOf(ctx: MenuContext): Promise<void> {
  const [entry] = ctx.items;
  if (entry === undefined || ctx.bucket === null) {
    return;
  }
  const open = openLocation(ctx.model);
  if (open?.bucket !== ctx.bucket || open.prefix !== entry.key) {
    await ctx.model.browser.openLocation(ctx.bucket, entry.key);
  }
}

/** Whether the open listing proves that this bucket has no objects. */
function knownEmpty(model: AppModel, bucket: string): boolean {
  const { listing } = model.browser.state;
  return (
    listing.bucket === bucket &&
    listing.prefix === "" &&
    listing.status === "ready" &&
    listing.entries.length === 0 &&
    listing.nextToken === null
  );
}

export { bucketUrl };

async function copyToClipboard(model: AppModel, text: string, what: string): Promise<void> {
  try {
    await model.platform.copyText(text);
    model.status.info(`${what} copied.`);
  } catch (error) {
    model.status.error(error, { notify: "Copying failed" });
  }
}

const LIVE: Record<LiveActionId, ActionDef> = {
  // ---- files and folders ----
  open: {
    visible: isSingle,
    run: async (ctx) => {
      const [entry] = ctx.items;
      if (entry !== undefined && ctx.bucket !== null) {
        await openEntry(ctx.model, entry, ctx.bucket);
      }
    },
  },
  "item.download": {
    many: true,
    visible: hasItems,
    run: (ctx) => downloadSelected(ctx.model, true, subjectOf(ctx)),
  },
  "item.rename": {
    visible: isSingle,
    run: (ctx) => {
      renameSelected(ctx.model, subjectOf(ctx));
    },
  },
  "item.share": {
    visible: (ctx) => isSingle(ctx) && !isSingleFolder(ctx),
    run: (ctx) => {
      shareSelected(ctx.model, subjectOf(ctx));
    },
  },
  // One row opens its sheet; several selected objects open the editors that change them together.
  "item.properties": {
    many: true,
    visible: hasItems,
    run: (ctx) => {
      showProperties(ctx.model, subjectOf(ctx));
    },
  },
  "item.versions": {
    many: true,
    disabled: needsPlace,
    run: (ctx) => {
      showVersions(ctx.model, subjectOf(ctx), ctx.place ?? undefined);
    },
  },
  "item.delete": {
    many: true,
    danger: true,
    visible: hasItems,
    run: (ctx) => deleteSelected(ctx.model, subjectOf(ctx)),
  },
  "clipboard.copy": {
    many: true,
    visible: hasItems,
    run: (ctx) => {
      copySelection(ctx.model, "copy", subjectOf(ctx));
    },
  },
  "clipboard.cut": {
    many: true,
    visible: hasItems,
    run: (ctx) => {
      copySelection(ctx.model, "cut", subjectOf(ctx));
    },
  },
  "clipboard.paste": {
    many: true,
    blockedTitle: "Paste not started",
    disabled: (ctx) => pasteBlockerFor(ctx.clipboard, pasteLocation(ctx)),
    run: (ctx) => paste(ctx.model, ctx.place ?? undefined),
  },
  "folder.new": {
    disabled: needsPlace,
    run: (ctx) => {
      newFolder(ctx.model, ctx.place ?? undefined);
    },
  },
  "folder.search": {
    visible: isSingleFolder,
    run: async (ctx) => {
      await showFolderOf(ctx);
      focusSearch();
    },
  },
  "upload.files": {
    disabled: needsPlace,
    run: (ctx) => uploadFiles(ctx.model, ctx.place ?? undefined),
  },
  "upload.folder": {
    disabled: needsPlace,
    run: (ctx) => uploadFolder(ctx.model, ctx.place ?? undefined),
  },
  "sync.open": {
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "sync", at: ctx.place ?? undefined });
    },
  },
  "sync.jobs": {
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "sync-jobs" });
    },
  },
  "path.copy": {
    visible: isSingleFolder,
    run: (ctx) => {
      const [entry] = ctx.items;
      return entry === undefined || ctx.bucket === null
        ? undefined
        : copyToClipboard(ctx.model, s3Uri(ctx.bucket, entry.key), "Path");
    },
  },

  // ---- the open folder ----
  "list.refresh": {
    many: true,
    // It reloads the open folder, so it belongs to the object list and not to a folder of the bucket tree.
    visible: (ctx) => ctx.pane === "files",
    disabled: needsAccount,
    run: (ctx) => ctx.model.browser.refresh(),
  },
  "list.select-all": { many: true, disabled: needsRows, run: (ctx) => selectAllEntries(ctx.model) },
  "list.invert": { many: true, disabled: needsRows, run: (ctx) => invertSelectedEntries(ctx.model) },
  "list.search": {
    disabled: needsPlace,
    run: () => {
      focusSearch();
    },
  },

  // ---- buckets ----
  "bucket.open": {
    visible: (ctx) => ctx.buckets.length === 1,
    run: async (ctx) => {
      const [bucket] = ctx.buckets;
      if (bucket !== undefined) {
        await ctx.model.browser.openLocation(bucket.name, "");
      }
    },
  },
  "bucket.new": {
    disabled: needsAccount,
    run: (ctx) => {
      if (ctx.accountId !== null) {
        ctx.model.dialogs.open({ kind: "create-bucket", accountId: ctx.accountId });
      }
    },
  },
  "bucket.refresh": {
    many: true,
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.browser.resetTree();
      return ctx.model.browser.loadBuckets();
    },
  },
  "bucket.download-all": {
    visible: (ctx) => ctx.buckets.length === 1,
    disabled: (ctx) => {
      const [bucket] = ctx.buckets;
      return bucket !== undefined && knownEmpty(ctx.model, bucket.name) ? t("hint.empty-bucket") : null;
    },
    run: async (ctx) => {
      const [bucket] = ctx.buckets;
      if (bucket !== undefined) {
        await downloadAll(ctx.model, bucket.name);
      }
    },
  },
  "bucket.copy-name": {
    many: true,
    run: (ctx) => copyToClipboard(ctx.model, ctx.buckets.map((bucket) => bucket.name).join("\n"), "Bucket name"),
  },
  "bucket.copy-url": {
    many: true,
    visible: (ctx) => ctx.account !== null,
    run: (ctx) => {
      const { account } = ctx;
      if (account === null) {
        return undefined;
      }
      const urls = ctx.buckets.map((bucket) => bucketUrl(account, bucket.name, bucket.region));
      return copyToClipboard(ctx.model, urls.join("\n"), "URL");
    },
  },
  "bucket.remove-external": {
    danger: true,
    visible: (ctx) => {
      const [bucket] = ctx.buckets;
      return ctx.buckets.length === 1 && bucket !== undefined && ctx.model.browser.state.bucketList.pinned.includes(bucket.name);
    },
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      return ctx.accountId === null || bucket === undefined
        ? undefined
        : removeExternalBucket(ctx.model, ctx.accountId, bucket.name);
    },
  },
  "bucket.delete": {
    many: true,
    danger: true,
    disabled: needsAccount,
    run: (ctx) => {
      // One typed confirmation per bucket: the user types each name.
      const { accountId } = ctx;
      if (accountId === null) {
        return;
      }
      for (const bucket of ctx.buckets) {
        ctx.model.dialogs.open({ kind: "delete-bucket", accountId, bucket: bucket.name });
      }
    },
  },

  // ---- accounts ----
  "account.edit": {
    visible: (ctx) => ctx.account !== null,
    run: (ctx) => {
      if (ctx.account !== null) {
        ctx.model.dialogs.open({ kind: "edit-account", accountId: ctx.account.id });
      }
    },
  },
  "account.set-default": {
    visible: (ctx) => ctx.account !== null,
    disabled: (ctx) =>
      ctx.account !== null && ctx.model.session.store.getState().defaultAccountId === ctx.account.id
        ? t("hint.already-default")
        : null,
    run: (ctx) => (ctx.account === null ? undefined : makeDefault(ctx.model, ctx.account)),
  },
  "account.master-password": {
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "master-password" });
    },
  },
  "account.mfa": {
    visible: (ctx) => ctx.account !== null,
    run: (ctx) => {
      if (ctx.account !== null) {
        ctx.model.dialogs.open({ kind: "edit-account", accountId: ctx.account.id, focus: "mfa" });
      }
    },
  },
  "account.remove": {
    danger: true,
    visible: (ctx) => ctx.account !== null,
    run: (ctx) => (ctx.account === null ? undefined : removeAccount(ctx.model, ctx.account)),
  },
};

/** The location a menu entry of a bucket or of one folder or object stands for. */
function targetLocation(ctx: MenuContext): { accountId: string; bucket: string; prefix: string } | null {
  if (ctx.accountId === null) {
    return null;
  }
  const [entry] = ctx.items;
  if (ctx.items.length === 1 && entry !== undefined && ctx.bucket !== null) {
    return { accountId: ctx.accountId, bucket: ctx.bucket, prefix: entry.key };
  }
  const [bucket] = ctx.buckets;
  return ctx.buckets.length === 1 && bucket !== undefined ? { accountId: ctx.accountId, bucket: bucket.name, prefix: "" } : null;
}

/** A CloudFront entry of the bucket menu: opens the manager on the distributions of the single bucket. */
function cloudfrontEntry(view: CloudFrontView): ActionDef {
  return {
    requires: (caps) => !caps.external_endpoint,
    visible: isSingleBucket,
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId !== null && bucket !== undefined) {
        showCloudFront(ctx.model, ctx.accountId, bucket.name, view);
      }
    },
  };
}

/** A bucket property entry: opens the Bucket properties dialog of the single bucket on the tab of its setting. */
function bucketProperty(tab: BucketPropertiesTab, requires?: (caps: BucketCaps) => boolean): ActionDef {
  return {
    requires,
    visible: isSingleBucket,
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId !== null && bucket !== undefined) {
        showBucketProperties(ctx.model, tab, { accountId: ctx.accountId, bucket: bucket.name });
      }
    },
  };
}

/** A bucket rules entry (CORS, lifecycle, Intelligent-Tiering, replication): opens the rules dialog of the single bucket. */
function bucketRules(tab: BucketRulesTab): ActionDef {
  return {
    visible: isSingleBucket,
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId !== null && bucket !== undefined) {
        ctx.model.dialogs.open({ kind: "bucket-rules", accountId: ctx.accountId, bucket: bucket.name, tab });
      }
    },
  };
}

/** A bucket entry that opens one of the T07 dialogs. */
function bucketDialog(kind: "bucket-lock" | "bucket-ownership" | "sharing-wizard"): ActionDef {
  return {
    visible: isSingleBucket,
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId !== null && bucket !== undefined) {
        ctx.model.dialogs.open({ kind, accountId: ctx.accountId, bucket: bucket.name });
      }
    },
  };
}

/** An object entry that opens one of the T07 dialogs on the selected objects. */
function objectDialog(kind: "object-lock" | "object-restore"): ActionDef {
  return {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      if (ctx.bucket !== null && ctx.accountId !== null) {
        ctx.model.dialogs.open({ kind, accountId: ctx.accountId, bucket: ctx.bucket, keys: ctx.items.map((entry) => entry.key) });
      }
    },
  };
}

/** An entry that opens a dialog on the one selected object. */
function singleFileDialog(kind: "open-with" | "website-redirect"): ActionDef {
  return {
    visible: isSingleFile,
    run: (ctx) => {
      const [entry] = ctx.items;
      if (ctx.bucket !== null && ctx.accountId !== null && entry !== undefined) {
        ctx.model.dialogs.open({ kind, accountId: ctx.accountId, bucket: ctx.bucket, key: entry.key });
      }
    },
  };
}

/** A metadata entry: opens the properties editor of the selected objects (one or several) on a tab. */
function objectProperty(tab: ObjectPropertiesTab): ActionDef {
  return {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      showProperties(ctx.model, subjectOf(ctx), tab);
    },
  };
}

/**
 * Handlers of features that have landed (bucket configuration T03, object metadata T06, versions and
 * preview T08, tools and export T12). Each one reuses the action function its toolbar button uses.
 * Add the definition here, then turn the feature on in available.ts.
 */
const PLANNED_DEFS: Partial<Record<PlannedActionId, ActionDef>> = {
  // ---- T14: windows and bookmarks ----
  "window.open-new": {
    visible: (ctx) => isSingleBucket(ctx) || isSingleFolder(ctx),
    disabled: needsAccount,
    run: (ctx) => openInNewWindow(ctx.model, targetLocation(ctx)),
  },
  "bookmark.add": {
    visible: (ctx) => isSingleBucket(ctx) || isSingle(ctx),
    disabled: needsAccount,
    run: (ctx) => {
      const at = targetLocation(ctx);
      if (at !== null) {
        showAddBookmark(ctx.model, at.accountId, at.bucket, at.prefix);
      }
    },
  },

  // ---- T03: bucket configuration ----
  "bucket.versioning": bucketProperty("versioning", (caps) => caps.versioning_supported),
  "bucket.logging": bucketProperty("logging"),
  "bucket.policy": bucketProperty("policy"),
  "bucket.public-access": bucketProperty("public-access"),
  "bucket.website": bucketProperty("website", (caps) => caps.website_supported),
  "bucket.website-routing": bucketProperty("website", (caps) => caps.website_supported),
  "bucket.acceleration": bucketProperty("acceleration", (caps) => caps.acceleration_supported),
  // ---- T04/T07: bucket rules ----
  "bucket.cors": bucketRules("cors"),
  "bucket.lifecycle": bucketRules("lifecycle"),
  "bucket.tiering": bucketRules("tiering"),
  "bucket.replication": bucketRules("replication"),
  "bucket.add-external": {
    disabled: needsAccount,
    run: (ctx) => {
      showAddExternalBucket(ctx.model);
    },
  },

  // ---- T05: destination picker, copy and move ----
  "file.copy-to": {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      chooseDestination(ctx.model, "copy", subjectOf(ctx));
    },
  },
  "file.move-to": {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      chooseDestination(ctx.model, "move", subjectOf(ctx));
    },
  },
  "folder.copy-all": {
    visible: isSingleFolder,
    run: (ctx) => {
      chooseDestination(ctx.model, "copy", folderSubject(subjectOf(ctx)));
    },
  },
  "folder.move-all": {
    visible: isSingleFolder,
    run: (ctx) => {
      chooseDestination(ctx.model, "move", folderSubject(subjectOf(ctx)));
    },
  },
  "bucket.paste": {
    visible: isSingleBucket,
    disabled: (ctx) => pasteBlockerFor(ctx.clipboard, pasteLocation(ctx)),
    run: (ctx) => paste(ctx.model, ctx.place ?? undefined),
  },
  // ---- T10: sharing wizard ----
  "bucket.sharing-wizard": bucketDialog("sharing-wizard"),

  // ---- T07: Object Lock, ownership and restore ----
  "bucket.ownership": bucketDialog("bucket-ownership"),
  "bucket.object-lock": bucketDialog("bucket-lock"),
  "file.lock": objectDialog("object-lock"),
  "file.restore": objectDialog("object-restore"),

  // ---- T06: object metadata (the editors also work on several objects at once) ----
  "file.tags": objectProperty("tags"),
  "file.storage-class": objectProperty("class"),
  "file.sse": objectProperty("encryption"),
  "file.http-headers": objectProperty("headers"),
  "file.acl": objectProperty("permissions"),

  // ---- T14: opening ----
  "file.open-with": singleFileDialog("open-with"),
  "file.edit": {
    visible: isSingleFile,
    run: async (ctx) => {
      const [entry] = ctx.items;
      if (ctx.bucket !== null && ctx.accountId !== null && entry !== undefined) {
        await ctx.model.edits.start(ctx.accountId, ctx.bucket, entry.key);
      }
    },
  },
  "file.website-redirect": singleFileDialog("website-redirect"),
  "bucket.open-browser": {
    visible: (ctx) => ctx.account !== null && isSingleBucket(ctx),
    disabled: needsAccount,
    run: async (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId === null || bucket === undefined) {
        return;
      }
      try {
        const url = await ctx.model.api.open_bucket_in_browser({ account_id: ctx.accountId, bucket: bucket.name, region: bucket.region });
        ctx.model.status.info(`Opened ${url} in the browser.`);
      } catch (error) {
        ctx.model.status.error(error, { notify: "The bucket could not be opened in the browser" });
      }
    },
  },

  // ---- T09: CloudFront ----
  "tools.cloudfront": {
    disabled: needsAccount,
    run: (ctx) => {
      if (ctx.accountId !== null) {
        showCloudFront(ctx.model, ctx.accountId, null);
      }
    },
  },
  "cloudfront.toggle": cloudfrontEntry("toggle"),
  "cloudfront.invalidate": cloudfrontEntry("invalidate"),
  "cloudfront.history": cloudfrontEntry("history"),
  "cloudfront.attach": cloudfrontEntry("attach"),
  "cloudfront.edit": cloudfrontEntry("edit"),
  "cloudfront.detach": cloudfrontEntry("detach"),
  // ---- T07: client-side encryption ----
  "file.encrypt": {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      showEncryption(ctx.model, "encrypt", subjectOf(ctx));
    },
  },
  "file.decrypt": {
    many: true,
    visible: onlyFiles,
    run: (ctx) => {
      showEncryption(ctx.model, "decrypt", subjectOf(ctx));
    },
  },

  // ---- T08: preview and versions ----
  "file.preview": {
    visible: isSingleFile,
    run: (ctx) => {
      previewSelected(ctx.model, subjectOf(ctx));
    },
  },

  // ---- T12: export, saved filters, diagnostics, unfinished uploads ----
  "file.export": {
    many: true,
    disabled: needsPlace,
    run: (ctx) => {
      openExportDialog(ctx.model, ctx.place ?? undefined);
    },
  },
  "list.filter": {
    visible: (ctx) => ctx.count === 0 || isSingleFolder(ctx),
    run: async (ctx) => {
      if (isSingleFolder(ctx)) {
        await showFolderOf(ctx);
      }
      ctx.model.dialogs.open({ kind: "filters" });
    },
  },
  // ---- T04: mount as drive (shown only when the build supports it) ----
  "bucket.mount": {
    visible: (ctx) => isSingleBucket(ctx) && mountSupported(ctx.model),
    disabled: needsAccount,
    run: (ctx) => {
      const [bucket] = ctx.buckets;
      if (ctx.accountId !== null && bucket !== undefined) {
        ctx.model.dialogs.open({ kind: "mount", accountId: ctx.accountId, bucket: bucket.name });
      }
    },
  },
  "tools.iam": {
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "iam" });
    },
  },
  "tools.diagnostics": {
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "diagnostics" });
    },
  },
  // ---- T13: security scan ----
  "tools.security-scan": {
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "security-scan" });
    },
  },
  "tools.multipart": {
    disabled: needsAccount,
    run: (ctx) => {
      ctx.model.dialogs.open({ kind: "uploads" });
    },
  },
  "account.test": {
    visible: (ctx) => ctx.account !== null,
    run: (ctx) => {
      if (ctx.account !== null) {
        ctx.model.dialogs.open({ kind: "diagnostics", accountId: ctx.account.id });
      }
    },
  },
};

export type ActionTable = Partial<Record<ActionId, ActionDef>>;

export const ACTIONS: ActionTable = { ...PLANNED_DEFS, ...LIVE };

/** What the entries resolve against; tests pass their own to try other flags and handlers. */
export interface RegistryDeps {
  defs: ActionTable;
  available: (id: ActionId) => boolean;
}

export const DEFAULT_DEPS: RegistryDeps = { defs: ACTIONS, available: (id) => isAvailable(id) };

export type ActionState = { visible: false } | { visible: true; disabled: string | null };

const HIDDEN: ActionState = { visible: false };

/**
 * Whether an action applies to the context, and if so whether it is enabled.
 * Hidden: the feature is not available, the endpoint lacks the capability, or
 * the action does not apply to this target. Disabled: it applies, but the
 * current state (an empty clipboard, no bucket open) does not allow it.
 */
export function actionState(id: ActionId, ctx: MenuContext, deps: RegistryDeps = DEFAULT_DEPS): ActionState {
  const def = deps.defs[id];
  if (!deps.available(id) || def === undefined) {
    return HIDDEN;
  }
  if (def.requires !== undefined && !def.requires(ctx.caps)) {
    return HIDDEN;
  }
  if (def.many !== true && ctx.count > 1) {
    return HIDDEN;
  }
  if (def.visible !== undefined && !def.visible(ctx)) {
    return HIDDEN;
  }
  return { visible: true, disabled: def.disabled?.(ctx) ?? null };
}

/** Runs an action unless it is hidden or disabled; failures go to the status bar. Returns whether it started. */
export function runAction(id: ActionId, ctx: MenuContext, deps: RegistryDeps = DEFAULT_DEPS): boolean {
  const state = actionState(id, ctx, deps);
  const def = deps.defs[id];
  if (!state.visible || def === undefined) {
    return false;
  }
  if (state.disabled !== null) {
    if (def.blockedTitle !== undefined) {
      ctx.model.status.error(state.disabled, { notify: def.blockedTitle });
    }
    return false;
  }
  void (async () => {
    try {
      await def.run(ctx);
    } catch (error) {
      ctx.model.status.error(error);
    }
  })();
  return true;
}
