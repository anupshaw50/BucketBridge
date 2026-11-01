import { LIVE_ACTION_IDS, type ActionId, type PlannedActionId } from "./ids";

/**
 * Feature availability of the menu entries that were planned before their feature existed. `true`: the feature
 * is built and its handler is in PLANNED_DEFS (registry.ts); `false`: it is not built and the entry stays hidden.
 *
 * An entry whose flag is `false` is hidden in every menu (and in the toolbar
 * and keymap, which resolve the same ids). Every planned id must be listed, so
 * the type checker reports a new id without a flag.
 *
 * To wire a feature when it lands:
 *   1. add its definition to PLANNED_DEFS in registry.ts (its `run` opens the
 *      new dialog or enqueues the new work), and
 *   2. flip its flag here.
 * The registry test fails when a flag is `true` and the action has no handler,
 * so an entry can never be shown without working.
 */
export const AVAILABLE: Readonly<Record<PlannedActionId, boolean>> = {
  // Another main window over the same core (T14).
  "window.open-new": true,
  // Saved locations kept in the configuration (T14).
  "bookmark.add": true,
  // Saved filters (T12): the dialog applies one to the open folder.
  "list.filter": true,
  // The read-only FUSE mount (T04). The entry only shows when the build supports it (features/mount/support.ts).
  "bucket.mount": true,
  // The sharing wizard (T10): a presigned link for an object of the bucket, with optional shortening.
  "bucket.sharing-wizard": true,
  // Opens through the open_bucket_in_browser command, which needs no opener capability.
  "bucket.open-browser": true,
  // Adds a bucket of another provider or account to the bucket list (T03).
  "bucket.add-external": true,
  // Pastes the clipboard into the root of the bucket.
  "bucket.paste": true,
  // Bucket properties (T03) open the dialog on the tab of the setting; the editors of T07 are not built yet.
  "bucket.versioning": true,
  "bucket.logging": true,
  "bucket.policy": true,
  "bucket.public-access": true,
  "bucket.cors": true,
  "bucket.lifecycle": true,
  "bucket.tiering": true,
  "bucket.replication": true,
  "bucket.website": true,
  "bucket.website-routing": true,
  "bucket.acceleration": true,
  "bucket.ownership": true,
  "bucket.object-lock": true,
  // CloudFront manager (T09).
  "cloudfront.attach": true,
  "cloudfront.edit": true,
  "cloudfront.toggle": true,
  "cloudfront.invalidate": true,
  "cloudfront.history": true,
  "cloudfront.detach": true,
  // Batch copy and move of a subtree to a destination chosen in the picker (T05).
  "folder.copy-all": true,
  "folder.move-all": true,
  // Object features: open with and website redirect have dialogs; metadata (T06), T07, preview (T08), client-side
  // encryption and export (T12) are built.
  "file.open-with": true,
  "file.preview": true,
  "file.move-to": true,
  "file.copy-to": true,
  "file.tags": true,
  "file.storage-class": true,
  "file.sse": true,
  "file.http-headers": true,
  "file.website-redirect": true,
  "file.acl": true,
  "file.encrypt": true,
  "file.decrypt": true,
  "file.lock": true,
  "file.restore": true,
  "file.export": true,
  // Tools: IAM (T02), CloudFront (T09), the security scan (T13), diagnostics and unfinished uploads (T12).
  "tools.iam": true,
  "tools.cloudfront": true,
  "tools.diagnostics": true,
  "tools.security-scan": true,
  "tools.multipart": true,
  // Opens the diagnostics probe for the account (T12).
  "account.test": true,
};

const LIVE: ReadonlySet<string> = new Set(LIVE_ACTION_IDS);

/** Whether the feature behind an action exists in this build. Actions in use are always available. */
export function isAvailable(id: ActionId, flags: Readonly<Record<PlannedActionId, boolean>> = AVAILABLE): boolean {
  return LIVE.has(id) || flags[id as PlannedActionId];
}
