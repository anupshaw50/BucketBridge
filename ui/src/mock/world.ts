import type { Account, BucketKind, Checksums, ChecksumAlgorithm, TransferTask } from "../api/types";
import { MockFs } from "./fs";
import type { MockObjectMeta } from "./objectMeta";
import { GiB, KiB, MiB, seededBytes, toBase64, toHex, uuid } from "./util";

/** The cloud side of the mock: providers (AWS or custom endpoints), credentials and buckets. */
export interface MockObject {
  size: number;
  etag: string;
  last_modified: string;
  storage_class: string;
  checksums: Checksums;
  /** Tags, headers, encryption and ACL; objects without it have the defaults (see objectMeta.ts). */
  meta?: MockObjectMeta;
}

export interface MockBucket {
  name: string;
  region: string;
  creation_date: string;
  owner: string;
  kind: BucketKind;
  objects: Map<string, MockObject>;
  /** Owners of other credentials that may use the bucket although it is not theirs (it is not in their bucket list). */
  sharedWith?: readonly string[];
}

export interface Credential {
  secret: string;
  owner: string;
}

export interface Provider {
  credentials: Map<string, Credential>;
  buckets: Map<string, MockBucket>;
}

/** Provider key used for accounts without a custom endpoint. */
export const AWS_PROVIDER = "aws";

export const EXTERNAL_ENDPOINT = "https://storage.example.com";

/** The only MFA code the mock accepts. */
export const MOCK_MFA_CODE = "123456";

/** Credentials the mock cloud knows; documented in docs/ui.md. */
export const MOCK_CREDENTIALS = {
  demo: { accessKeyId: "MOCKDEMOACCESSKEY001", secret: "mock-demo-secret-0001" },
  external: { accessKeyId: "MOCKEXTACCESSKEY0002", secret: "mock-ext-secret-0002" },
  mfa: { accessKeyId: "MOCKMFAACCESSKEY0003", secret: "mock-mfa-secret-0003" },
  fresh: { accessKeyId: "MOCKNEWACCESSKEY0004", secret: "mock-new-secret-0004" },
} as const;

export const MOCK_HOME = "/home/demo";

export interface World {
  providers: Map<string, Provider>;
  accounts: Account[];
  defaultAccountId: string | null;
  secrets: Map<string, string>;
  fs: MockFs;
  tasks: TransferTask[];
}

export interface WorldOptions {
  fixture: "demo" | "empty";
  /** Number of files in the "Many files" local folder (demo fixture). */
  manyFiles: number;
  /** Number of objects in demo-logs/app/ (demo fixture), for pagination. */
  logObjects: number;
}

export function makeChecksums(seed: string, algorithm: ChecksumAlgorithm): Checksums {
  const checksums: Checksums = { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null };
  switch (algorithm) {
    case "crc32":
      checksums.crc32 = toBase64(seededBytes(`crc32:${seed}`, 4));
      break;
    case "crc32c":
      checksums.crc32c = toBase64(seededBytes(`crc32c:${seed}`, 4));
      break;
    case "crc64nvme":
      checksums.crc64nvme = toBase64(seededBytes(`crc64:${seed}`, 8));
      break;
    case "sha1":
      checksums.sha1 = toBase64(seededBytes(`sha1:${seed}`, 20));
      break;
    case "sha256":
      checksums.sha256 = toBase64(seededBytes(`sha256:${seed}`, 32));
      break;
    case "none":
      break;
  }
  return checksums;
}

export function makeObject(
  key: string,
  size: number,
  lastModified: string,
  algorithm: ChecksumAlgorithm = "crc32",
  storageClass = "STANDARD",
): MockObject {
  const seed = `${key}:${size}:${lastModified}`;
  return {
    size,
    etag: toHex(seededBytes(`etag:${seed}`, 16)),
    last_modified: lastModified,
    storage_class: storageClass,
    checksums: makeChecksums(seed, size === 0 ? "none" : algorithm),
  };
}

function bucket(name: string, region: string, owner: string, kind: BucketKind, created: string): MockBucket {
  return { name, region, creation_date: created, owner, kind, objects: new Map() };
}

