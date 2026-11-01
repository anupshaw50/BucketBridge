// The tools and export commands of the mock backend (docs/desktop-api.md, "Tools and export (T12)"):
// diagnostics, uncompleted multipart uploads, saved filters and the export of a listing. The state
// and the rules live here; MockBackend only delegates, after its own latency and bookkeeping.

import type { CommandArgs } from "../api/backend";
import { BackendError } from "../api/errors";
import type {
  AbortResult,
  Account,
  AccountInput,
  BbErrorCode,
  BucketFailure,
  DiagnosticStatus,
  DiagnosticStep,
  DiagnosticStepId,
  DiagnosticsReport,
  ExportOutcome,
  ListingFormat,
  LogLevel,
  ObjectEntry,
  SavedFilter,
  Settings,
  UploadListing,
  UploadRow,
} from "../api/types";
import { compileFilter, matchesObject } from "../lib/filters";
import { utf8Length } from "../lib/s3keys";
import { validateBucketName, validateEndpoint, validateRegion } from "../lib/validation";
import { dirname, type MockFs, normalizePath } from "./fs";
import { compareKeys, isoMinusDays, nowIso, seededBytes, toHex } from "./util";
import { AWS_PROVIDER, EXTERNAL_ENDPOINT, type MockBucket, type MockObject, type Provider } from "./world";

/** What the tools need from the rest of the mock backend. */
export interface ToolsHost {
  readonly fs: MockFs;
  settings(): Settings;
  /** The stored account; `not_found` for an unknown id. */
  account(id: string): Account;
  /** Rejects with `locked` or `mfa_required`, like every account-bound command. */
  requireAccess(account: Account): void;
  /** The server behind an endpoint (`null` is AWS), or `undefined` when nothing answers there. */
  provider(endpoint: string | null): Provider | undefined;
  /** The secret access key stored for an account. */
  storedSecret(account: Account): string | undefined;
  /** The bucket as the account sees it, with every rule of a real call (credentials, ownership); throws. */
  bucket(accountId: string, name: string): MockBucket;
  log(level: LogLevel, source: string, message: string): void;
  /** Waits for simulated time (nothing happens in tests, where the latency is 0). */
  simulate(ms: number): Promise<void>;
}

const MAX_FILTERS = 200;
const MAX_FILTER_NAME = 100;
const DAY_MS = 86_400_000;
const MOCK_CA = "Mock Trust Services CA";

/** Buckets whose uploads the demo account may not list (a bucket policy denies it). */
const LISTING_DENIED: ReadonlySet<string> = new Set(["demo-website"]);

interface UploadSeed {
  provider: string;
  bucket: string;
  key: string;
  ageDays: number;
  storageClass: string;
}

/** In-progress uploads of the demo world; their ages are relative to when the mock starts. */
const UPLOAD_SEEDS: readonly UploadSeed[] = [
  { provider: AWS_PROVIDER, bucket: "demo-backups", key: "db/2026-10-29.sql.gz", ageDays: 2, storageClass: "STANDARD_IA" },
  { provider: AWS_PROVIDER, bucket: "demo-backups", key: "archive/2019.tar", ageDays: 45, storageClass: "STANDARD" },
  { provider: AWS_PROVIDER, bucket: "demo-backups", key: "archive/2018.tar", ageDays: 100, storageClass: "STANDARD" },
  { provider: AWS_PROVIDER, bucket: "demo-photos", key: "photos/2025/winter/video.mov", ageDays: 12, storageClass: "STANDARD" },
  { provider: AWS_PROVIDER, bucket: "demo-photos", key: "photos/2024/raw/IMG_0001.cr2", ageDays: 200, storageClass: "STANDARD" },
  { provider: AWS_PROVIDER, bucket: "demo-logs", key: "app/app-00099.log", ageDays: 31, storageClass: "STANDARD" },
  { provider: AWS_PROVIDER, bucket: "demo-website", key: "img/hero.mp4", ageDays: 9, storageClass: "STANDARD" },
  { provider: EXTERNAL_ENDPOINT, bucket: "archive-2025", key: "media/interview-raw.mp4", ageDays: 60, storageClass: "STANDARD" },
];

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

