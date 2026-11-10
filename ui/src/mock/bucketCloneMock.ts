// The clone commands of the mock backend (docs/desktop-api.md, "Clone a bucket"). They are built from the
// commands of the settings the clone copies, so the mock follows the same rules as the real calls (a bucket that
// exists is a conflict, a refused setting is reported on its own). The mock keeps no bucket tags or default
// encryption, so those two always read as not configured.

import type { Backend } from "../api/backend";
import { BackendError, toBbError } from "../api/errors";
import type {
  BbError,
  CloneBucketResult,
  ClonePreview,
  ClonePreviewWarning,
  CloneSetting,
  CloneSettingPreview,
  CloneSettingResult,
  TransferTask,
} from "../api/types";

/** The settings in the order the host applies them. */
export const CLONE_ORDER: readonly CloneSetting[] = [
  "versioning",
  "encryption",
  "public_access_block",
  "policy",
  "website",
  "cors",
  "tags",
  "lifecycle",
];

export type CloneApi = Pick<
  Backend,
  | "list_buckets"
  | "list_objects"
  | "create_bucket"
  | "enqueue_copy"
  | "get_bucket_policy"
  | "put_bucket_policy"
  | "get_bucket_cors"
  | "put_bucket_cors"
  | "get_bucket_lifecycle"
  | "put_bucket_lifecycle"
  | "get_bucket_versioning"
  | "put_bucket_versioning"
  | "get_bucket_website"
  | "put_bucket_website"
  | "get_public_access_block"
  | "put_public_access_block"
  | "delete_public_access_block"
>;

interface Read {
  configured: boolean;
  count: number | null;
  detail: string | null;
  apply: (target: string) => Promise<void>;
  warning?: ClonePreviewWarning;
}

function statementsOf(policy: string): { count: number; denies: boolean } {
  try {
    const doc = JSON.parse(policy) as { Statement?: unknown };
    const list = Array.isArray(doc.Statement) ? doc.Statement : doc.Statement === undefined ? [] : [doc.Statement];
    return { count: list.length, denies: list.some((item) => (item as { Effect?: unknown }).Effect === "Deny") };
  } catch {
    return { count: 0, denies: false };
  }
}

/** The policy with `arn:aws:s3:::from` and `arn:aws:s3:::from/...` renamed to `to`; other names stay. */
export function renameBucketInPolicy(policy: string, from: string, to: string): string {
  let doc: unknown;
  try {
    doc = JSON.parse(policy);
  } catch {
    return policy;
  }
  const needle = `:s3:::${from}`;
  const rename = (text: string): string => {
    let out = "";
    let rest = text;
    for (let at = rest.indexOf(needle); at >= 0; at = rest.indexOf(needle)) {
      const end = at + needle.length;
      const next = rest.charAt(end);
      out += `${rest.slice(0, at)}:s3:::${next === "" || next === "/" || next === "*" ? to : from}`;
      rest = rest.slice(end);
    }
    return out + rest;
  };
  const walk = (value: unknown): unknown => {
    if (typeof value === "string") {
      return rename(value);
    }
    if (Array.isArray(value)) {
      return value.map(walk);
    }
    if (value !== null && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, walk(item)]));
    }
    return value;
  };
  return JSON.stringify(walk(doc), null, 2);
}

function readers(api: CloneApi, account: string, bucket: string): Record<CloneSetting, () => Promise<Read>> {
  const none = (): Read => ({ configured: false, count: null, detail: null, apply: () => Promise.resolve() });
  return {
    versioning: async () => {
      const value = await api.get_bucket_versioning({ account_id: account, bucket });
      return {
        configured: value.status !== "unversioned",
        count: null,
        detail: value.status === "unversioned" ? null : value.status,
        warning: value.status === "enabled" ? "versions_not_copied" : undefined,
        apply: (target) => api.put_bucket_versioning({ account_id: account, bucket: target, status: value.status }),
      };
    },
    encryption: () => Promise.resolve(none()),
    public_access_block: async () => {
      const value = await api.get_public_access_block({ account_id: account, bucket });
      const blocked = value === null ? 0 : [value.block_public_acls, value.ignore_public_acls, value.block_public_policy, value.restrict_public_buckets].filter(Boolean).length;
      return {
        configured: value !== null,
        count: value === null ? null : blocked,
        detail: null,
        apply: (target) =>
          value === null
            ? api.delete_public_access_block({ account_id: account, bucket: target })
            : api.put_public_access_block({ account_id: account, bucket: target, config: value }),
      };
    },
    policy: async () => {
      const value = await api.get_bucket_policy({ account_id: account, bucket });
      const found = value === null ? null : statementsOf(value);
      return {
        configured: value !== null,
        count: found?.count ?? null,
        detail: null,
        warning: found?.denies === true ? "policy_denies" : undefined,
        apply: (target) =>
          value === null ? Promise.resolve() : api.put_bucket_policy({ account_id: account, bucket: target, policy: renameBucketInPolicy(value, bucket, target) }),
      };
    },
    website: async () => {
      const value = await api.get_bucket_website({ account_id: account, bucket });
      return {
        configured: value !== null,
        count: null,
        detail: value === null ? null : value.type === "hosting" ? value.index_document : value.host_name,
        apply: (target) => (value === null ? Promise.resolve() : api.put_bucket_website({ account_id: account, bucket: target, website: value })),
      };
    },
    cors: async () => {
      const value = await api.get_bucket_cors({ account_id: account, bucket });
      return {
        configured: value !== null,
        count: value?.rules.length ?? null,
        detail: null,
        apply: (target) => (value === null ? Promise.resolve() : api.put_bucket_cors({ account_id: account, bucket: target, config: value })),
      };
    },
    tags: () => Promise.resolve(none()),
    lifecycle: async () => {
      const value = await api.get_bucket_lifecycle({ account_id: account, bucket });
      return {
        configured: value !== null,
        count: value?.rules.length ?? null,
        detail: null,
        apply: (target) => (value === null ? Promise.resolve() : api.put_bucket_lifecycle({ account_id: account, bucket: target, config: value })),
      };
    },
  };
}