function day(offsetDays: number, hour = 9, minute = 30): string {
  const base = Date.UTC(2026, 9, 20, hour, minute, 0);
  return new Date(base + offsetDays * 86_400_000).toISOString();
}

function newAccount(
  secrets: Map<string, string>,
  fields: Omit<Account, "id" | "secret_ref" | "session_token_ref">,
  secret: string,
): Account {
  const id = uuid();
  const secretRef = `bucketbridge/account/${id}/secret`;
  secrets.set(secretRef, secret);
  return { id, secret_ref: secretRef, session_token_ref: null, ...fields };
}

function emptyProviders(): Map<string, Provider> {
  const aws: Provider = { credentials: new Map(), buckets: new Map() };
  const external: Provider = { credentials: new Map(), buckets: new Map() };
  aws.credentials.set(MOCK_CREDENTIALS.demo.accessKeyId, { secret: MOCK_CREDENTIALS.demo.secret, owner: "demo" });
  aws.credentials.set(MOCK_CREDENTIALS.mfa.accessKeyId, { secret: MOCK_CREDENTIALS.mfa.secret, owner: "secure" });
  aws.credentials.set(MOCK_CREDENTIALS.fresh.accessKeyId, { secret: MOCK_CREDENTIALS.fresh.secret, owner: "fresh" });
  external.credentials.set(MOCK_CREDENTIALS.external.accessKeyId, {
    secret: MOCK_CREDENTIALS.external.secret,
    owner: "archive",
  });
  return new Map([
    [AWS_PROVIDER, aws],
    [EXTERNAL_ENDPOINT, external],
  ]);
}

function put(target: MockBucket, key: string, size: number, modified: string, storageClass = "STANDARD"): void {
  target.objects.set(key, makeObject(key, size, modified, "crc32", storageClass));
}