function isIpAddress(host: string): boolean {
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.startsWith("[");
}

/** A stable pseudo-random duration, so that the reports of one setup do not change between runs. */
function stableMs(seed: string, min: number, spread: number): number {
  const bytes = seededBytes(seed, 2);
  return min + ((((bytes[0] ?? 0) << 8) | (bytes[1] ?? 0)) % spread);
}

function addressOf(host: string, index: number): string {
  const bytes = seededBytes(`${host}:${index}`, 2);
  return `203.0.113.${(bytes[0] ?? 0) % 250 + 1}`;
}

/** Whether `host` is on the proxy bypass list (exact names, `*.suffix`, `localhost` and `*`). */
function bypassed(host: string, entries: readonly string[]): boolean {
  const name = host.toLowerCase();
  return entries.some((entry) => {
    const pattern = entry.trim().toLowerCase();
    if (pattern === "*" || pattern === name) {
      return true;
    }
    if (pattern === "localhost") {
      return name === "localhost" || name.endsWith(".localhost") || name.startsWith("127.");
    }
    const suffix = pattern.startsWith("*.") ? pattern.slice(1) : pattern.startsWith(".") ? pattern : null;
    return suffix !== null && name.endsWith(suffix);
  });
}

function step(
  id: DiagnosticStepId,
  status: DiagnosticStatus,
  elapsed: number,
  code: string,
  summary: string,
  detail: string | null = null,
): DiagnosticStep {
  return { id, status, elapsed_ms: status === "skip" ? 0 : elapsed, summary, detail, code };
}

function objectRow(key: string, object: MockObject): ObjectEntry {
  return {
    key,
    size: object.size,
    etag: object.etag,
    last_modified: object.last_modified,
    storage_class: object.storage_class,
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: false,
  };
}

function folderRow(key: string): ObjectEntry {
  return {
    key,
    size: 0,
    etag: null,
    last_modified: null,
    storage_class: null,
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: true,
  };
}

