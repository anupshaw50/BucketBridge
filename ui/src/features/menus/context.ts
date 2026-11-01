import type { Account, Bucket, ObjectEntry } from "../../api/types";
import type { AppModel, ClipboardContents } from "../../state/model";
import { type Place, openLocation, selectedEntries } from "../objects/actions";
import { isFolderEntry } from "../../lib/objects";
import type { Pane } from "./ids";

/** What the account and bucket support. Entries are hidden when their capability is missing. */
export interface BucketCaps {
  versioning_supported: boolean;
  versioning_enabled: boolean;
  object_lock_enabled: boolean;
  replication_supported: boolean;
  website_supported: boolean;
  acceleration_supported: boolean;
  /** x-amz-request-payer is required (the account has "requester pays" on). */
  requester_pays: boolean;
  /** S3-compatible provider: the account has a custom endpoint. */
  external_endpoint: boolean;
  cloudfront_attached: boolean;
}

/**
 * Capabilities from data that is already loaded, so opening a menu never
 * performs network work: an account with a custom endpoint is an external
 * provider, which does not offer the bucket sub-resource APIs unless it is
 * known to. Results of capability probes (GetBucketVersioning and so on) belong
 * here once a probe cache exists; they override these defaults per bucket.
 */
export function capsFor(account: Account | null, bucket: Bucket | null): BucketCaps {
  const external = (account !== null && account.endpoint !== null) || bucket?.kind === "external";
  return {
    versioning_supported: !external,
    versioning_enabled: false,
    object_lock_enabled: false,
    replication_supported: !external,
    website_supported: !external,
    acceleration_supported: !external,
    requester_pays: account?.request_payer ?? false,
    external_endpoint: external,
    cloudfront_attached: bucket?.kind === "cloudfront",
  };
}

/** What several buckets have in common: a capability counts only when every bucket has it. */
export function commonCaps(list: readonly BucketCaps[]): BucketCaps {
  const [first, ...rest] = list;
  if (first === undefined) {
    return capsFor(null, null);
  }
  const result = { ...first };
  for (const caps of rest) {
    for (const key of Object.keys(result) as (keyof BucketCaps)[]) {
      result[key] = key === "external_endpoint" || key === "requester_pays" ? result[key] || caps[key] : result[key] && caps[key];
    }
  }
  return result;
}

/** Everything a menu entry needs to decide whether it is shown, enabled and what it does. */
export interface MenuContext {
  model: AppModel;
  /** The pane that owns the menu; it decides which shortcuts are shown. */
  pane: Pane;
  /** The active account (or the account row in the account menu). */
  accountId: string | null;
  account: Account | null;
  caps: BucketCaps;
  /** Buckets the menu was opened on (bucket menu). */
  buckets: readonly Bucket[];
  /** Bucket of `items`. */
  bucket: string | null;
  /** Folders and files the menu was opened on. */
  items: readonly ObjectEntry[];
  /** Folder that new items, uploads and a paste go to. */
  place: Place | null;
  /** How many things the menu acts on (1 for a single node, N for a multi-selection, 0 for empty space). */
  count: number;
  /** Entries of the open folder after the filter. */
  visibleCount: number;
  clipboard: ClipboardContents | null;
}

function base(model: AppModel, pane: Pane) {
  const session = model.session.store.getState();
  const account = session.accounts.find((item) => item.id === session.activeAccountId) ?? null;
  return {
    model,
    pane,
    accountId: session.activeAccountId,
    account,
    visibleCount: model.browser.state.visibleKeys.length,
    clipboard: model.clipboard.getState().contents,
  };
}

function bucketByName(model: AppModel, name: string): Bucket | null {
  return model.browser.state.bucketList.buckets.find((item) => item.name === name) ?? null;
}

/** Folder that the open listing shows, as a place. */
function openPlace(model: AppModel): Place | null {
  const location = openLocation(model);
  return location === null ? null : { bucket: location.bucket, prefix: location.prefix };
}

/** Menu of one or more buckets of the buckets pane. */
export function bucketContext(model: AppModel, buckets: readonly Bucket[]): MenuContext {
  const info = base(model, "buckets");
  const [first] = buckets;
  return {
    ...info,
    caps: commonCaps(buckets.map((bucket) => capsFor(info.account, bucket))),
    buckets,
    bucket: first?.name ?? null,
    items: [],
    place: buckets.length === 1 && first !== undefined ? { bucket: first.name, prefix: "" } : null,
    count: buckets.length,
  };
}

/** Menu of one or more folders and files, in the object list or (folders) in the bucket tree. */
export function itemsContext(
  model: AppModel,
  bucket: string,
  entries: readonly ObjectEntry[],
  pane: Pane = "files",
): MenuContext {
  const info = base(model, pane);
  const [first] = entries;
  const open = openPlace(model);
  let place: Place | null;
  if (entries.length === 1 && first !== undefined && isFolderEntry(first)) {
    place = { bucket, prefix: first.key };
  } else if (open !== null && open.bucket === bucket) {
    place = open;
  } else {
    place = { bucket, prefix: "" };
  }
  return {
    ...info,
    caps: capsFor(info.account, bucketByName(model, bucket)),
    buckets: [],
    bucket,
    items: entries,
    place,
    count: entries.length,
  };
}

/** Menu of the empty area of the object list. */
export function filesEmptyContext(model: AppModel): MenuContext {
  const info = base(model, "files");
  const open = openPlace(model);
  return {
    ...info,
    caps: capsFor(info.account, open === null ? null : bucketByName(model, open.bucket)),
    buckets: [],
    bucket: open?.bucket ?? null,
    items: [],
    place: open,
    count: 0,
  };
}

/** Menu of the empty area of the buckets pane. */
export function bucketsEmptyContext(model: AppModel): MenuContext {
  const info = base(model, "buckets");
  return { ...info, caps: capsFor(info.account, null), buckets: [], bucket: null, items: [], place: null, count: 0 };
}

/** Menu of an account. */
export function accountContext(model: AppModel, account: Account): MenuContext {
  const info = base(model, "accounts");
  return {
    ...info,
    accountId: account.id,
    account,
    caps: capsFor(account, null),
    buckets: [],
    bucket: null,
    items: [],
    place: null,
    count: 1,
  };
}

/**
 * The context of the object list as the toolbar and the keyboard see it: the
 * selected rows of the open folder (or `items`, when a key acts on the focused
 * row), with the open folder as the place.
 */
export function filesPaneContext(model: AppModel, items: readonly ObjectEntry[] = selectedEntries(model)): MenuContext {
  const info = base(model, "files");
  const open = openPlace(model);
  return {
    ...info,
    caps: capsFor(info.account, open === null ? null : bucketByName(model, open.bucket)),
    buckets: [],
    bucket: open?.bucket ?? null,
    items,
    place: open,
    count: items.length,
  };
}

/** The context of the bucket tree as the keyboard sees it: one bucket (or none, for the pane itself). */
export function bucketsPaneContext(model: AppModel, buckets: readonly Bucket[] = []): MenuContext {
  return buckets.length === 0 ? bucketsEmptyContext(model) : bucketContext(model, buckets);
}