function bbError(raw: unknown): BbError {
  return toBbError(raw).toJSON();
}

async function sourceRegion(api: CloneApi, account: string, bucket: string): Promise<string | null> {
  const buckets = await api.list_buckets({ account_id: account });
  return buckets.find((item) => item.name === bucket)?.region ?? null;
}

export async function previewClone(api: CloneApi, args: { account_id: string; bucket: string }): Promise<ClonePreview> {
  const region = await sourceRegion(api, args.account_id, args.bucket);
  const reads = readers(api, args.account_id, args.bucket);
  const settings: CloneSettingPreview[] = [];
  const warnings = new Set<ClonePreviewWarning>();
  for (const setting of CLONE_ORDER) {
    try {
      const read = await reads[setting]();
      if (read.warning !== undefined) {
        warnings.add(read.warning);
      }
      settings.push({
        setting,
        state: read.configured ? "configured" : "not_configured",
        count: read.configured ? read.count : null,
        detail: read.detail,
        error: null,
      });
    } catch (raw) {
      const error = bbError(raw);
      settings.push({ setting, state: error.code === "remote" ? "unsupported" : "failed", count: null, detail: null, error });
    }
  }
  return { source_bucket: args.bucket, region, settings, warnings: ["policy_denies", "kms_key", "versions_not_copied"].filter((w): w is ClonePreviewWarning => warnings.has(w as ClonePreviewWarning)) };
}

export async function cloneBucket(api: CloneApi, args: {
  account_id: string;
  source_bucket: string;
  new_bucket: string;
  region: string | null;
  prefix: string;
  copy_objects: boolean;
  settings: CloneSetting[];
}): Promise<CloneBucketResult> {
  if (args.new_bucket === args.source_bucket) {
    throw new BackendError("usage", "The new bucket must have a different name than the source.");
  }
  if (args.prefix !== "" && !args.prefix.endsWith("/")) {
    throw new BackendError("usage", `The prefix '${args.prefix}' must end with '/'.`);
  }
  const reads = readers(api, args.account_id, args.source_bucket);
  const wanted = CLONE_ORDER.filter((setting) => args.settings.includes(setting));
  const region = args.region ?? (await sourceRegion(api, args.account_id, args.source_bucket));
  const read = new Map<CloneSetting, Read | BbError>();
  for (const setting of wanted) {
    try {
      read.set(setting, await reads[setting]());
    } catch (raw) {
      read.set(setting, bbError(raw));
    }
  }
  await api.create_bucket({ account_id: args.account_id, name: args.new_bucket, region });
  const results: CloneSettingResult[] = [];
  for (const setting of wanted) {
    const item = read.get(setting);
    if (item === undefined) {
      continue;
    }
    if ("code" in item) {
      results.push({ setting, state: "failed", error: item });
      continue;
    }
    if (!item.configured && setting !== "public_access_block") {
      results.push({ setting, state: "skipped", error: null });
      continue;
    }
    try {
      await item.apply(args.new_bucket);
      results.push({ setting, state: "applied", error: null });
    } catch (raw) {
      results.push({ setting, state: "failed", error: bbError(raw) });
    }
  }
  let tasks: TransferTask[] = [];
  let copyError: BbError | null = null;
  if (args.copy_objects) {
    try {
      tasks = await copyObjects(api, args);
    } catch (raw) {
      copyError = bbError(raw);
    }
  }
  return { outcome: { bucket: args.new_bucket, region, results }, tasks, copy_error: copyError };
}

async function copyObjects(api: CloneApi, args: { account_id: string; source_bucket: string; new_bucket: string; prefix: string }): Promise<TransferTask[]> {
  let keys: string[];
  let destination: string;
  if (args.prefix === "") {
    keys = [];
    let token: string | null = null;
    do {
      const page: Awaited<ReturnType<CloneApi["list_objects"]>> = await api.list_objects({
        account_id: args.account_id,
        bucket: args.source_bucket,
        prefix: "",
        flat: false,
        token,
      });
      keys.push(...page.entries.map((entry) => entry.key));
      token = page.next_token;
    } while (token !== null);
    destination = "";
  } else {
    keys = [args.prefix];
    const trimmed = args.prefix.slice(0, -1);
    destination = trimmed.includes("/") ? trimmed.slice(0, trimmed.lastIndexOf("/") + 1) : "";
  }
  if (keys.length === 0) {
    return [];
  }
  return api.enqueue_copy({
    account_id: args.account_id,
    src_bucket: args.source_bucket,
    keys,
    dst_bucket: args.new_bucket,
    dst_prefix: destination,
  });
}