/** Builds the initial mock world. The "empty" fixture has no accounts, objects or tasks. */
export function buildWorld(options: WorldOptions): World {
  const providers = emptyProviders();
  const fs = new MockFs();
  const secrets = new Map<string, string>();
  fs.mkdirp(`${MOCK_HOME}/Downloads`);
  fs.mkdirp(`${MOCK_HOME}/Documents`);
  // Folders reported by app_info, so "show in folder" works in the simulated file system.
  fs.mkdirp(`${MOCK_HOME}/.config/bucketbridge`);
  fs.mkdirp(`${MOCK_HOME}/.local/share/bucketbridge/logs`);
  if (options.fixture === "empty") {
    fs.writeFile(`${MOCK_HOME}/Documents/hello.txt`, { size: 12, modified: day(0) });
    return { providers, accounts: [], defaultAccountId: null, secrets, fs, tasks: [] };
  }

  const aws = providers.get(AWS_PROVIDER);
  const external = providers.get(EXTERNAL_ENDPOINT);
  if (!aws || !external) {
    throw new Error("mock providers missing");
  }

  // ---- buckets and objects ----
  const photos = bucket("demo-photos", "us-east-1", "demo", "aws", day(-300));
  put(photos, "photos/2024/beach.jpg", 2_457_600, day(-30, 14, 5));
  put(photos, "photos/2024/sunset at sea.jpg", 3_145_728, day(-29));
  put(photos, "photos/2024/café.png", 845_120, day(-28));
  put(photos, "photos/2025/", 0, day(-20));
  put(photos, "photos/2025/winter/snow.jpg", 1_966_080, day(-19), "STANDARD_IA");
  put(photos, "documents/report-q3.pdf", 1_258_291, day(-10));
  put(photos, "documents/notes.txt", 4_096, day(-3));
  put(photos, "documents/budget 2026.xlsx", 52_224, day(-2));
  put(photos, "odd//double-slash.txt", 120, day(-5));
  put(photos, "space folder/hello world.txt", 2_048, day(-4));
  put(photos, "emoji \u{1F4F7}/pic.jpg", 512_000, day(-6));
  put(photos, "archive/", 0, day(-60));
  put(photos, "readme.md", 1_024, day(-1));

  const backups = bucket("demo-backups", "eu-west-1", "demo", "aws", day(-200));
  put(backups, "db/2026-10-26.sql.gz", 734_003_200, day(6, 2, 0), "STANDARD_IA");
  put(backups, "db/2026-10-27.sql.gz", 736_100_352, day(7, 2, 0), "STANDARD_IA");
  put(backups, "db/2026-10-28.sql.gz", 738_197_504, day(8, 2, 0), "STANDARD_IA");
  put(backups, "documents/notes.txt", 4_096, day(-3));
  put(backups, "archive/2019.tar", 4 * GiB, day(-400), "GLACIER");

  const website = bucket("demo-website", "us-east-1", "demo", "cloudfront", day(-150));
  put(website, "index.html", 6 * KiB, day(-7));
  put(website, "css/site.css", 18 * KiB, day(-7));
  put(website, "js/app.js", 140 * KiB, day(-7));
  put(website, "img/logo.svg", 3 * KiB, day(-90));

  const logs = bucket("demo-logs", "us-west-2", "demo", "aws", day(-120));
  for (let index = 1; index <= options.logObjects; index += 1) {
    const name = `app/app-${index.toString().padStart(5, "0")}.log`;
    put(logs, name, 8 * KiB + (index % 97) * 131, day(-(index % 30), index % 24, index % 60));
  }
  put(logs, "latest.log", 12 * KiB, day(8));

  const empty = bucket("demo-empty", "us-east-1", "demo", "aws", day(-12));

  // A bucket of another account that the demo user may use but not list: add it with "Add external bucket".
  const exchange = bucket("partner-exchange", "us-east-1", "partner", "aws", day(-90));
  exchange.sharedWith = ["demo"];
  put(exchange, "inbox/orders-2026-10.csv", 48 * KiB, day(-1));
  put(exchange, "inbox/README.txt", 700, day(-60));

  const vault = bucket("secure-vault", "us-east-1", "secure", "aws", day(-500));
  put(vault, "keys/README.txt", 900, day(-100));
  put(vault, "contracts/2026/agreement.pdf", 2 * MiB, day(-15));

  const archive = bucket("archive-2025", "eu-central-1", "archive", "external", day(-250));
  put(archive, "media/interview.mp4", 820 * MiB, day(-70));
  put(archive, "media/transcript.txt", 64 * KiB, day(-69));
  const media = bucket("archive-media", "eu-central-1", "archive", "external", day(-240));
  put(media, "covers/cover-01.png", 1_400 * KiB, day(-44));

  for (const item of [photos, backups, website, logs, empty, vault, exchange]) {
    aws.buckets.set(item.name, item);
  }
  for (const item of [archive, media]) {
    external.buckets.set(item.name, item);
  }

  // ---- local files ----
  fs.writeFile(`${MOCK_HOME}/Documents/report.pdf`, { size: 1_258_291, modified: day(-11) });
  fs.writeFile(`${MOCK_HOME}/Documents/budget 2026.xlsx`, { size: 52_224, modified: day(-2) });
  fs.writeFile(`${MOCK_HOME}/Documents/notes/todo.txt`, { size: 2_048, modified: day(-1) });
  fs.writeFile(`${MOCK_HOME}/Documents/notes/ideas.md`, { size: 6_144, modified: day(-1) });
  fs.writeFile(`${MOCK_HOME}/Pictures/holiday/beach.jpg`, { size: 2_457_600, modified: day(-30, 14, 5) });
  fs.writeFile(`${MOCK_HOME}/Pictures/holiday/new-photo.jpg`, { size: 1_048_576, modified: day(1) });
  fs.writeFile(`${MOCK_HOME}/Pictures/holiday/unreadable-raw.cr2`, { size: 24 * MiB, modified: day(1) });
  fs.writeFile(`${MOCK_HOME}/Pictures/holiday/flaky-video.mp4`, { size: 12 * MiB, modified: day(2) });
  fs.mkdirp(`${MOCK_HOME}/Pictures/empty folder`);
  fs.writeFile(`${MOCK_HOME}/Videos/big-video.mkv`, { size: 1_536 * MiB, modified: day(-40) });
  fs.writeFile(`${MOCK_HOME}/Projects/site/index.html`, { size: 6 * KiB, modified: day(3) });
  fs.writeFile(`${MOCK_HOME}/Projects/site/css/site.css`, { size: 18 * KiB, modified: day(3) });
  for (let index = 1; index <= options.manyFiles; index += 1) {
    fs.writeFile(`${MOCK_HOME}/Many files/file-${index.toString().padStart(5, "0")}.txt`, {
      size: 1_024 + (index % 13) * 256,
      modified: day(-(index % 20)),
    });
  }

  // ---- accounts ----
  const demo = newAccount(
    secrets,
    {
      label: "Demo",
      access_key_id: MOCK_CREDENTIALS.demo.accessKeyId,
      region: "us-east-1",
      endpoint: null,
      path_style: false,
      request_payer: false,
      mfa_serial: null,
    },
    MOCK_CREDENTIALS.demo.secret,
  );
  const archiveAccount = newAccount(
    secrets,
    {
      label: "Archive (custom endpoint)",
      access_key_id: MOCK_CREDENTIALS.external.accessKeyId,
      region: "eu-central-1",
      endpoint: EXTERNAL_ENDPOINT,
      path_style: true,
      request_payer: false,
      mfa_serial: null,
    },
    MOCK_CREDENTIALS.external.secret,
  );
  const secure = newAccount(
    secrets,
    {
      label: "Secure (MFA)",
      access_key_id: MOCK_CREDENTIALS.mfa.accessKeyId,
      region: "us-east-1",
      endpoint: null,
      path_style: false,
      request_payer: false,
      mfa_serial: "arn:aws:iam::000000000000:mfa/demo-device",
    },
    MOCK_CREDENTIALS.mfa.secret,
  );
  const broken = newAccount(
    secrets,
    {
      label: "Wrong secret",
      access_key_id: MOCK_CREDENTIALS.demo.accessKeyId,
      region: "us-east-1",
      endpoint: null,
      path_style: false,
      request_payer: false,
      mfa_serial: null,
    },
    "not-the-right-secret",
  );

  // ---- transfer tasks restored from a previous session ----
  const created = day(8, 10, 0);
  const tasks: TransferTask[] = [
    {
      id: uuid(),
      account_id: demo.id,
      group_id: null,
      kind: "download",
      src: { type: "s3", bucket: "demo-photos", key: "photos/2024/sunset at sea.jpg" },
      dst: { type: "local", path: `${MOCK_HOME}/Downloads/sunset at sea.jpg` },
      bytes_total: 3_145_728,
      bytes_done: 1_258_291,
      speed: 0,
      eta: null,
      state: "stopped",
      error: null,
      attempts: 1,
      created_at: created,
      updated_at: created,
    },
    {
      id: uuid(),
      account_id: demo.id,
      group_id: null,
      kind: "upload",
      src: { type: "local", path: `${MOCK_HOME}/Pictures/holiday/unreadable-raw.cr2` },
      dst: { type: "s3", bucket: "demo-backups", key: "raw/unreadable-raw.cr2" },
      bytes_total: 24 * MiB,
      bytes_done: 0,
      speed: 0,
      eta: null,
      state: "failed",
      error: "Permission denied (os error 13)",
      attempts: 1,
      created_at: created,
      updated_at: created,
    },
    {
      id: uuid(),
      account_id: demo.id,
      group_id: null,
      kind: "copy",
      src: { type: "s3", bucket: "demo-photos", key: "documents/notes.txt" },
      dst: { type: "s3", bucket: "demo-backups", key: "documents/notes.txt" },
      bytes_total: 4_096,
      bytes_done: 4_096,
      speed: 0,
      eta: null,
      state: "completed",
      error: null,
      attempts: 1,
      created_at: created,
      updated_at: created,
    },
  ];

  return {
    providers,
    accounts: [demo, archiveAccount, secure, broken],
    defaultAccountId: demo.id,
    secrets,
    fs,
    tasks,
  };
}
