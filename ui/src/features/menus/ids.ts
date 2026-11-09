// Identifiers of every menu action. The ids are the keys of the message bundle
// (src/i18n/en.ts) and of the availability map (available.ts), so a label or a
// flag that is missing is a compile error.

/** Actions that work in this build. Each one has a handler in registry.ts. */
export const LIVE_ACTION_IDS = [
  "open",
  "item.download",
  "item.rename",
  "item.share",
  "item.properties",
  "item.versions",
  "item.delete",
  "clipboard.copy",
  "clipboard.cut",
  "clipboard.paste",
  "folder.new",
  "folder.search",
  "upload.files",
  "upload.folder",
  "sync.open",
  "sync.jobs",
  "path.copy",
  "list.refresh",
  "list.select-all",
  "list.invert",
  "list.search",
  "bucket.open",
  "bucket.new",
  "bucket.refresh",
  "bucket.download-all",
  "bucket.copy-name",
  "bucket.copy-url",
  "copy.name",
  "copy.key",
  "copy.uri",
  "copy.url-virtual",
  "copy.url-path",
  "bucket.remove-external",
  "bucket.delete",
  "account.edit",
  "account.set-default",
  "account.master-password",
  "account.mfa",
  "account.remove",
] as const;

/**
 * Actions of features that are not built yet. They are laid out in the menus
 * and have labels, but stay hidden until their flag in available.ts is turned on.
 */
export const PLANNED_ACTION_IDS = [
  "window.open-new",
  "bookmark.add",
  "list.filter",
  "bucket.sharing-wizard",
  "bucket.mount",
  "bucket.open-browser",
  "bucket.add-external",
  "bucket.paste",
  "bucket.versioning",
  "bucket.logging",
  "bucket.policy",
  "bucket.public-access",
  "bucket.cors",
  "bucket.lifecycle",
  "bucket.tiering",
  "bucket.replication",
  "bucket.website",
  "bucket.website-routing",
  "bucket.acceleration",
  "bucket.ownership",
  "bucket.object-lock",
  "cloudfront.attach",
  "cloudfront.edit",
  "cloudfront.toggle",
  "cloudfront.invalidate",
  "cloudfront.history",
  "cloudfront.detach",
  "folder.copy-all",
  "folder.move-all",
  "file.open-with",
  "file.edit",
  "file.preview",
  "file.move-to",
  "file.copy-to",
  "file.tags",
  "file.storage-class",
  "file.sse",
  "file.http-headers",
  "file.website-redirect",
  "file.acl",
  "file.encrypt",
  "file.decrypt",
  "file.lock",
  "file.restore",
  "file.export",
  "tools.iam",
  "tools.cloudfront",
  "tools.diagnostics",
  "tools.security-scan",
  "tools.multipart",
  "account.test",
] as const;

export type LiveActionId = (typeof LIVE_ACTION_IDS)[number];
export type PlannedActionId = (typeof PLANNED_ACTION_IDS)[number];
export type ActionId = LiveActionId | PlannedActionId;

/** One menu per node type. `selection` is the menu of several selected rows. */
export const MENU_IDS = ["bucket", "folder", "file", "selection", "files-empty", "buckets-empty", "account"] as const;
export type MenuId = (typeof MENU_IDS)[number];

export const SUBMENU_IDS = ["bucket.properties", "bucket.cloudfront", "file.metadata", "copy-as"] as const;
export type SubmenuId = (typeof SUBMENU_IDS)[number];

/** Reasons shown as the tooltip of a disabled entry. */
export const HINT_IDS = [
  "no-bucket",
  "no-account",
  "no-items",
  "empty-list",
  "empty-bucket",
  "already-default",
] as const;
export type HintId = (typeof HINT_IDS)[number];

export type MessageKey =
  | `action.${ActionId}`
  | `menu.${MenuId}`
  | `submenu.${SubmenuId}`
  | `hint.${HintId}`;

/** The pane that owns a menu; it decides which shortcuts are shown. */
export type Pane = "files" | "buckets" | "accounts";