/** A text field of a CSV file: spreadsheet formulas are defused with a leading apostrophe, then quoted if needed. */
export function csvField(value: string): string {
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return /[",\r\n]/.test(safe) ? `"${safe.replace(/"/g, '""')}"` : safe;
}

/** The CSV text the real backend writes for these rows (the mock only needs its size). */
export function csvText(rows: readonly ObjectEntry[]): string {
  const lines = ["key,size,last_modified,etag,storage_class"];
  for (const row of rows) {
    lines.push(
      [
        csvField(row.key),
        row.is_prefix ? "" : String(row.size),
        row.last_modified === null ? "" : row.last_modified.replace(".000Z", "Z"),
        csvField(row.etag ?? ""),
        csvField(row.storage_class ?? ""),
      ].join(","),
    );
  }
  return `${lines.join("\n")}\n`;
}

const FORMAT_OVERHEAD: Record<Exclude<ListingFormat, "csv">, { base: number; perRow: number }> = {
  xlsx: { base: 4_096, perRow: 64 },
  parquet: { base: 1_024, perRow: 48 },
};

interface Credentials {
  access_key_id: string;
  secret_access_key: string;
}

/** State and rules of the tools and export commands. */
export class ToolsMock {
  private readonly host: ToolsHost;
  private filters: SavedFilter[] = [];
  private readonly uploads = new Map<string, UploadRow[]>();

  constructor(host: ToolsHost) {
    this.host = host;
    for (const seed of UPLOAD_SEEDS) {
      const endpoint = seed.provider === AWS_PROVIDER ? null : seed.provider;
      if (this.host.provider(endpoint)?.buckets.has(seed.bucket) !== true) {
        continue;
      }
      const list = this.uploadsOf(seed.provider, seed.bucket);
      list.push({
        bucket: seed.bucket,
        key: seed.key,
        upload_id: toHex(seededBytes(`upload:${seed.bucket}/${seed.key}`, 16)),
        initiated: isoMinusDays(seed.ageDays),
        storage_class: seed.storageClass,
        initiator: "demo-owner",
      });
    }
  }

  // ------------------------------------------------------------ diagnostics

  async runDiagnostics(args: CommandArgs["run_diagnostics"]): Promise<DiagnosticsReport> {
    const { account_id: accountId, input } = args;
    if (args.bucket !== null) {
      const problem = validateBucketName(args.bucket);
      if (problem !== null) {
        fail("usage", problem);
      }
    }
    let probe: Probe;
    if (accountId !== null && input === null) {
      const account = this.host.account(accountId);
      this.host.requireAccess(account);
      probe = {
        endpoint: account.endpoint,
        region: account.region,
        credentials: { access_key_id: account.access_key_id, secret_access_key: this.host.storedSecret(account) ?? "" },
      };
    } else if (accountId === null && input !== null) {
      probe = this.probeOfInput(input);
    } else {
      fail("usage", "Give either a stored account or an unsaved account to probe, not both and not neither.");
    }
    const report = this.probe(probe, args.bucket);
    await this.host.simulate(Math.min(report.elapsed_ms, 800));
    const failed = report.steps.filter((item) => item.status === "fail").length;
    this.host.log(
      report.ok ? "information" : "warning",
      "diagnostics",
      report.ok ? "Diagnostics finished: every check passed." : `Diagnostics finished: ${failed} check(s) failed.`,
    );
    return report;
  }

  private probeOfInput(input: AccountInput): Probe {
    const regionProblem = validateRegion(input.region);
    if (regionProblem !== null) {
      fail("usage", regionProblem);
    }
    if (input.endpoint !== null) {
      const endpointProblem = validateEndpoint(input.endpoint);
      if (endpointProblem !== null) {
        fail("usage", endpointProblem);
      }
    }
    if (input.access_key_id.trim().length === 0) {
      fail("usage", "The access key ID is empty.");
    }
    if (input.secret_access_key.length === 0) {
      fail("usage", "The secret access key is empty.");
    }
    return {
      endpoint: input.endpoint,
      region: input.region,
      credentials: { access_key_id: input.access_key_id, secret_access_key: input.secret_access_key },
    };
  }

  private probe(probe: Probe, bucket: string | null): DiagnosticsReport {
    const url = new URL(probe.endpoint ?? `https://s3.${probe.region}.amazonaws.com`);
    const secure = url.protocol === "https:";
    const host = url.hostname;
    const port = url.port === "" ? (secure ? 443 : 80) : Number(url.port);
    const settings = this.host.settings();
    const proxied = settings.proxy.enabled && settings.proxy.host !== "" && !bypassed(host, settings.proxy.bypass);
    const server = this.host.provider(probe.endpoint);
    const target = `${url.origin}${url.pathname === "/" ? "" : url.pathname.replace(/\/$/, "")}`;
    const steps: DiagnosticStep[] = [];
    let blocked = false;
    const skipBlocked = (id: DiagnosticStepId, what: string): DiagnosticStep =>
      step(id, "skip", 0, "blocked", `Skipped: ${what} did not work.`);

    // DNS and TCP concern the proxy when one carries the traffic, the endpoint otherwise.
    const peerHost = proxied ? settings.proxy.host : host;
    const peerPort = proxied ? settings.proxy.port : port;
    const peerKnown = proxied || server !== undefined;
    if (isIpAddress(peerHost)) {
      steps.push(step("dns", "skip", 0, "ip_address", `${peerHost} is an IP address, so no lookup is needed.`));
    } else if (!peerKnown) {
      steps.push(
        step("dns", "fail", stableMs(`dns:${peerHost}`, 20, 40), "not_found", `${peerHost} could not be resolved.`, "Check the endpoint for typos and that the computer can reach its DNS server."),
      );
      blocked = true;
    } else {
      const count = 1 + stableMs(`count:${peerHost}`, 0, 2);
      const addresses = Array.from({ length: count }, (_, index) => addressOf(peerHost, index));
      steps.push(
        step(
          "dns",
          "pass",
          stableMs(`dns:${peerHost}`, 6, 24),
          "resolved",
          `${peerHost} resolves to ${count === 1 ? "1 address" : `${count} addresses`}${proxied ? " (the proxy host)" : ""}.`,
          addresses.join(", "),
        ),
      );
    }
    if (blocked) {
      steps.push(skipBlocked("tcp", "the DNS lookup"));
    } else if (!peerKnown) {
      steps.push(
        step("tcp", "fail", stableMs(`tcp:${peerHost}`, 2, 6), "refused", `The connection to ${peerHost}:${peerPort} was refused.`, "Nothing is listening on that address and port: check the endpoint and that the server is running."),
      );
      blocked = true;
    } else {
      steps.push(
        step("tcp", "pass", stableMs(`tcp:${peerHost}`, 12, 40), "connected", `Connected to ${isIpAddress(peerHost) ? peerHost : addressOf(peerHost, 0)}:${peerPort}${proxied ? " (the proxy)" : ""}.`),
      );
    }

    // The proxy step.
    if (blocked) {
      steps.push(skipBlocked("proxy", "the connection"));
    } else if (!proxied) {
      const configured = settings.proxy.enabled && settings.proxy.host !== "";
      steps.push(
        configured
          ? step("proxy", "skip", 0, "direct", `${host} is on the bypass list, so the proxy is not used.`)
          : step("proxy", "skip", 0, "none", "No proxy is configured."),
      );
    } else if (server === undefined) {
      steps.push(
        step("proxy", "fail", stableMs(`proxy:${host}`, 20, 30), "rejected", `The proxy could not connect to ${host}:${port} (HTTP 502).`, "The proxy cannot reach the endpoint: check the endpoint and the proxy rules."),
      );
      blocked = true;
    } else if (secure) {
      steps.push(step("proxy", "pass", stableMs(`proxy:${host}`, 10, 30), "tunnel_open", `The ${settings.proxy.kind === "socks5" ? "SOCKS5" : "HTTP"} proxy opened a tunnel to ${host}:${port}.`));
    } else {
      steps.push(step("proxy", "pass", stableMs(`proxy:${host}`, 4, 10), "forwarded", "The proxy forwards requests to this plain-HTTP endpoint."));
    }

    // TLS.
    if (blocked) {
      steps.push(skipBlocked("tls", "the connection"));
    } else if (!secure) {
      steps.push(step("tls", "skip", 0, "plain_http", "The endpoint uses plain HTTP, so there is no TLS handshake."));
    } else {
      const expires = new Date(Date.now() + 200 * DAY_MS).toISOString().slice(0, 10);
      steps.push(
        step(
          "tls",
          "pass",
          stableMs(`tls:${host}`, 25, 50),
          "handshake",
          `TLS 1.3 handshake with ${host} succeeded.`,
          `Certificate subject: CN=${host}\nIssuer: CN=${MOCK_CA}\nExpires: ${expires} (in 200 days)`,
        ),
      );
    }

    // Credentials.
    let accepted = false;
    let owner: string | null = null;
    if (blocked || server === undefined) {
      steps.push(skipBlocked("credentials", "the connection"));
    } else {
      const credential = server.credentials.get(probe.credentials.access_key_id);
      const elapsed = stableMs(`credentials:${host}`, 90, 220);
      if (credential === undefined) {
        steps.push(
          step("credentials", "fail", elapsed, "invalid_access_key", "The server does not know this access key ID (InvalidAccessKeyId).", "Check the access key ID for typos, and that the key exists on this service and was not deleted."),
        );
      } else if (credential.secret !== probe.credentials.secret_access_key) {
        steps.push(
          step("credentials", "fail", elapsed, "bad_signature", "The server rejected the signature (SignatureDoesNotMatch).", "The secret access key is probably wrong: check it for typos and trailing spaces. A wrong region can cause this too."),
        );
      } else {
        accepted = true;
        owner = credential.owner;
        steps.push(step("credentials", "pass", elapsed, "valid", "The server accepted a signed ListBuckets request.", "Clock difference to the server: 1 s."));
      }
    }

    // Region of the named bucket.
    if (bucket === null) {
      steps.push(step("region", "skip", 0, "no_bucket", "No bucket was named, so the region was not probed."));
    } else if (blocked || server === undefined) {
      steps.push(skipBlocked("region", "the connection"));
    } else {
      const found = server.buckets.get(bucket);
      const elapsed = stableMs(`region:${bucket}`, 40, 120);
      const custom = probe.endpoint !== null;
      if (!accepted || (found !== undefined && found.owner !== owner)) {
        steps.push(
          step(
            "region",
            "fail",
            elapsed,
            "access_denied",
            `Access to the bucket ${bucket} was denied (HTTP 403).`,
            found !== undefined && !custom ? `The bucket exists in ${found.region}.` : null,
          ),
        );
      } else if (found === undefined) {
        steps.push(step("region", "fail", elapsed, "bucket_not_found", `The bucket ${bucket} does not exist (NoSuchBucket).`, "Check the bucket name, and that it lives on this endpoint."));
      } else if (custom) {
        steps.push(
          step("region", "pass", elapsed, "no_region_header", `The bucket ${bucket} exists and is accessible. This endpoint does not report regions, so the account region ${probe.region} is used.`),
        );
      } else {
        steps.push(
          step(
            "region",
            "pass",
            elapsed,
            "found",
            `The bucket ${bucket} is in ${found.region}.`,
            found.region === probe.region ? null : `The account region is ${probe.region}; requests for this bucket are sent to ${found.region} automatically.`,
          ),
        );
      }
    }

    const elapsed = steps.reduce((sum, item) => sum + item.elapsed_ms, 0) + 3;
    return {
      target,
      region: probe.region,
      bucket,
      started_at: nowIso(),
      elapsed_ms: elapsed,
      ok: steps.every((item) => item.status !== "fail"),
      steps,
    };
  }

  // ------------------------------------------------------------ multipart uploads

  listUploads(args: CommandArgs["list_multipart_uploads"]): UploadListing {
    const account = this.host.account(args.account_id);
    this.host.requireAccess(account);
    if (args.buckets.length === 0) {
      fail("usage", "Choose at least one bucket.");
    }
    for (const name of args.buckets) {
      const problem = validateBucketName(name);
      if (problem !== null) {
        fail("usage", `${name}: ${problem}`);
      }
    }
    const providerKey = account.endpoint ?? AWS_PROVIDER;
    const uploads: UploadRow[] = [];
    const failures: BucketFailure[] = [];
    for (const name of new Set(args.buckets)) {
      const denied = this.denial(account, name);
      if (denied !== null) {
        failures.push({ bucket: name, error: denied });
        continue;
      }
      uploads.push(...this.uploadsOf(providerKey, name).map((row) => ({ ...row })));
    }
    uploads.sort(
      (a, b) =>
        compareKeys(a.bucket, b.bucket) || compareKeys(a.key, b.key) || compareKeys(a.initiated ?? "", b.initiated ?? ""),
    );
    return { uploads, failures };
  }

  abortUploads(args: CommandArgs["abort_multipart_uploads"]): AbortResult[] {
    const account = this.host.account(args.account_id);
    this.host.requireAccess(account);
    if (args.uploads.length === 0) {
      fail("usage", "Select at least one upload.");
    }
    for (const ref of args.uploads) {
      const problem = validateBucketName(ref.bucket);
      if (problem !== null) {
        fail("usage", `${ref.bucket}: ${problem}`);
      }
      if (ref.key.length === 0 || utf8Length(ref.key) > 1024) {
        fail("usage", "An upload key must be 1 to 1024 bytes long.");
      }
      if (ref.upload_id.length === 0) {
        fail("usage", "The upload ID is empty.");
      }
    }
    const providerKey = account.endpoint ?? AWS_PROVIDER;
    const results: AbortResult[] = [];
    for (const ref of args.uploads) {
      const result: AbortResult = { bucket: ref.bucket, key: ref.key, upload_id: ref.upload_id, aborted: false, error: null };
      const denied = this.denial(account, ref.bucket, false);
      if (denied !== null) {
        result.error = denied;
      } else {
        const list = this.uploadsOf(providerKey, ref.bucket);
        const index = list.findIndex((row) => row.key === ref.key && row.upload_id === ref.upload_id);
        if (index === -1) {
          result.error = { code: "not_found", message: "The specified upload does not exist (NoSuchUpload)." };
        } else {
          list.splice(index, 1);
          result.aborted = true;
          this.host.log("information", "s3", `Aborted upload ${ref.upload_id} of s3://${ref.bucket}/${ref.key}.`);
        }
      }
      results.push(result);
    }
    return results;
  }

  private uploadsOf(providerKey: string, bucket: string): UploadRow[] {
    const id = `${providerKey}\u0000${bucket}`;
    let list = this.uploads.get(id);
    if (list === undefined) {
      list = [];
      this.uploads.set(id, list);
    }
    return list;
  }

  /** Why the account cannot reach the uploads of a bucket, or `null`. `policy` adds the bucket policy that hides listings. */
  private denial(account: Account, name: string, policy = true): { code: BbErrorCode; message: string } | null {
    const server = this.host.provider(account.endpoint);
    if (server === undefined) {
      return { code: "remote", message: `Could not connect to ${account.endpoint ?? "the endpoint"}: connection refused.` };
    }
    const credential = server.credentials.get(account.access_key_id);
    if (credential === undefined) {
      return { code: "auth", message: "The access key ID does not exist (InvalidAccessKeyId)." };
    }
    if (this.host.storedSecret(account) !== credential.secret) {
      return { code: "auth", message: "The request signature does not match. Check the secret access key (SignatureDoesNotMatch)." };
    }
    const found = server.buckets.get(name);
    if (found === undefined) {
      return { code: "not_found", message: `The bucket ${name} does not exist (NoSuchBucket).` };
    }
    if (found.owner !== credential.owner || (policy && LISTING_DENIED.has(name))) {
      return { code: "auth", message: `Access to ${name} was denied (AccessDenied).` };
    }
    return null;
  }

  // ------------------------------------------------------------ saved filters

  listFilters(): SavedFilter[] {
    return structuredClone(this.filters);
  }

  saveFilter(args: CommandArgs["save_filter"]): SavedFilter[] {
    const name = args.filter.name.trim();
    if (name.length === 0 || Array.from(name).length > MAX_FILTER_NAME) {
      fail("usage", `A filter name has 1 to ${MAX_FILTER_NAME} characters.`);
    }
    if (/\p{Cc}/u.test(name)) {
      fail("usage", "A filter name cannot contain control characters.");
    }
    const compiled = compileFilter(args.filter.filter);
    if (!compiled.ok) {
      fail("usage", compiled.message);
    }
    const stored: SavedFilter = { name, filter: structuredClone(args.filter.filter) };
    const same = (left: string, right: string) => left.toLowerCase() === right.toLowerCase();
    if (args.replaces === null) {
      if (this.filters.some((item) => same(item.name, name))) {
        fail("conflict", `A saved filter named "${name}" already exists.`);
      }
      if (this.filters.length >= MAX_FILTERS) {
        fail("conflict", `At most ${MAX_FILTERS} filters can be saved.`);
      }
      this.filters.push(stored);
    } else {
      const replaced = args.replaces;
      const index = this.filters.findIndex((item) => same(item.name, replaced));
      if (index === -1) {
        fail("not_found", `There is no saved filter named "${replaced}".`);
      }
      if (this.filters.some((item, position) => position !== index && same(item.name, name))) {
        fail("conflict", `A saved filter named "${name}" already exists.`);
      }
      this.filters[index] = stored;
    }
    this.host.log("information", "filters", `Saved filter "${name}" stored.`);
    return this.listFilters();
  }

  deleteFilter(args: CommandArgs["delete_filter"]): SavedFilter[] {
    const index = this.filters.findIndex((item) => item.name.toLowerCase() === args.name.toLowerCase());
    if (index === -1) {
      fail("not_found", `There is no saved filter named "${args.name}".`);
    }
    this.filters.splice(index, 1);
    this.host.log("information", "filters", `Saved filter "${args.name}" deleted.`);
    return this.listFilters();
  }

  // ------------------------------------------------------------ export

  exportListing(args: CommandArgs["export_listing"]): ExportOutcome {
    const path = normalizePath(args.path);
    if (path === null) {
      fail("usage", "The export file must be an absolute path.");
    }
    let accepts: (row: ObjectEntry) => boolean = () => true;
    if (args.filter !== null) {
      const compiled = compileFilter(args.filter);
      if (!compiled.ok) {
        fail("usage", compiled.message);
      }
      const filter = compiled.filter;
      accepts = (row) => matchesObject(filter, row, args.prefix);
    }
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const rows = this.rowsOf(bucket, args.prefix, args.recursive).filter(accepts);
    if (this.host.fs.isDir(path)) {
      fail("io", `cannot write ${path}: it is a directory`);
    }
    if (!this.host.fs.isDir(dirname(path))) {
      fail("io", `cannot create ${path}: no such folder`);
    }
    let bytes: number;
    if (args.format === "csv") {
      bytes = new TextEncoder().encode(csvText(rows)).length;
    } else {
      const overhead = FORMAT_OVERHEAD[args.format];
      bytes = overhead.base + rows.length * overhead.perRow;
    }
    this.host.fs.writeFile(path, { size: bytes, modified: nowIso() });
    this.host.log(
      "information",
      "app",
      `Exported ${rows.length} row(s) of s3://${args.bucket}/${args.prefix} to ${path} as ${args.format}.`,
    );
    return { path, rows: rows.length, bytes };
  }

  /** The rows of a listing: every object below the prefix, or what a folder view shows (folders first, one level). */
  private rowsOf(bucket: MockBucket, prefix: string, recursive: boolean): ObjectEntry[] {
    const keys = [...bucket.objects.keys()].filter((key) => key.startsWith(prefix)).sort(compareKeys);
    if (recursive) {
      return keys.flatMap((key) => {
        const object = bucket.objects.get(key);
        return object === undefined ? [] : [objectRow(key, object)];
      });
    }
    const folders: ObjectEntry[] = [];
    const seen = new Set<string>();
    const objects: ObjectEntry[] = [];
    for (const key of keys) {
      if (key === prefix) {
        continue;
      }
      const slash = key.indexOf("/", prefix.length);
      if (slash === -1) {
        const object = bucket.objects.get(key);
        if (object !== undefined) {
          objects.push(objectRow(key, object));
        }
      } else {
        const folder = key.slice(0, slash + 1);
        if (!seen.has(folder)) {
          seen.add(folder);
          folders.push(folderRow(folder));
        }
      }
    }
    return [...folders, ...objects];
  }
}

interface Probe {
  endpoint: string | null;
  region: string;
  credentials: Credentials;
}
