import type { Backend, BackendEventName, BackendEvents, CommandArgs, Unsubscribe } from "../api/backend";
import { BackendError } from "../api/errors";
import { bucketUrl } from "../lib/bucketUrl";
import { MockCloudFront } from "./cloudfront";
import { validatePaths } from "../features/cloudfront/paths";
import type {
  ConflictEvent,
  IfExists,
  OverwriteChoice,
  PendingConflict,
  CloudFrontDistribution,
  CloudFrontInvalidation,
  CloudFrontPage,
  CreatedDistribution,
  InvalidationPage,
  PathSet,
  AccelerationStatus,
  AbortResult,
  BucketUsage,
  ExportAccountsOutcome,
  ImportReport,
  Account,
  AwsProfileEntry,
  CredentialSource,
  ImportOutcome,
  InstanceRoleProbe,
  SsoAccount,
  SsoLoginPoll,
  SsoLoginStarted,
  SsoSignInStatus,
  BucketRules,
  CorsConfig,
  LifecycleConfig,
  ReplicationConfig,
  RuleIssue,
  Bookmark,
  WindowLocation,
  LinkAction,
  QuitMode,
  QuitRequest,
  TieringConfig,
  AccountList,
  AppInfo,
  BbErrorCode,
  Bucket,
  BucketConfig,
  BucketVersioning,
  CrashReport,
  LoggingTarget,
  DiagnosticsReport,
  ExportOutcome,
  EditResolved,
  DragIncoming,
  EditSession,
  DeleteOutcome,
  LogEntry,
  LogLevel,
  ObjectEntry,
  ObjectPage,
  PolicyIssue,
  PublicAccessBlock,
  SavedFilter,
  ObjectPreview,
  RestoreOutcome,
  SecurityStatus,
  Settings,
  SyncJob,
  SyncPlan,
  SyncRow,
  TransferKind,
  TransferTask,
  WebsiteConfig,
  UploadListing,
  MountInfo,
  MountStatus,
  ScanReport,
  VersionGroupPage,
} from "../api/types";
import { ObjectLockMock } from "./objectLock";
import { parseTimestamp } from "../lib/format";
import { baseName, collidingNames, destinationKey, isFolderKey, parentPrefix, utf8Length } from "../lib/s3keys";
import { cloneSettings, DEFAULT_SETTINGS, hasErrors, validateSettings } from "../lib/settings";
import { formatPolicy, issueText } from "../lib/policy";
import { validateBucketName, validateEndpoint, validateRegion } from "../lib/validation";
import { corsIssues, lifecycleIssues, MockBucketRules, replicationIssues, tieringIssues } from "./bucketRules";
import { type BucketSlot, checkExistingBucketName, MockBucketConfigs } from "./bucketConfig";
import { basename, joinPath, type MockFile, type MockFs, normalizePath } from "./fs";
import { checkUserPolicy, IamMock } from "./iamMock";
import { reportBytes, scanReport } from "./scanMock";
import { ToolsMock } from "./toolsMock";
import { UsageMock } from "./usageMock";
import { AccountFilesMock } from "./accountFilesMock";
import { EditMock } from "./editMock";
import { CseMock } from "./cse";
import { CredentialsMock, checkSource } from "./credentialsMock";
import { ObjectMetaMock, seedObjectMeta } from "./objectMeta";
import { clone, compareKeys, MiB, nowIso, seededBytes, sleep, toHex, uuid } from "./util";
import { MockVersions } from "./versions";
import { buildReport, ExcludeRules, MockSyncJobs } from "./syncJobs";
import {
  AWS_PROVIDER,
  buildWorld,
  makeObject,
  MOCK_CREDENTIALS,
  MOCK_HOME,
  MOCK_MFA_CODE,
  type MockBucket,
  type MockObject,
  type Provider,
} from "./world";

export interface MockOptions {
  fixture?: "demo" | "empty";
  /** Artificial delay per command in milliseconds (0 in tests). */
  latencyMs?: number;
  /** Drive transfers and syncs with real timers; tests turn this off and call `advance`. */
  autoRun?: boolean;
  /** Simulation step; also the minimum interval between progress events of a task. */
  tickMs?: number;
  /** Simulated throughput of one upload or download. */
  bytesPerSecond?: number;
  /** Starts with this master password set. */
  masterPassword?: string | null;
  /** Starts locked (needs `masterPassword`). */
  locked?: boolean;
  /** Starts with a crash report from a previous run. */
  crashReport?: boolean;
  /** Overrides settings.advanced.list_page_size at start. */
  pageSize?: number;
  /** Number of files in the local "Many files" folder (demo fixture). */
  manyFiles?: number;
  /** Number of objects in demo-logs/app/ (demo fixture). */
  logObjects?: number;
  /** Pretends the build can mount drives (default: false, like a build without the mount feature). */
  mountSupported?: boolean;
  /** Platform the mock reports in mount_status (default "linux"); "windows" offers drive letters D, E and Z. */
  mountPlatform?: string;
}

/** Drive letters the mock offers when it pretends to be Windows. */
const MOCK_DRIVE_LETTERS = ["D", "E", "Z"];

/** What one enqueue call asked for besides the selection. */
interface JobOptions {
  ifExists: IfExists | null;
  /** The account that owns the destination bucket of a copy or move; null: the task's account. */
  dstAccount: string | null;
}

const NO_OPTIONS: JobOptions = { ifExists: null, dstAccount: null };

interface TaskRecord {
  task: TransferTask;
  /** Keys a delete task removes, fixed when it starts. */
  deleteKeys: string[] | null;
  options?: JobOptions;
  /** Set while the task waits for an answer to an overwrite question. */
  waiting?: PendingConflict;
}

interface PlanRecord {
  plan: SyncPlan;
  executing: boolean;
  queue: number[];
  attempts: Map<number, number>;
  cancelled?: boolean;
}

interface Connection {
  account: Account;
  provider: Provider;
  owner: string;
}

const LEVEL_RANK: Record<LogLevel, number> = { error: 0, warning: 1, information: 2 };
const DELETE_OBJECTS_PER_SECOND = 400;
const SERVER_COPY_FACTOR = 4;
const MFA_SESSION_MS = 12 * 60 * 60 * 1000;
const MAX_PRESIGN_SECS = 7 * 24 * 60 * 60;
const LOG_CAPACITY = 10_000;

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

function emptyChecksums() {
  return { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null };
}

function prefixEntry(key: string): ObjectEntry {
  return {
    key,
    size: 0,
    etag: null,
    last_modified: null,
    storage_class: null,
    checksums: emptyChecksums(),
    is_prefix: true,
  };
}

/** A listed object. Listings never carry checksums (only HEAD-style calls do). */
function objectEntry(key: string, object: MockObject): ObjectEntry {
  return {
    key,
    size: object.size,
    etag: object.etag,
    last_modified: object.last_modified,
    storage_class: object.storage_class,
    checksums: emptyChecksums(),
    is_prefix: false,
  };
}

function encodeToken(lastKey: string): string {
  return `v1:${encodeURIComponent(lastKey)}`;
}

function decodeToken(token: string): string {
  if (!token.startsWith("v1:")) {
    fail("usage", "The continuation token is not valid.");
  }
  try {
    return decodeURIComponent(token.slice(3));
  } catch {
    fail("usage", "The continuation token is not valid.");
  }
}

function encodeKeyPath(key: string): string {
  return key.split("/").map(encodeURIComponent).join("/");
}

function nameMatches(path: string, word: string): boolean {
  return basename(path).toLowerCase().includes(word);
}

/**
 * In-memory implementation of the whole IPC contract, used in the browser
 * and in tests. It simulates accounts, a secret store with an optional master
 * password, S3 buckets and objects, the transfer queue, folder sync, the
 * event log and settings.
 */
export class MockBackend implements Backend {
  readonly fs: MockFs;
  private readonly options: Required<Omit<MockOptions, "masterPassword" | "pageSize">>;
  private readonly providers: Map<string, Provider>;
  private accounts: Account[];
  private readonly iam: IamMock;
  private defaultAccountId: string | null;
  private readonly secrets: Map<string, string>;
  private masterPassword: string | null;
  private unlocked: boolean;
  private readonly mfaSessions = new Map<string, number>();
  private readonly tasks = new Map<string, TaskRecord>();
  /** "Apply to all" answers by task group. */
  private readonly batchAnswers = new Map<string, OverwriteChoice>();
  private readonly plans = new Map<string, PlanRecord>();
  private settings: Settings;
  private proxyPassword: string | null = null;
  private shortenerToken: string | null = null;
  private shortened: string[] = [];
  private readonly bucketConfigs = new MockBucketConfigs();
  private readonly bucketRules = new MockBucketRules();
  /** External buckets pinned to an account: account id to bucket names. */
  private readonly pins = new Map<string, Set<string>>();
  private bookmarks: Bookmark[] = [];
  private readonly syncJobs = new MockSyncJobs();
  /** Locations of the windows opened with `open_new_window` (null: no location), for tests. */
  readonly openedWindows: (WindowLocation | null)[] = [];
  /** The `quit_app` modes asked for, in order, for tests. */
  readonly quitCalls: QuitMode[] = [];
  private pendingLink: LinkAction | null = null;
  /** The `relay_drag` calls, for tests, and whether another window is reported to be under the pointer. */
  readonly relayedDrags: CommandArgs["relay_drag"][] = [];
  relayReaches = false;
  /** Accounts whose ListBuckets is refused (AccessDenied). */
  private readonly listingDenied = new Set<string>();
  private readonly versions = new MockVersions();
  private readonly cloudfront = new MockCloudFront();
  /** Download tasks of one version: they read the version, not the current object. */
  private readonly versionTasks = new Set<string>();
  /** URLs that open_bucket_in_browser opened, for tests and demos. */
  readonly openedUrls: string[] = [];
  private logEntries: LogEntry[] = [];
  private logSequence = 0;
  private crash: CrashReport | null;
  private readonly handlers = new Map<BackendEventName, Set<(payload: never) => void>>();
  private readonly tools: ToolsMock;
  private readonly cse = new CseMock();
  /** AWS profiles, the instance role and the SSO sign-in (tests drive its knobs). */
  readonly credentials = new CredentialsMock({
    latency: () => this.latency(),
    opener: (url) => {
      this.openedUrls.push(url);
    },
  });
  private readonly objectMeta = new ObjectMetaMock({
    bucket: (accountId, name) => this.bucketFor(this.connect(accountId), name),
    latency: () => this.latency(),
    log: (message) => this.addLog("information", "s3", message),
  });
  private readonly accountFiles = new AccountFilesMock();
  private readonly usage = new UsageMock({
    bucket: (accountId, name) => this.bucketFor(this.connect(accountId), name),
    uploads: (accountId, bucket) => this.tools.listUploads({ account_id: accountId, buckets: [bucket] }).uploads,
    versions: this.versions,
    emit: (event) => this.emit("usage://progress", event),
  });
  private readonly mounts = new Map<string, MountInfo>();
  private readonly edits = new EditMock({
    bucket: (accountId, name) => this.bucketFor(this.connect(accountId), name),
    log: (message) => this.addLog("information", "s3", message),
    emit: (session) => this.emit("edit://changed", session),
  });
  private timer: ReturnType<typeof setInterval> | null = null;
  private disposed = false;

  constructor(options: MockOptions = {}) {
    this.options = {
      fixture: options.fixture ?? "demo",
      latencyMs: options.latencyMs ?? 0,
      autoRun: options.autoRun ?? true,
      tickMs: options.tickMs ?? 250,
      bytesPerSecond: options.bytesPerSecond ?? 8 * MiB,
      locked: options.locked ?? false,
      crashReport: options.crashReport ?? false,
      manyFiles: options.manyFiles ?? 0,
      logObjects: options.logObjects ?? 2_500,
      mountSupported: options.mountSupported ?? false,
      mountPlatform: options.mountPlatform ?? "linux",
    };
    const world = buildWorld({
      fixture: this.options.fixture,
      manyFiles: this.options.manyFiles,
      logObjects: this.options.logObjects,
    });
    this.fs = world.fs;
    this.providers = world.providers;
    seedObjectMeta(this.providers);
    this.accounts = world.accounts;
    this.defaultAccountId = world.defaultAccountId;
    this.secrets = world.secrets;
    if (this.options.fixture === "demo") {
      this.bucketConfigs.seedDemo(
        (bucket) => `${AWS_PROVIDER}/${bucket}`,
        (bucket) => `https://storage.example.com/${bucket}`,
      );
    }
    for (const task of world.tasks) {
      this.tasks.set(task.id, { task, deleteKeys: null });
    }
    this.masterPassword = options.masterPassword ?? null;
    this.unlocked = this.masterPassword === null || !this.options.locked;
    this.settings = cloneSettings(DEFAULT_SETTINGS);
    if (options.pageSize !== undefined) {
      this.settings.advanced.list_page_size = options.pageSize;
    }
    this.crash = this.options.crashReport
      ? {
          path: `${MOCK_HOME}/.local/share/bucketbridge/crash/crash-2026-10-28T21-14-03Z.txt`,
          created_at: "2026-10-28T21:14:03Z",
          contents: [
            "BucketBridge 0.1.0 crashed.",
            "",
            "thread 'tokio-runtime-worker' panicked at crates/core/src/transfer/multipart.rs:212:17:",
            "part number 10001 exceeds the maximum of 10000",
            "",
            "stack backtrace:",
            "   0: bucketbridge_core::transfer::multipart::plan_parts",
            "   1: bucketbridge_core::transfer::upload::run",
            "   2: tokio::runtime::task::harness::poll",
          ].join("\n"),
        }
      : null;
    if (this.crash !== null) {
      this.fs.writeFile(this.crash.path, {
        size: new TextEncoder().encode(this.crash.contents).length,
        modified: this.crash.created_at,
      });
    }
    this.iam = new IamMock({
      account: (id) => this.getAccount(id),
      requireAccess: (account) => this.requireAccess(account),
      log: (message) => this.addLog("information", "iam", message),
    });
    this.tools = new ToolsMock({
      fs: this.fs,
      settings: () => this.settings,
      account: (id) => this.getAccount(id),
      requireAccess: (account) => this.requireAccess(account),
      provider: (endpoint) => this.providers.get(endpoint ?? AWS_PROVIDER),
      storedSecret: (account) => this.secrets.get(account.secret_ref),
      bucket: (accountId, name) => this.bucketFor(this.connect(accountId), name),
      log: (level, source, message) => this.addLog(level, source, message),
      simulate: (ms) => (this.options.latencyMs > 0 ? sleep(ms) : Promise.resolve()),
    });
    if (this.options.fixture === "demo") {
      this.versions.seedDemo(this.providers);
    }
    this.addLog("information", "app", "BucketBridge started (mock backend).");
  }

  // ---------------------------------------------------------------- events

  listen<E extends BackendEventName>(event: E, handler: (payload: BackendEvents[E]) => void): Unsubscribe {
    let set = this.handlers.get(event);
    if (!set) {
      set = new Set();
      this.handlers.set(event, set);
    }
    const stored = handler as (payload: never) => void;
    set.add(stored);
    return () => {
      set.delete(stored);
    };
  }

  /** Delivers events asynchronously, like the Tauri event bus, with a private copy per emit. */
  private emit<E extends BackendEventName>(event: E, payload: BackendEvents[E]): void {
    const set = this.handlers.get(event);
    if (!set || set.size === 0) {
      return;
    }
    const copy = clone(payload);
    queueMicrotask(() => {
      for (const handler of set) {
        (handler as (value: BackendEvents[E]) => void)(copy);
      }
    });
  }

  // ---------------------------------------------------------------- test and demo helpers

  /** A `bb://` link reached the app: keeps the resolved action and announces it, like the host. */
  receiveLink(action: LinkAction): void {
    this.pendingLink = action;
    this.emit("link://pending", null);
  }

  /** The app was asked to quit while tasks are active (the setting is "ask"): announces it like the host. */
  requestQuit(request: QuitRequest): void {
    this.emit("quit://requested", request);
  }

  /** Runs the transfer and sync simulation for `ms` milliseconds of simulated time. */
  advance(ms: number): void {
    let remaining = ms;
    while (remaining > 0) {
      const step = Math.min(this.options.tickMs, remaining);
      this.step(step);
      remaining -= step;
    }
  }

  /** Adds a log entry as if the backend had recorded it. */
  injectLog(level: LogLevel, source: string, message: string): void {
    this.addLog(level, source, message);
  }

  /** Stops timers; the instance should not be used afterwards. */
  dispose(): void {
    this.disposed = true;
    this.stopTimer();
    this.handlers.clear();
  }

  objectKeys(bucketName: string): string[] {
    for (const provider of this.providers.values()) {
      const found = provider.buckets.get(bucketName);
      if (found) {
        return this.sortedKeys(found);
      }
    }
    return [];
  }

  // ---------------------------------------------------------------- security and accounts

  async security_status(): Promise<SecurityStatus> {
    await this.latency();
    return this.securityStatus();
  }

  async set_master_password(args: CommandArgs["set_master_password"]): Promise<void> {
    await this.latency();
    if (this.masterPassword !== null && args.current !== this.masterPassword) {
      this.addLog("warning", "security", "Master password change rejected: wrong current password.");
      fail("auth", "The current master password is not correct.");
    }
    if (args.new_password === null) {
      if (this.masterPassword === null) {
        fail("usage", "No master password is set.");
      }
      this.masterPassword = null;
      this.unlocked = true;
      this.addLog("information", "security", "Master password removed.");
    } else {
      if (args.new_password.length < 8) {
        fail("usage", "The master password must be at least 8 characters long.");
      }
      const changed = this.masterPassword !== null;
      this.masterPassword = args.new_password;
      this.unlocked = true;
      this.addLog("information", "security", changed ? "Master password changed." : "Master password set.");
    }
    this.emit("security://changed", this.securityStatus());
  }

  async unlock(args: CommandArgs["unlock"]): Promise<void> {
    await this.latency();
    if (this.masterPassword === null) {
      return;
    }
    if (args.password !== this.masterPassword) {
      this.addLog("warning", "security", "Unlock failed: wrong master password.");
      fail("auth", "The master password is not correct.");
    }
    this.unlocked = true;
    this.addLog("information", "security", "Secrets unlocked.");
    this.emit("security://changed", this.securityStatus());
    this.ensureTimer();
  }

  async lock(): Promise<void> {
    await this.latency();
    if (this.masterPassword !== null) {
      this.unlocked = false;
      this.addLog("information", "security", "Secrets locked.");
    }
    this.emit("security://changed", this.securityStatus());
  }

  async list_accounts(): Promise<AccountList> {
    await this.latency();
    return clone({
      accounts: this.accounts,
      default_account_id: this.defaultAccountId,
      folders: this.accountFiles.list(this.accounts),
    });
  }

  async add_account(args: CommandArgs["add_account"]): Promise<Account> {
    await this.latency();
    const input = args.input;
    this.requireUnlocked();
    const label = input.label.trim();
    if (label.length === 0) {
      fail("usage", "The account label cannot be empty.");
    }
    this.requireUniqueLabel(label, null);
    const source: CredentialSource = input.source ?? { type: "static_keys" };
    if (source.type !== "static_keys") {
      return this.addKeylessAccount(input, label, source);
    }
    if (input.access_key_id.trim().length === 0 || input.secret_access_key.length === 0) {
      fail("usage", "The access key ID and the secret access key are required.");
    }
    this.checkRegion(input.region);
    if (input.endpoint !== null) {
      this.checkEndpoint(input.endpoint);
    }
    const id = uuid();
    const secretRef = `bucketbridge/account/${id}/secret`;
    this.secrets.set(secretRef, input.secret_access_key);
    let sessionRef: string | null = null;
    if (input.session_token !== null) {
      sessionRef = `bucketbridge/account/${id}/session-token`;
      this.secrets.set(sessionRef, input.session_token);
    }
    const account: Account = {
      id,
      label,
      access_key_id: input.access_key_id.trim(),
      secret_ref: secretRef,
      session_token_ref: sessionRef,
      region: input.region,
      endpoint: input.endpoint,
      path_style: input.path_style,
      request_payer: input.request_payer,
      mfa_serial: input.mfa_serial,
      source: { type: "static_keys" },
    };
    this.accounts.push(account);
    this.defaultAccountId ??= id;
    this.addLog("information", "accounts", `Account "${label}" added.`);
    return clone(account);
  }

  /** An account whose credentials come from a profile, a role, SSO or the instance role: no secret is stored. */
  private addKeylessAccount(input: CommandArgs["add_account"]["input"], label: string, source: CredentialSource): Account {
    if (input.access_key_id.trim().length > 0 || input.secret_access_key.length > 0 || input.session_token !== null) {
      fail("usage", "an account that gets its credentials from another source has no access key, secret key or session token of its own");
    }
    this.checkRegion(input.region);
    if (input.endpoint !== null) {
      this.checkEndpoint(input.endpoint);
    }
    checkSource(source, this.accounts, null);
    const account: Account = {
      id: uuid(),
      label,
      access_key_id: "",
      secret_ref: "",
      session_token_ref: null,
      region: input.region,
      endpoint: input.endpoint,
      path_style: input.path_style,
      request_payer: input.request_payer,
      mfa_serial: source.type === "assume_role" ? (source.mfa_serial ?? null) : null,
      source,
    };
    this.accounts.push(account);
    this.defaultAccountId ??= account.id;
    this.addLog("information", "accounts", `Account "${label}" added.`);
    return clone(account);
  }

  async update_account(args: CommandArgs["update_account"]): Promise<Account> {
    await this.latency();
    const account = this.getAccount(args.id);
    const update = args.update;
    const touchesSecrets =
      update.secret_access_key !== null || update.session_token !== null || update.clear_session_token;
    if (touchesSecrets) {
      this.requireUnlocked();
    }
    if (update.label !== null) {
      const label = update.label.trim();
      if (label.length === 0) {
        fail("usage", "The account label cannot be empty.");
      }
      this.requireUniqueLabel(label, account.id);
    }
    if (update.region !== null) {
      this.checkRegion(update.region);
    }
    if (update.endpoint !== null) {
      this.checkEndpoint(update.endpoint);
    }
    if (update.access_key_id?.trim().length === 0) {
      fail("usage", "The access key ID cannot be empty.");
    }
    if (update.source) {
      if (account.source.type === "static_keys" || account.source.type !== update.source.type) {
        fail("usage", "the kind of credential source of an account cannot change");
      }
      checkSource(update.source, this.accounts, account.id);
      account.source = update.source;
      account.mfa_serial = update.source.type === "assume_role" ? (update.source.mfa_serial ?? null) : null;
      this.mfaSessions.delete(account.id);
    }
    if (update.label !== null) account.label = update.label.trim();
    if (update.access_key_id !== null) account.access_key_id = update.access_key_id.trim();
    if (update.secret_access_key !== null) {
      if (account.secret_ref === "") {
        account.secret_ref = `bucketbridge/account/${account.id}/secret`;
      }
      this.secrets.set(account.secret_ref, update.secret_access_key);
    }
    if (update.clear_session_token && account.session_token_ref !== null) {
      this.secrets.delete(account.session_token_ref);
      account.session_token_ref = null;
    }
    if (update.session_token !== null) {
      account.session_token_ref ??= `bucketbridge/account/${account.id}/session-token`;
      this.secrets.set(account.session_token_ref, update.session_token);
    }
    if (update.region !== null) account.region = update.region;
    if (update.clear_endpoint) account.endpoint = null;
    if (update.endpoint !== null) account.endpoint = update.endpoint;
    if (update.path_style !== null) account.path_style = update.path_style;
    if (update.request_payer !== null) account.request_payer = update.request_payer;
    if (update.clear_mfa_serial) account.mfa_serial = null;
    if (update.mfa_serial !== null) account.mfa_serial = update.mfa_serial;
    if (update.access_key_id !== null || update.secret_access_key !== null || update.clear_mfa_serial) {
      this.mfaSessions.delete(account.id);
    }
    this.addLog("information", "accounts", `Account "${account.label}" updated.`);
    return clone(account);
  }

  async remove_account(args: CommandArgs["remove_account"]): Promise<void> {
    await this.latency();
    const account = this.getAccount(args.id);
    const dependent = this.accounts.find(
      (item) => item.source.type === "assume_role" && item.source.source_account_id === account.id,
    );
    if (dependent !== undefined) {
      fail("conflict", `account "${dependent.label}" assumes a role through this account: remove it first`);
    }
    if (account.secret_ref !== "") {
      this.secrets.delete(account.secret_ref);
    }
    if (account.session_token_ref !== null) {
      this.secrets.delete(account.session_token_ref);
    }
    this.accounts = this.accounts.filter((item) => item.id !== account.id);
    this.pins.delete(account.id);
    this.bookmarks = this.bookmarks.filter((item) => item.account_id !== account.id);
    this.syncJobs.dropAccount(account.id);
    this.listingDenied.delete(account.id);
    this.mfaSessions.delete(account.id);
    if (this.defaultAccountId === account.id) {
      this.defaultAccountId = this.accounts[0]?.id ?? null;
    }
    this.addLog("information", "accounts", `Account "${account.label}" removed.`);
  }

  async set_default_account(args: CommandArgs["set_default_account"]): Promise<void> {
    await this.latency();
    const account = this.getAccount(args.id);
    this.defaultAccountId = account.id;
    this.addLog("information", "accounts", `Account "${account.label}" is now the default.`);
  }

  async set_account_layout(args: CommandArgs["set_account_layout"]): Promise<AccountList> {
    await this.latency();
    this.accounts = this.accountFiles.setLayout(this.accounts, args.layout);
    return this.list_accounts();
  }

  async export_accounts(args: CommandArgs["export_accounts"]): Promise<ExportAccountsOutcome> {
    await this.latency();
    const outcome = this.accountFiles.export(this.accounts, args, this.fs);
    this.addLog("information", "accounts", `Exported ${outcome.accounts} account(s) without secrets.`);
    return outcome;
  }

  async import_accounts(args: CommandArgs["import_accounts"]): Promise<ImportReport> {
    await this.latency();
    const report = this.accountFiles.import(this.accounts, args, (account) => {
      this.defaultAccountId ??= account.id;
    });
    this.addLog("information", "accounts", `Imported ${report.imported.length} account(s).`);
    return clone(report);
  }

  /** The text of an account file that an export wrote (for tests). */
  readAccountFile(path: string): string {
    return this.accountFiles.read(path);
  }

  /** Puts an account file in place as if an export had written it (for tests). */
  seedAccountFile(path: string, text: string): void {
    this.accountFiles.seedFile(path, text);
  }

  // ------------------------------------------------- credential sources

  async list_aws_profiles(): Promise<AwsProfileEntry[]> {
    return this.credentials.listProfiles(
      (name) =>
        this.accounts.find((item) => item.source.type === "aws_profile" && item.source.profile === name)?.id ?? null,
    );
  }

  async import_aws_profiles(args: CommandArgs["import_aws_profiles"]): Promise<ImportOutcome> {
    const listed = await this.list_aws_profiles();
    const outcome: ImportOutcome = { imported: [], skipped: [] };
    for (const name of new Set(args.names)) {
      const entry = listed.find((item) => item.name === name);
      if (entry === undefined) {
        outcome.skipped.push({ profile: name, reason: "the profile does not exist" });
      } else if (entry.account_id !== null) {
        outcome.skipped.push({ profile: name, reason: "the profile is imported already" });
      } else if (!entry.importable) {
        outcome.skipped.push({ profile: name, reason: entry.note ?? "the profile cannot be used" });
      } else {
        let label = name;
        for (let n = 1; this.accounts.some((item) => item.label.toLowerCase() === label.toLowerCase()); n += 1) {
          label = n === 1 ? `${name} (AWS)` : `${name} (AWS ${n})`;
        }
        outcome.imported.push(
          await this.add_account({
            input: {
              label,
              access_key_id: "",
              secret_access_key: "",
              session_token: null,
              region: entry.region ?? "us-east-1",
              endpoint: null,
              path_style: false,
              request_payer: false,
              mfa_serial: null,
              source: { type: "aws_profile", profile: name },
            },
          }),
        );
      }
    }
    return outcome;
  }

  probe_instance_role(): Promise<InstanceRoleProbe> {
    return this.credentials.probeInstanceRole();
  }

  sso_start_login(args: CommandArgs["sso_start_login"]): Promise<SsoLoginStarted> {
    return this.credentials.startLogin(args);
  }

  sso_poll_login(args: CommandArgs["sso_poll_login"]): Promise<SsoLoginPoll> {
    return this.credentials.pollLogin(args.login_id);
  }

  sso_cancel_login(args: CommandArgs["sso_cancel_login"]): Promise<void> {
    this.credentials.cancelLogin(args.login_id);
    return Promise.resolve();
  }

  sso_status(args: CommandArgs["sso_status"]): Promise<SsoSignInStatus> {
    return this.credentials.status(args);
  }

  sso_sign_out(args: CommandArgs["sso_sign_out"]): Promise<void> {
    return this.credentials.signOut(args);
  }

  sso_list_accounts(args: CommandArgs["sso_list_accounts"]): Promise<SsoAccount[]> {
    return this.credentials.listAccounts(args);
  }

  sso_list_roles(args: CommandArgs["sso_list_roles"]): Promise<string[]> {
    return this.credentials.listRoles(args, args.account_id);
  }

  async mfa_login(args: CommandArgs["mfa_login"]): Promise<void> {
    await this.latency();
    const account = this.getAccount(args.account_id);
    this.requireUnlocked();
    if (account.mfa_serial === null) {
      fail("usage", "This account has no MFA device configured.");
    }
    if (!/^\d{6}$/.test(args.code)) {
      fail("usage", "Enter the 6-digit code shown by the MFA device.");
    }
    this.checkCredentials(account);
    if (args.code !== MOCK_MFA_CODE) {
      this.addLog("error", "security", `MFA code rejected for "${account.label}".`);
      fail("auth", "The MFA code was not accepted. Wait for the next code and try again.");
    }
    this.mfaSessions.set(account.id, Date.now() + MFA_SESSION_MS);
    this.addLog("information", "security", `MFA session started for "${account.label}".`);
    this.ensureTimer();
  }

  // ---------------------------------------------------------------- browsing

  async list_buckets(args: CommandArgs["list_buckets"]): Promise<Bucket[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const pinned = [...(this.pins.get(args.account_id) ?? [])];
    const buckets: Bucket[] = [];
    if (this.listingDenied.has(args.account_id)) {
      // Like the backend: without pinned buckets the refusal is the error.
      if (pinned.length === 0) {
        fail("auth", "Access Denied (AccessDenied, HTTP 403)");
      }
    } else {
      for (const item of connection.provider.buckets.values()) {
        if (item.owner === connection.owner) {
          buckets.push({
            name: item.name,
            region: item.region,
            creation_date: item.creation_date,
            owner: `${item.owner}-owner`,
            kind: item.kind,
          });
        }
      }
    }
    // External buckets that the listing does not contain: a name and nothing else.
    for (const name of pinned) {
      if (!buckets.some((item) => item.name === name)) {
        buckets.push({ name, region: null, creation_date: null, owner: null, kind: "external" });
      }
    }
    buckets.sort((a, b) => compareKeys(a.name, b.name));
    return buckets;
  }

  async list_objects(args: CommandArgs["list_objects"]): Promise<ObjectPage> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    if (args.prefix.length > 0 && !args.prefix.endsWith("/")) {
      fail("usage", "A listing prefix must be empty or end with a slash.");
    }
    const after = args.token === null ? null : decodeToken(args.token);
    const pageSize = this.settings.advanced.list_page_size;
    const prefix = args.prefix;
    const entries: ObjectEntry[] = [];
    let lastKey: string | null = null;
    let more = false;
    let openPrefix: string | null = null;
    for (const key of this.sortedKeys(target)) {
      if (!key.startsWith(prefix) || key === prefix) {
        continue;
      }
      if (after !== null && compareKeys(key, after) <= 0) {
        continue;
      }
      let candidate: ObjectEntry;
      if (args.flat) {
        const object = target.objects.get(key);
        if (!object) {
          continue;
        }
        candidate = objectEntry(key, object);
      } else {
        const rest = key.slice(prefix.length);
        const slash = rest.indexOf("/");
        if (slash === -1) {
          const object = target.objects.get(key);
          if (!object) {
            continue;
          }
          candidate = objectEntry(key, object);
        } else {
          const common = prefix + rest.slice(0, slash + 1);
          if (common === openPrefix || (after !== null && key.startsWith(after) && after.endsWith("/"))) {
            continue;
          }
          openPrefix = common;
          candidate = prefixEntry(common);
        }
      }
      if (entries.length === pageSize) {
        more = true;
        break;
      }
      entries.push(candidate);
      lastKey = candidate.key;
    }
    // Like S3's CommonPrefixes and Contents, each page lists its prefixes first, then its objects.
    const ordered = [...entries.filter((entry) => entry.is_prefix), ...entries.filter((entry) => !entry.is_prefix)];
    return { entries: ordered, next_token: more && lastKey !== null ? encodeToken(lastKey) : null };
  }

  async create_folder(args: CommandArgs["create_folder"]): Promise<void> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    if (!isFolderKey(args.key) || args.key === "/") {
      fail("usage", "A folder key must end with a slash.");
    }
    if (utf8Length(args.key) > 1024) {
      fail("usage", "The key is longer than 1024 bytes.");
    }
    if (!target.objects.has(args.key)) {
      target.objects.set(args.key, makeObject(args.key, 0, nowIso()));
    }
    this.addLog("information", "s3", `Folder s3://${args.bucket}/${args.key} created.`);
  }

  async create_bucket(args: CommandArgs["create_bucket"]): Promise<void> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const nameError = validateBucketName(args.name);
    if (nameError) {
      fail("usage", nameError);
    }
    if (args.region !== null) {
      this.checkRegion(args.region);
    }
    const existing = connection.provider.buckets.get(args.name);
    if (existing) {
      if (existing.owner === connection.owner) {
        fail("conflict", "You already own a bucket with this name (BucketAlreadyOwnedByYou).");
      }
      fail("conflict", "The bucket name is already taken by someone else (BucketAlreadyExists).");
    }
    connection.provider.buckets.set(args.name, {
      name: args.name,
      region: args.region ?? connection.account.region,
      creation_date: nowIso(),
      owner: connection.owner,
      kind: connection.account.endpoint === null ? "aws" : "external",
      objects: new Map(),
    });
    this.addLog("information", "s3", `Bucket ${args.name} created.`);
  }

  async delete_bucket(args: CommandArgs["delete_bucket"]): Promise<void> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.name);
    if (target.objects.size > 0) {
      fail("conflict", "The bucket is not empty (BucketNotEmpty). Delete its objects first.");
    }
    connection.provider.buckets.delete(target.name);
    this.bucketConfigs.forget(this.slot(connection, target).key);
    this.addLog("information", "s3", `Bucket ${args.name} deleted.`);
  }

  async presign(args: CommandArgs["presign"]): Promise<string> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    if (!Number.isInteger(args.expires_secs) || args.expires_secs < 1 || args.expires_secs > MAX_PRESIGN_SECS) {
      fail("usage", "The expiry must be between 1 second and 7 days.");
    }
    if (args.key.length === 0 || isFolderKey(args.key)) {
      fail("usage", "Choose an object, not a folder.");
    }
    const now = new Date();
    const stamp = now.toISOString().replace(/[-:]/g, "").replace(/\.\d+/, "");
    const date = stamp.slice(0, 8);
    const signature = toHex(seededBytes(`${args.bucket}/${args.key}/${args.method}/${stamp}`, 32));
    const credential = encodeURIComponent(
      `${connection.account.access_key_id}/${date}/${target.region}/s3/aws4_request`,
    );
    const endpoint = connection.account.endpoint;
    let base: string;
    if (endpoint !== null) {
      const url = new URL(endpoint);
      base = connection.account.path_style
        ? `${url.origin}/${args.bucket}`
        : `${url.protocol}//${args.bucket}.${url.host}`;
    } else {
      base = `https://${args.bucket}.s3.${target.region}.example.com`;
    }
    const query = [
      "X-Amz-Algorithm=AWS4-HMAC-SHA256",
      `X-Amz-Credential=${credential}`,
      `X-Amz-Date=${stamp}`,
      `X-Amz-Expires=${args.expires_secs}`,
      "X-Amz-SignedHeaders=host",
      `X-Amz-Signature=${signature}`,
    ].join("&");
    this.addLog(
      "information",
      "s3",
      `Presigned ${args.method.toUpperCase()} URL created for s3://${args.bucket}/${args.key}.`,
    );
    return `${base}/${encodeKeyPath(args.key)}?${query}`;
  }

  async open_object(args: CommandArgs["open_object"]): Promise<void> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    if (isFolderKey(args.key) || !target.objects.has(args.key)) {
      fail("not_found", "The object does not exist (NoSuchKey).");
    }
    this.addLog(
      "information",
      "s3",
      `Opened s3://${args.bucket}/${args.key} with the default application (downloaded to a temporary folder).`,
    );
  }

  async open_object_with(args: CommandArgs["open_object_with"]): Promise<void> {
    await this.latency();
    if (args.app.trim().length === 0) {
      fail("usage", "Choose an application.");
    }
    await this.open_object(args);
    this.settings.general.open_with_recent = [
      args.app,
      ...this.settings.general.open_with_recent.filter((known) => known !== args.app),
    ].slice(0, 8);
    this.addLog("information", "s3", `Opened s3://${args.bucket}/${args.key} with ${args.app}.`);
  }

  async start_edit(args: CommandArgs["start_edit"]): Promise<EditSession> {
    await this.latency();
    const app = args.app?.trim() ?? "";
    this.connect(args.account_id);
    const { session } = this.edits.start(args.account_id, args.bucket, args.key);
    if (app.length > 0) {
      this.settings.general.open_with_recent = [
        app,
        ...this.settings.general.open_with_recent.filter((known) => known !== app),
      ].slice(0, 8);
    }
    return session;
  }

  async list_edits(): Promise<EditSession[]> {
    await this.latency();
    return this.edits.list();
  }

  async stop_edit(args: CommandArgs["stop_edit"]): Promise<void> {
    await this.latency();
    this.edits.stop(args.id, args.discard === true);
  }

  async resolve_edit_conflict(args: CommandArgs["resolve_edit_conflict"]): Promise<EditResolved> {
    await this.latency();
    return this.edits.resolve(args.id, args.resolution);
  }

  /** Plays the user's editor saving the local copy of an edit session (uploads it unless the object changed). */
  saveEdit(id: string): void {
    this.edits.save(id);
  }

  /** Plays somebody else replacing an object on the server (its ETag changes). */
  changeObject(accountId: string, bucket: string, key: string): void {
    this.edits.changeRemote(accountId, bucket, key);
  }

  /** Plays somebody else deleting an object on the server. */
  deleteObject(accountId: string, bucket: string, key: string): void {
    this.edits.deleteRemote(accountId, bucket, key);
  }

  async open_bucket_in_browser(args: CommandArgs["open_bucket_in_browser"]): Promise<string> {
    await this.latency();
    const connection = this.connect(args.account_id);
    this.bucketFor(connection, args.bucket);
    const url = bucketUrl(connection.account, args.bucket, args.region);
    this.openedUrls.push(url);
    this.addLog("information", "s3", `Opened ${url} in the browser.`);
    return url;
  }

  // ---------------------------------------------------------------- bucket configuration

  /** Failure injection for tests and demos: a section of a bucket answers with this error. */
  failBucketSection(accountId: string, bucket: string, section: keyof BucketConfig, error: { code: BbErrorCode; message: string }): void {
    const connection = this.connect(accountId);
    this.bucketConfigs.failSection(this.slot(connection, this.bucketFor(connection, bucket)).key, section, error);
  }

  /** Makes ListBuckets of an account fail with AccessDenied (pinned buckets still show). */
  denyListing(accountId: string): void {
    this.listingDenied.add(accountId);
  }

  async get_bucket_config(args: CommandArgs["get_bucket_config"]): Promise<BucketConfig> {
    await this.latency();
    const connection = this.connect(args.account_id);
    this.checkExistingName(args.bucket);
    try {
      return this.bucketConfigs.config(this.slot(connection, this.bucketFor(connection, args.bucket)));
    } catch (raw) {
      // A bucket that is missing or refused fails every section on its own, like the backend's.
      if (raw instanceof BackendError && (raw.code === "not_found" || raw.code === "auth")) {
        return MockBucketConfigs.failedConfig(raw.toJSON(), connection.account.endpoint !== null);
      }
      throw raw;
    }
  }

  async get_bucket_policy(args: CommandArgs["get_bucket_policy"]): Promise<string | null> {
    await this.latency();
    return this.bucketConfigs.getPolicy(this.configSlot(args.account_id, args.bucket));
  }

  async put_bucket_policy(args: CommandArgs["put_bucket_policy"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putPolicy(this.configSlot(args.account_id, args.bucket), args.policy);
    this.addLog("information", "s3", `Policy of bucket ${args.bucket} saved.`);
  }

  async delete_bucket_policy(args: CommandArgs["delete_bucket_policy"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.deletePolicy(this.configSlot(args.account_id, args.bucket));
    this.addLog("information", "s3", `Policy of bucket ${args.bucket} deleted.`);
  }

  async validate_bucket_policy(args: CommandArgs["validate_bucket_policy"]): Promise<PolicyIssue[]> {
    await this.latency();
    return MockBucketConfigs.validate(args.policy);
  }

  async format_bucket_policy(args: CommandArgs["format_bucket_policy"]): Promise<string> {
    await this.latency();
    const formatted = formatPolicy(args.policy);
    if ("issue" in formatted) {
      fail("usage", issueText(formatted.issue));
    }
    return formatted.text;
  }

  async get_public_access_block(args: CommandArgs["get_public_access_block"]): Promise<PublicAccessBlock | null> {
    await this.latency();
    return this.bucketConfigs.getBlock(this.configSlot(args.account_id, args.bucket));
  }

  async put_public_access_block(args: CommandArgs["put_public_access_block"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putBlock(this.configSlot(args.account_id, args.bucket), args.config);
    this.addLog("information", "s3", `Public access block of bucket ${args.bucket} saved.`);
  }

  async delete_public_access_block(args: CommandArgs["delete_public_access_block"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.deleteBlock(this.configSlot(args.account_id, args.bucket));
    this.addLog("information", "s3", `Public access block of bucket ${args.bucket} removed.`);
  }

  async get_bucket_versioning(args: CommandArgs["get_bucket_versioning"]): Promise<BucketVersioning> {
    await this.latency();
    return this.bucketConfigs.getVersioning(this.configSlot(args.account_id, args.bucket));
  }

  async put_bucket_versioning(args: CommandArgs["put_bucket_versioning"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putVersioning(this.configSlot(args.account_id, args.bucket), args.status);
    this.addLog("information", "s3", `Versioning of bucket ${args.bucket} is now ${args.status}.`);
  }

  async get_bucket_logging(args: CommandArgs["get_bucket_logging"]): Promise<LoggingTarget | null> {
    await this.latency();
    return this.bucketConfigs.getLogging(this.configSlot(args.account_id, args.bucket));
  }

  async put_bucket_logging(args: CommandArgs["put_bucket_logging"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putLogging(this.configSlot(args.account_id, args.bucket), args.target);
    this.addLog("information", "s3", `Access logging of bucket ${args.bucket} saved.`);
  }

  async disable_bucket_logging(args: CommandArgs["disable_bucket_logging"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.disableLogging(this.configSlot(args.account_id, args.bucket));
    this.addLog("information", "s3", `Access logging of bucket ${args.bucket} turned off.`);
  }

  async get_bucket_website(args: CommandArgs["get_bucket_website"]): Promise<WebsiteConfig | null> {
    await this.latency();
    return this.bucketConfigs.getWebsite(this.configSlot(args.account_id, args.bucket));
  }

  async put_bucket_website(args: CommandArgs["put_bucket_website"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putWebsite(this.configSlot(args.account_id, args.bucket), args.website);
    this.addLog("information", "s3", `Website configuration of bucket ${args.bucket} saved.`);
  }

  async delete_bucket_website(args: CommandArgs["delete_bucket_website"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.deleteWebsite(this.configSlot(args.account_id, args.bucket));
    this.addLog("information", "s3", `Website hosting of bucket ${args.bucket} turned off.`);
  }

  async get_bucket_acceleration(args: CommandArgs["get_bucket_acceleration"]): Promise<AccelerationStatus> {
    await this.latency();
    return this.bucketConfigs.getAcceleration(this.configSlot(args.account_id, args.bucket));
  }

  async put_bucket_acceleration(args: CommandArgs["put_bucket_acceleration"]): Promise<void> {
    await this.latency();
    this.bucketConfigs.putAcceleration(this.configSlot(args.account_id, args.bucket), args.status);
    this.addLog("information", "s3", `Transfer acceleration of bucket ${args.bucket} is now ${args.status}.`);
  }

  // ---- bucket rules (T04, T07) ----

  async get_bucket_rules(args: CommandArgs["get_bucket_rules"]): Promise<BucketRules> {
    await this.latency();
    return this.bucketRules.rules(this.configSlot(args.account_id, args.bucket).key);
  }

  async get_bucket_lifecycle(args: CommandArgs["get_bucket_lifecycle"]): Promise<LifecycleConfig | null> {
    await this.latency();
    return this.bucketRules.getLifecycle(this.configSlot(args.account_id, args.bucket).key);
  }

  async put_bucket_lifecycle(args: CommandArgs["put_bucket_lifecycle"]): Promise<void> {
    await this.latency();
    this.bucketRules.putLifecycle(this.configSlot(args.account_id, args.bucket).key, args.config);
    this.addLog("information", "s3", `Lifecycle rules of bucket ${args.bucket} saved.`);
  }

  async delete_bucket_lifecycle(args: CommandArgs["delete_bucket_lifecycle"]): Promise<void> {
    await this.latency();
    this.bucketRules.deleteLifecycle(this.configSlot(args.account_id, args.bucket).key);
  }

  async validate_bucket_lifecycle(args: CommandArgs["validate_bucket_lifecycle"]): Promise<RuleIssue[]> {
    await this.latency();
    return lifecycleIssues(args.document);
  }

  async list_bucket_tiering(args: CommandArgs["list_bucket_tiering"]): Promise<TieringConfig[]> {
    await this.latency();
    return this.bucketRules.tiering(this.configSlot(args.account_id, args.bucket).key);
  }

  async put_bucket_tiering(args: CommandArgs["put_bucket_tiering"]): Promise<void> {
    await this.latency();
    this.bucketRules.putTiering(this.configSlot(args.account_id, args.bucket).key, args.config);
  }

  async delete_bucket_tiering(args: CommandArgs["delete_bucket_tiering"]): Promise<void> {
    await this.latency();
    this.bucketRules.deleteTiering(this.configSlot(args.account_id, args.bucket).key, args.id);
  }

  async validate_bucket_tiering(args: CommandArgs["validate_bucket_tiering"]): Promise<RuleIssue[]> {
    await this.latency();
    return tieringIssues(args.document);
  }

  async get_bucket_cors(args: CommandArgs["get_bucket_cors"]): Promise<CorsConfig | null> {
    await this.latency();
    return this.bucketRules.getCors(this.configSlot(args.account_id, args.bucket).key);
  }

  async put_bucket_cors(args: CommandArgs["put_bucket_cors"]): Promise<void> {
    await this.latency();
    this.bucketRules.putCors(this.configSlot(args.account_id, args.bucket).key, args.config);
    this.addLog("information", "s3", `CORS rules of bucket ${args.bucket} saved.`);
  }

  async delete_bucket_cors(args: CommandArgs["delete_bucket_cors"]): Promise<void> {
    await this.latency();
    this.bucketRules.deleteCors(this.configSlot(args.account_id, args.bucket).key);
  }

  async validate_bucket_cors(args: CommandArgs["validate_bucket_cors"]): Promise<RuleIssue[]> {
    await this.latency();
    return corsIssues(args.document);
  }

  async get_bucket_replication(args: CommandArgs["get_bucket_replication"]): Promise<ReplicationConfig | null> {
    await this.latency();
    return this.bucketRules.getReplication(this.configSlot(args.account_id, args.bucket).key);
  }

  async put_bucket_replication(args: CommandArgs["put_bucket_replication"]): Promise<void> {
    await this.latency();
    const slot = this.configSlot(args.account_id, args.bucket);
    const enabled = this.bucketConfigs.getVersioning(slot).status === "enabled";
    this.bucketRules.putReplication(slot.key, args.bucket, enabled, args.config);
    this.addLog("information", "s3", `Replication of bucket ${args.bucket} saved.`);
  }

  async delete_bucket_replication(args: CommandArgs["delete_bucket_replication"]): Promise<void> {
    await this.latency();
    this.bucketRules.deleteReplication(this.configSlot(args.account_id, args.bucket).key);
  }

  async validate_bucket_replication(args: CommandArgs["validate_bucket_replication"]): Promise<RuleIssue[]> {
    await this.latency();
    return replicationIssues(args.document);
  }

  async list_external_buckets(args: CommandArgs["list_external_buckets"]): Promise<string[]> {
    await this.latency();
    this.getAccount(args.account_id);
    return [...(this.pins.get(args.account_id) ?? [])].sort(compareKeys);
  }

  async add_external_bucket(args: CommandArgs["add_external_bucket"]): Promise<void> {
    await this.latency();
    const connection = this.connect(args.account_id);
    this.checkExistingName(args.name);
    if (!connection.provider.buckets.has(args.name)) {
      fail("not_found", `bucket '${args.name}' does not exist`);
    }
    const names = this.pins.get(args.account_id) ?? new Set<string>();
    names.add(args.name);
    this.pins.set(args.account_id, names);
    this.addLog("information", "accounts", `Bucket ${args.name} pinned to "${connection.account.label}" as an external bucket.`);
  }

  async remove_external_bucket(args: CommandArgs["remove_external_bucket"]): Promise<void> {
    await this.latency();
    const account = this.getAccount(args.account_id);
    const names = this.pins.get(account.id);
    if (names?.delete(args.name) !== true) {
      fail("not_found", `bucket '${args.name}' is not pinned to this account`);
    }
    if (names.size === 0) {
      this.pins.delete(account.id);
    }
    this.addLog("information", "accounts", `External bucket ${args.name} removed from "${account.label}".`);
  }

  // ---------------------------------------------------------------- bookmarks and windows (T14)
  async list_bookmarks(): Promise<Bookmark[]> {
    await this.latency();
    return clone(this.bookmarks);
  }

  async add_bookmark(args: CommandArgs["add_bookmark"]): Promise<Bookmark[]> {
    await this.latency();
    this.getAccount(args.account_id);
    if (args.bucket.trim() === "") {
      fail("usage", "a bookmark needs a bucket");
    }
    const typed = args.label?.trim() ?? "";
    const label = typed !== "" ? this.checkBookmarkLabel(typed) : args.prefix === "" ? args.bucket : `${args.bucket}/${args.prefix}`;
    const existing = this.bookmarks.find(
      (item) => item.account_id === args.account_id && item.bucket === args.bucket && item.prefix === args.prefix,
    );
    if (existing !== undefined) {
      fail("conflict", `this location is already bookmarked as '${existing.label}'`);
    }
    this.bookmarks.push({ id: uuid(), account_id: args.account_id, bucket: args.bucket, prefix: args.prefix, label });
    return this.bookmarksChanged();
  }

  async rename_bookmark(args: CommandArgs["rename_bookmark"]): Promise<Bookmark[]> {
    await this.latency();
    const label = this.checkBookmarkLabel(args.label.trim());
    const bookmark = this.bookmarks.find((item) => item.id === args.id);
    if (bookmark === undefined) {
      fail("not_found", `there is no bookmark '${args.id}'`);
    }
    bookmark.label = label;
    return this.bookmarksChanged();
  }

  async remove_bookmark(args: CommandArgs["remove_bookmark"]): Promise<Bookmark[]> {
    await this.latency();
    if (!this.bookmarks.some((item) => item.id === args.id)) {
      fail("not_found", `there is no bookmark '${args.id}'`);
    }
    this.bookmarks = this.bookmarks.filter((item) => item.id !== args.id);
    return this.bookmarksChanged();
  }

  relay_drag(args: CommandArgs["relay_drag"]): Promise<boolean> {
    this.relayedDrags.push({ ...args });
    return Promise.resolve(this.relayReaches);
  }

  /** Test hook: delivers a drag of another window to this one. */
  deliverDrag(incoming: DragIncoming): void {
    this.emit("drag://incoming", incoming);
  }

  async open_new_window(args: CommandArgs["open_new_window"]): Promise<string> {
    await this.latency();
    this.openedWindows.push(args.location === null ? null : { ...args.location });
    return `main-${this.openedWindows.length}`;
  }

  private checkBookmarkLabel(label: string): string {
    if (label === "") {
      fail("usage", "the bookmark needs a label");
    }
    if ([...label].length > 100) {
      fail("usage", "the label is too long; the maximum is 100 characters");
    }
    return label;
  }

  private bookmarksChanged(): Bookmark[] {
    this.emit("bookmarks://changed", clone(this.bookmarks));
    return clone(this.bookmarks);
  }

  // ---------------------------------------------------------------- object metadata (T06)

  get_object_details(args: CommandArgs["get_object_details"]) {
    return this.objectMeta.get_object_details(args);
  }

  update_object_tags(args: CommandArgs["update_object_tags"]) {
    return this.objectMeta.update_object_tags(args);
  }

  set_object_storage_class(args: CommandArgs["set_object_storage_class"]) {
    return this.objectMeta.set_object_storage_class(args);
  }

  set_object_encryption(args: CommandArgs["set_object_encryption"]) {
    return this.objectMeta.set_object_encryption(args);
  }

  update_object_headers(args: CommandArgs["update_object_headers"]) {
    return this.objectMeta.update_object_headers(args);
  }

  set_object_acl(args: CommandArgs["set_object_acl"]) {
    return this.objectMeta.set_object_acl(args);
  }

  // ---------------------------------------------------------------- object lock, ownership, restore (T07)

  private readonly objectLock = new ObjectLockMock();

  get_bucket_object_lock(args: CommandArgs["get_bucket_object_lock"]) {
    return this.objectLock.get_bucket_object_lock(args);
  }

  put_bucket_object_lock(args: CommandArgs["put_bucket_object_lock"]) {
    return this.objectLock.put_bucket_object_lock(args);
  }

  get_object_lock(args: CommandArgs["get_object_lock"]) {
    return this.objectLock.get_object_lock(args);
  }

  set_object_retention(args: CommandArgs["set_object_retention"]) {
    return this.objectLock.set_object_retention(args);
  }

  set_object_legal_hold(args: CommandArgs["set_object_legal_hold"]) {
    return this.objectLock.set_object_legal_hold(args);
  }

  get_object_ownership(args: CommandArgs["get_object_ownership"]) {
    return this.objectLock.get_object_ownership(args);
  }

  put_object_ownership(args: CommandArgs["put_object_ownership"]) {
    return this.objectLock.put_object_ownership(args);
  }

  restore_objects(args: CommandArgs["restore_objects"]) {
    return this.objectLock.restore_objects(args);
  }

  get_restore_state(args: CommandArgs["get_restore_state"]) {
    return this.objectLock.get_restore_state(args);
  }

  // ---------------------------------------------------------------- client-side encryption (T07)

  cse_probe_object(args: CommandArgs["cse_probe_object"]) {
    return this.cse.cse_probe_object(args);
  }

  cse_check_password(args: CommandArgs["cse_check_password"]) {
    return this.cse.cse_check_password(args);
  }

  cse_encrypt_object(args: CommandArgs["cse_encrypt_object"]) {
    return this.cse.cse_encrypt_object(args);
  }

  cse_decrypt_object(args: CommandArgs["cse_decrypt_object"]) {
    return this.cse.cse_decrypt_object(args);
  }

  /** The mock keeps no object content: an encrypting upload is an ordinary one. */
  enqueue_upload_encrypted(args: CommandArgs["enqueue_upload_encrypted"]): Promise<TransferTask[]> {
    return this.enqueue_upload(args);
  }

  /** The mock keeps no object content: a decrypting download is an ordinary one. */
  enqueue_download_decrypted(args: CommandArgs["enqueue_download_decrypted"]): Promise<TransferTask[]> {
    return this.enqueue_download(args);
  }

  // ---------------------------------------------------------------- transfer queue

  async enqueue_upload(args: CommandArgs["enqueue_upload"]): Promise<TransferTask[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    this.bucketFor(connection, args.bucket);
    this.checkPrefix(args.prefix);
    if (args.local_paths.length === 0) {
      fail("usage", "Choose at least one file or folder.");
    }
    const sources = args.local_paths.map((raw) => {
      const path = normalizePath(raw);
      if (path === null || !this.fs.exists(path)) {
        fail("not_found", `The local path does not exist: ${raw}`);
      }
      return path;
    });
    this.requireDistinctDestinations(
      sources.map((path) => `${args.prefix}${basename(path)}${this.fs.isDir(path) ? "/" : ""}`),
      `s3://${args.bucket}/${args.prefix}`,
    );
    const created: TransferTask[] = [];
    for (const path of sources) {
      if (this.fs.isFile(path)) {
        const file = this.fs.stat(path);
        created.push(
          this.createTask(connection.account.id, null, "upload", { type: "local", path }, {
            type: "s3",
            bucket: args.bucket,
            key: `${args.prefix}${basename(path)}`,
          }, file?.size ?? 0),
        );
        continue;
      }
      const group = uuid();
      const root = `${args.prefix}${basename(path)}/`;
      const walked = this.fs.walk(path);
      for (const file of walked.files) {
        created.push(
          this.createTask(connection.account.id, group, "upload", { type: "local", path: file.path }, {
            type: "s3",
            bucket: args.bucket,
            key: `${root}${file.rel}`,
          }, file.file.size),
        );
      }
      for (const dir of walked.emptyDirs) {
        created.push(
          this.createTask(connection.account.id, group, "upload", { type: "local", path: dir.path }, {
            type: "s3",
            bucket: args.bucket,
            key: `${root}${dir.rel}/`,
          }, 0),
        );
      }
      if (walked.files.length === 0 && walked.emptyDirs.length === 0) {
        created.push(
          this.createTask(connection.account.id, group, "upload", { type: "local", path }, {
            type: "s3",
            bucket: args.bucket,
            key: root,
          }, 0),
        );
      }
    }
    this.addLog("information", "transfer", `Queued ${created.length} upload task(s) to s3://${args.bucket}/${args.prefix}.`);
    return this.announce(created, { ifExists: args.if_exists ?? null, dstAccount: null });
  }

  async enqueue_download(args: CommandArgs["enqueue_download"]): Promise<TransferTask[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const source = this.bucketFor(connection, args.bucket);
    const destination = normalizePath(args.dest_dir);
    if (destination === null) {
      fail("usage", "The destination folder must be an absolute path.");
    }
    if (this.fs.isFile(destination)) {
      fail("io", `Not a directory: ${destination}`);
    }
    this.requireKeys(args.keys);
    this.requireDistinctDestinations(
      args.keys.map((key) => joinPath(destination, baseName(key))),
      destination,
    );
    const created: TransferTask[] = [];
    for (const key of args.keys) {
      if (isFolderKey(key)) {
        const keys = this.keysUnder(source, key);
        if (keys.length === 0) {
          fail("not_found", `No objects found under s3://${args.bucket}/${key}`);
        }
        const group = uuid();
        const strip = parentPrefix(key).length;
        for (const objectKey of keys) {
          const object = source.objects.get(objectKey);
          created.push(
            this.createTask(connection.account.id, group, "download", {
              type: "s3",
              bucket: args.bucket,
              key: objectKey,
            }, { type: "local", path: joinPath(destination, objectKey.slice(strip)) }, object?.size ?? 0),
          );
        }
      } else {
        const object = source.objects.get(key);
        if (!object) {
          fail("not_found", `The object does not exist: s3://${args.bucket}/${key}`);
        }
        created.push(
          this.createTask(connection.account.id, null, "download", { type: "s3", bucket: args.bucket, key }, {
            type: "local",
            path: joinPath(destination, baseName(key)),
          }, object.size),
        );
      }
    }
    this.addLog("information", "transfer", `Queued ${created.length} download task(s) to ${destination}.`);
    return this.announce(created, { ifExists: args.if_exists ?? null, dstAccount: null });
  }

  async enqueue_copy(args: CommandArgs["enqueue_copy"]): Promise<TransferTask[]> {
    await this.latency();
    return this.enqueueCopyOrMove("copy", args);
  }

  async enqueue_move(args: CommandArgs["enqueue_move"]): Promise<TransferTask[]> {
    await this.latency();
    return this.enqueueCopyOrMove("move", args);
  }

  async enqueue_delete(args: CommandArgs["enqueue_delete"]): Promise<TransferTask[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    this.requireKeys(args.keys);
    const created: TransferTask[] = [];
    for (const key of args.keys) {
      const count = isFolderKey(key) ? this.keysUnder(target, key).length : target.objects.has(key) ? 1 : 0;
      if (count === 0) {
        fail("not_found", `Nothing to delete at s3://${args.bucket}/${key}`);
      }
      created.push(
        this.createTask(connection.account.id, null, "delete", { type: "s3", bucket: args.bucket, key }, null, count),
      );
    }
    this.addLog("information", "transfer", `Queued ${created.length} delete task(s) in ${args.bucket}.`);
    return this.announce(created);
  }

  async rename(args: CommandArgs["rename"]): Promise<TransferTask[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    const folder = isFolderKey(args.from_key);
    if (folder !== isFolderKey(args.to_key)) {
      fail("usage", "A folder can only be renamed to a folder, and an object to an object.");
    }
    if (args.from_key === args.to_key) {
      fail("usage", "The new name is the same as the current one.");
    }
    if (folder && args.to_key.startsWith(args.from_key)) {
      fail("usage", "A folder cannot be moved into itself.");
    }
    if (utf8Length(args.to_key) > 1024) {
      fail("usage", "The new key is longer than 1024 bytes.");
    }
    const sources = folder ? this.keysUnder(target, args.from_key) : target.objects.has(args.from_key) ? [args.from_key] : [];
    if (sources.length === 0) {
      fail("not_found", `s3://${args.bucket}/${args.from_key} does not exist.`);
    }
    const exists = folder ? this.keysUnder(target, args.to_key).length > 0 : target.objects.has(args.to_key);
    if (exists) {
      fail("conflict", `s3://${args.bucket}/${args.to_key} already exists.`);
    }
    const group = folder ? uuid() : null;
    const created = sources.map((key) =>
      this.createTask(connection.account.id, group, "move", { type: "s3", bucket: args.bucket, key }, {
        type: "s3",
        bucket: args.bucket,
        key: folder ? `${args.to_key}${key.slice(args.from_key.length)}` : args.to_key,
      }, target.objects.get(key)?.size ?? 0),
    );
    this.addLog("information", "transfer", `Renaming s3://${args.bucket}/${args.from_key} to ${args.to_key}.`);
    return this.announce(created);
  }

  async list_tasks(): Promise<TransferTask[]> {
    await this.latency();
    return [...this.tasks.values()].map((record) => clone(record.task));
  }

  async list_conflicts(): Promise<PendingConflict[]> {
    await this.latency();
    const waiting: PendingConflict[] = [];
    for (const record of this.tasks.values()) {
      if (record.waiting !== undefined) {
        waiting.push(clone(record.waiting));
      }
    }
    return waiting;
  }

  async answer_conflict(args: CommandArgs["answer_conflict"]): Promise<void> {
    await this.latency();
    const record = this.tasks.get(args.task_id);
    if (!record?.waiting) {
      fail("not_found", `Task ${args.task_id} is not waiting for an answer.`);
    }
    const group = record.task.group_id;
    this.settleConflict(record, args.choice);
    if (args.apply_to_batch && group !== null) {
      this.batchAnswers.set(group, args.choice);
      for (const other of [...this.tasks.values()]) {
        if (other.waiting !== undefined && other.task.group_id === group) {
          this.settleConflict(other, args.choice);
        }
      }
    }
    this.ensureTimer();
  }

  async start_all(): Promise<void> {
    await this.latency();
    for (const record of this.tasks.values()) {
      if (record.task.state === "stopped") {
        this.setState(record, "queued");
      }
    }
    this.ensureTimer();
  }

  async stop_all(): Promise<void> {
    await this.latency();
    for (const record of this.tasks.values()) {
      if (record.task.state === "queued" || record.task.state === "running") {
        this.setState(record, "stopped");
      }
    }
  }

  async cancel_all(): Promise<void> {
    await this.latency();
    for (const record of this.tasks.values()) {
      if (record.task.state !== "completed" && record.task.state !== "cancelled") {
        this.setState(record, "cancelled");
      }
    }
    this.addLog("information", "transfer", "All unfinished tasks cancelled.");
  }

  async clear_finished(): Promise<void> {
    await this.latency();
    const ids: string[] = [];
    for (const [id, record] of this.tasks) {
      if (record.task.state === "completed" || record.task.state === "cancelled") {
        ids.push(id);
      }
    }
    for (const id of ids) {
      this.tasks.delete(id);
    }
    if (ids.length > 0) {
      this.emit("transfer://removed", { ids });
    }
  }

  async pause_task(args: CommandArgs["pause_task"]): Promise<void> {
    await this.latency();
    const record = this.getTask(args.id);
    if (record.task.state !== "queued" && record.task.state !== "running") {
      fail("conflict", "Only queued or running tasks can be paused.");
    }
    this.dropQuestion(record);
    this.setState(record, "stopped");
  }

  async resume_task(args: CommandArgs["resume_task"]): Promise<void> {
    await this.latency();
    const record = this.getTask(args.id);
    if (record.task.state !== "stopped") {
      fail("conflict", "Only stopped tasks can be resumed.");
    }
    this.setState(record, "queued");
    this.ensureTimer();
  }

  async cancel_task(args: CommandArgs["cancel_task"]): Promise<void> {
    await this.latency();
    const record = this.getTask(args.id);
    if (record.task.state === "completed" || record.task.state === "cancelled" || record.task.state === "skipped") {
      fail("conflict", "The task has already finished.");
    }
    this.dropQuestion(record);
    this.setState(record, "cancelled");
  }

  async retry_task(args: CommandArgs["retry_task"]): Promise<void> {
    await this.latency();
    const record = this.getTask(args.id);
    if (record.task.state !== "failed") {
      fail("conflict", "Only failed tasks can be retried.");
    }
    record.task.bytes_done = 0;
    this.setState(record, "queued");
    this.ensureTimer();
  }

  // ---------------------------------------------------------------- sync

  async sync_analyze(args: CommandArgs["sync_analyze"]): Promise<SyncPlan> {
    await this.latency();
    const connection = this.connect(args.account_id);
    if (args.delete && args.direction === "bidirectional") {
      fail("usage", "Deleting extra files is not available when synchronising in both directions.");
    }
    const match = /^s3:\/\/([^/]+)\/(.*)$/.exec(args.s3_prefix);
    if (!match) {
      fail("usage", 'The S3 folder must look like "s3://bucket/prefix/".');
    }
    const [, bucketName = "", prefix = ""] = match;
    this.checkPrefix(prefix);
    const target = this.bucketFor(connection, bucketName);
    const options = args.options;
    const rules = new ExcludeRules(options?.exclude ?? []);
    const included = (rel: string): boolean =>
      !rules.isExcluded(rel) &&
      (options === undefined || options.include.length === 0 || options.include.some((glob) => includeMatches(glob, rel)));
    const remote = new Map<string, MockObject>();
    for (const key of this.keysUnder(target, prefix)) {
      if (!isFolderKey(key) && included(key.slice(prefix.length))) {
        const object = target.objects.get(key);
        if (object) {
          remote.set(key.slice(prefix.length), object);
        }
      }
    }
    if (args.direction === "s3_to_s3") {
      return this.analyzeS3ToS3(connection, args, prefix, remote, included);
    }
    const root = normalizePath(args.local_root);
    if (root === null || !this.fs.isDir(root)) {
      fail("not_found", `The local folder does not exist: ${args.local_root}`);
    }
    const local = new Map<string, MockFile>();
    for (const file of this.fs.walk(root).files) {
      if (included(file.rel)) {
        local.set(file.rel, file.file);
      }
    }
    const paths = [...new Set([...local.keys(), ...remote.keys()])].sort(compareKeys);
    const rows: SyncRow[] = [];
    for (const rel of paths) {
      const row = this.planRow(rel, local.get(rel) ?? null, remote.get(rel) ?? null, prefix, args);
      if (row) {
        rows.push(row);
      }
    }
    const plan: SyncPlan = {
      id: uuid(),
      account_id: connection.account.id,
      local_root: root,
      s3_prefix: args.s3_prefix,
      direction: args.direction,
      delete: args.delete,
      created_at: nowIso(),
      rows,
    };
    this.plans.set(plan.id, { plan, executing: false, queue: [], attempts: new Map() });
    this.addLog("information", "sync", `Sync analysed: ${rows.length} row(s) for ${root} and ${args.s3_prefix}.`);
    return clone(plan);
  }

  async sync_execute(args: CommandArgs["sync_execute"]): Promise<void> {
    await this.latency();
    const record = this.getPlan(args.plan_id);
    if (record.executing) {
      fail("conflict", "This plan is already running.");
    }
    this.connect(record.plan.account_id);
    // Every copy or delete row that is not done yet, including rows that failed before.
    // Rows whose analysis failed cannot be executed.
    record.queue = [];
    record.plan.rows.forEach((row, index) => {
      if ((row.action === "copy" || row.action === "delete") && !row.done) {
        record.queue.push(index);
      }
    });
    record.executing = true;
    this.addLog("information", "sync", `Sync started: ${record.queue.length} row(s) to process.`);
    this.emitProgress(record, false);
    if (record.queue.length === 0) {
      this.finishPlan(record);
      return;
    }
    this.ensureTimer();
  }

  async sync_cancel(args: CommandArgs["sync_cancel"]): Promise<void> {
    await this.latency();
    const record = this.getPlan(args.plan_id);
    if (!record.executing) {
      return;
    }
    record.queue = [];
    record.cancelled = true;
    this.addLog("warning", "sync", "Sync cancelled; unprocessed rows can be resumed.");
    this.finishPlan(record);
  }

  async sync_get(args: CommandArgs["sync_get"]): Promise<SyncPlan> {
    await this.latency();
    return clone(this.getPlan(args.plan_id).plan);
  }

  async list_sync_jobs(): Promise<SyncJob[]> {
    await this.latency();
    return this.syncJobs.list();
  }

  async save_sync_job(args: CommandArgs["save_sync_job"]): Promise<SyncJob> {
    await this.latency();
    const saved = this.syncJobs.save(args.job, (id) => this.accounts.some((account) => account.id === id));
    this.emit("sync_jobs://changed", this.syncJobs.list());
    return saved;
  }

  async delete_sync_job(args: CommandArgs["delete_sync_job"]): Promise<SyncJob[]> {
    await this.latency();
    const left = this.syncJobs.delete(args.id);
    this.emit("sync_jobs://changed", clone(left));
    return left;
  }

  async run_sync_job(args: CommandArgs["run_sync_job"]): Promise<SyncPlan> {
    const job = this.syncJobs.find(args.id);
    return this.sync_analyze({
      account_id: job.account_id,
      local_root: job.local_root,
      s3_prefix: job.s3_prefix,
      direction: job.direction,
      delete: job.delete,
      options: job.options,
      ...(job.s3_source === undefined ? {} : { source_prefix: job.s3_source }),
    });
  }

  // ---------------------------------------------------------------- versions and preview

  async list_versions(args: CommandArgs["list_versions"]): Promise<VersionGroupPage> {
    await this.latency();
    const target = this.bucketFor(this.connect(args.account_id), args.bucket);
    return clone(this.versions.list(target, args, this.settings.advanced.list_page_size));
  }

  async restore_version(args: CommandArgs["restore_version"]): Promise<RestoreOutcome> {
    await this.latency();
    const target = this.bucketFor(this.connect(args.account_id), args.bucket);
    const { entry } = this.versions.restore(target, args.key, args.version_id);
    this.addLog("information", "s3", `Version ${args.version_id} of s3://${args.bucket}/${args.key} restored.`);
    return {
      key: args.key,
      restored_version_id: args.version_id,
      version_id: entry.version_id,
      etag: entry.etag,
      size: entry.size,
    };
  }

  async purge_versions(args: CommandArgs["purge_versions"]): Promise<DeleteOutcome> {
    await this.latency();
    const target = this.bucketFor(this.connect(args.account_id), args.bucket);
    const outcome = this.versions.purge(target, args.versions);
    this.addLog("information", "s3", `Purged ${outcome.deleted} version(s) in ${args.bucket}.`);
    return outcome;
  }

  async enqueue_download_version(args: CommandArgs["enqueue_download_version"]): Promise<TransferTask[]> {
    await this.latency();
    const connection = this.connect(args.account_id);
    const target = this.bucketFor(connection, args.bucket);
    const destination = normalizePath(args.dest_path);
    if (destination === null) {
      fail("usage", "The download file must be an absolute path.");
    }
    if (args.key.length === 0 || isFolderKey(args.key)) {
      fail("usage", "Only versions of objects can be downloaded.");
    }
    if (args.version_id.length === 0) {
      fail("usage", "The version ID is empty.");
    }
    const entry = this.versions.find(target, args.key, args.version_id);
    if (entry.is_delete_marker) {
      fail("usage", `Version ${args.version_id} is a delete marker; it has no content to download.`);
    }
    const task = this.createTask(
      connection.account.id,
      null,
      "download",
      { type: "s3", bucket: args.bucket, key: args.key },
      { type: "local", path: destination },
      entry.size,
    );
    this.versionTasks.add(task.id);
    this.addLog("information", "transfer", `Queued the download of version ${args.version_id} of s3://${args.bucket}/${args.key}.`);
    return this.announce([task]);
  }

  async preview_object(args: CommandArgs["preview_object"]): Promise<ObjectPreview> {
    await this.latency();
    const target = this.bucketFor(this.connect(args.account_id), args.bucket);
    return clone(this.versions.preview(target, args.key, args.version_id, args.page, args.page_size));
  }

  // ---------------------------------------------------------------- CloudFront

  /** Like the backend: CloudFront belongs to AWS, so an account with a custom endpoint has none. */
  private cloudfrontFor(accountId: string): MockCloudFront {
    const connection = this.connect(accountId);
    if (connection.account.endpoint !== null) {
      fail("usage", "CloudFront is not available for an account with a custom endpoint");
    }
    return this.cloudfront;
  }

  async list_cloudfront_distributions(args: CommandArgs["list_cloudfront_distributions"]): Promise<CloudFrontPage> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).list(args.marker);
  }

  async get_cloudfront_distribution(args: CommandArgs["get_cloudfront_distribution"]): Promise<CloudFrontDistribution> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).get(args.id);
  }

  async find_cloudfront_distributions(args: CommandArgs["find_cloudfront_distributions"]): Promise<CloudFrontDistribution[]> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).forBucket(args.bucket);
  }

  async create_cloudfront_distribution(args: CommandArgs["create_cloudfront_distribution"]): Promise<CreatedDistribution> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).create(args.new);
  }

  async update_cloudfront_distribution(args: CommandArgs["update_cloudfront_distribution"]): Promise<CloudFrontDistribution> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).update(args.id, args.edit, args.expected_etag);
  }

  async set_cloudfront_enabled(args: CommandArgs["set_cloudfront_enabled"]): Promise<CloudFrontDistribution> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).update(args.id, { enabled: args.enabled }, args.expected_etag);
  }

  async detach_cloudfront_bucket(args: CommandArgs["detach_cloudfront_bucket"]): Promise<CloudFrontDistribution> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).detach(args.id, args.bucket, args.expected_etag);
  }

  async create_cloudfront_invalidation(args: CommandArgs["create_cloudfront_invalidation"]): Promise<CloudFrontInvalidation> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).invalidate(args.distribution_id, args.paths);
  }

  async list_cloudfront_invalidations(args: CommandArgs["list_cloudfront_invalidations"]): Promise<InvalidationPage> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).history(args.distribution_id, args.marker);
  }

  async get_cloudfront_invalidation(args: CommandArgs["get_cloudfront_invalidation"]): Promise<CloudFrontInvalidation> {
    await this.latency();
    return this.cloudfrontFor(args.account_id).invalidation(args.distribution_id, args.invalidation_id);
  }

  async validate_invalidation_paths(args: CommandArgs["validate_invalidation_paths"]): Promise<string[]> {
    await this.latency();
    const checked = validatePaths(args.paths);
    if (!checked.ok) {
      fail("usage", checked.message);
    }
    return checked.paths;
  }

  async list_invalidation_path_sets(): Promise<PathSet[]> {
    await this.latency();
    return this.cloudfront.listSets();
  }

  async save_invalidation_path_set(args: CommandArgs["save_invalidation_path_set"]): Promise<PathSet[]> {
    await this.latency();
    return this.cloudfront.saveSet(args.set, args.replaces);
  }

  async delete_invalidation_path_set(args: CommandArgs["delete_invalidation_path_set"]): Promise<PathSet[]> {
    await this.latency();
    return this.cloudfront.deleteSet(args.name);
  }

  /** Test hook: every running invalidation completes. */
  completeInvalidations(): void {
    this.cloudfront.completeAll();
  }

  // ---------------------------------------------------------------- settings, log, app

  async get_settings(): Promise<Settings> {
    await this.latency();
    return cloneSettings(this.settings);
  }

  async save_settings(args: CommandArgs["save_settings"]): Promise<void> {
    await this.latency();
    const errors = validateSettings(args.settings);
    if (hasErrors(errors)) {
      const problems = Object.entries(errors).map(([field, message]) => `${field}: ${message}`);
      fail("usage", `The settings are not valid. ${problems.join(" ")}`);
    }
    const next = cloneSettings(args.settings);
    // The password reference is owned by set_proxy_password.
    next.proxy.password_ref = this.settings.proxy.password_ref;
    // Likewise the shortener token reference belongs to set_shortener_token.
    next.shortener.token_ref = this.settings.shortener.token_ref;
    this.settings = next;
    this.addLog("information", "settings", "Settings saved.");
    this.ensureTimer();
  }

  async set_proxy_password(args: CommandArgs["set_proxy_password"]): Promise<void> {
    await this.latency();
    this.requireUnlocked();
    this.proxyPassword = args.password;
    this.settings.proxy.password_ref = args.password === null ? null : "bucketbridge/proxy/password";
    this.addLog("information", "settings", args.password === null ? "Proxy password removed." : "Proxy password saved.");
  }

  async set_shortener_token(args: CommandArgs["set_shortener_token"]): Promise<void> {
    await this.latency();
    this.requireUnlocked();
    this.shortenerToken = args.token;
    this.settings.shortener.token_ref = args.token === null ? null : "bucketbridge/shortener/token";
    this.addLog("information", "settings", args.token === null ? "Shortener token removed." : "Shortener token saved.");
  }

  async shorten_url(args: CommandArgs["shorten_url"]): Promise<string> {
    await this.latency();
    const provider = this.settings.shortener.provider;
    if (provider === "none") {
      fail("usage", "No URL shortener is configured.");
    }
    if (provider === "bitly" && this.shortenerToken === null) {
      fail("usage", "Bitly needs an access token: add it in the settings.");
    }
    if (!/^https?:\/\/\S+$/i.test(args.url)) {
      fail("usage", "Only http and https links can be shortened.");
    }
    this.shortened.push(args.url);
    const host = provider === "bitly" ? "bit.ly" : "tinyurl.com";
    return `https://${host}/m${this.shortened.length}`;
  }

  /** For tests: the links sent to the shortener so far. */
  shortenedLinks(): readonly string[] {
    return this.shortened;
  }

  /** For tests: the proxy password the backend holds (never sent to the UI). */
  storedProxyPassword(): string | null {
    return this.proxyPassword;
  }

  async get_log(args: CommandArgs["get_log"]): Promise<LogEntry[]> {
    await this.latency();
    const needle = args.text?.trim().toLocaleLowerCase() ?? "";
    let entries = this.logEntries.filter(
      (entry) =>
        (args.levels === null || args.levels.includes(entry.level)) &&
        (needle.length === 0 ||
          entry.message.toLocaleLowerCase().includes(needle) ||
          entry.source.toLocaleLowerCase().includes(needle)),
    );
    if (args.limit !== null) {
      if (!Number.isInteger(args.limit) || args.limit < 0) {
        fail("usage", "The limit must be a non-negative integer.");
      }
      entries = args.limit === 0 ? [] : entries.slice(-args.limit);
    }
    return clone(entries);
  }

  async export_log(args: CommandArgs["export_log"]): Promise<void> {
    await this.latency();
    const path = normalizePath(args.path);
    if (path === null) {
      fail("usage", "The export path must be absolute.");
    }
    let text: string;
    if (args.format === "json") {
      text = JSON.stringify(this.logEntries, null, 2);
    } else {
      const quote = (value: string) => `"${value.replace(/"/g, '""')}"`;
      text = [
        "id,ts,level,source,message",
        ...this.logEntries.map((entry) =>
          [entry.id, entry.ts, entry.level, quote(entry.source), quote(entry.message)].join(","),
        ),
      ].join("\n");
    }
    this.fs.writeFile(path, { size: new TextEncoder().encode(text).length, modified: nowIso() });
    this.addLog("information", "app", `Event log exported to ${path}.`);
  }

  async clear_log(): Promise<void> {
    await this.latency();
    this.logEntries = [];
  }

  async app_info(): Promise<AppInfo> {
    await this.latency();
    return {
      name: "BucketBridge",
      version: "0.3.0",
      license: "GPL-3.0-only",
      repository_url: "https://github.com/anupshaw50/BucketBridge",
      config_dir: `${MOCK_HOME}/.config/bucketbridge`,
      data_dir: `${MOCK_HOME}/.local/share/bucketbridge`,
      log_dir: `${MOCK_HOME}/.local/share/bucketbridge/logs`,
    };
  }

  async last_crash_report(): Promise<CrashReport | null> {
    await this.latency();
    return this.crash === null ? null : clone(this.crash);
  }

  async take_pending_link(): Promise<LinkAction | null> {
    await this.latency();
    const action = this.pendingLink;
    this.pendingLink = null;
    return action === null ? null : clone(action);
  }

  async quit_app(args: CommandArgs["quit_app"]): Promise<void> {
    await this.latency();
    this.quitCalls.push(args.mode);
  }

  async dismiss_crash_report(args: CommandArgs["dismiss_crash_report"]): Promise<void> {
    await this.latency();
    if (this.crash?.path !== args.path) {
      fail("not_found", "There is no crash report at that path.");
    }
    this.crash = null;
  }

  // ---------------------------------------------------------------- tools and export (T12)

  async run_diagnostics(args: CommandArgs["run_diagnostics"]): Promise<DiagnosticsReport> {
    await this.latency();
    return this.tools.runDiagnostics(args);
  }

  async compute_bucket_usage(args: CommandArgs["compute_bucket_usage"]): Promise<BucketUsage> {
    await this.latency();
    return this.usage.compute(args);
  }

  async cancel_bucket_usage(args: CommandArgs["cancel_bucket_usage"]): Promise<void> {
    await this.latency();
    this.usage.cancel(args.job_id);
  }

  async list_multipart_uploads(args: CommandArgs["list_multipart_uploads"]): Promise<UploadListing> {
    await this.latency();
    return this.tools.listUploads(args);
  }

  async abort_multipart_uploads(args: CommandArgs["abort_multipart_uploads"]): Promise<AbortResult[]> {
    await this.latency();
    return this.tools.abortUploads(args);
  }

  async iam_status(args: CommandArgs["iam_status"]) {
    await this.latency();
    return this.iam.status(args);
  }

  async iam_list_users(args: CommandArgs["iam_list_users"]) {
    await this.latency();
    return this.iam.listUsers(args);
  }

  async iam_create_user(args: CommandArgs["iam_create_user"]) {
    await this.latency();
    return this.iam.createUser(args);
  }

  async iam_update_user(args: CommandArgs["iam_update_user"]) {
    await this.latency();
    return this.iam.updateUser(args);
  }

  async iam_delete_user(args: CommandArgs["iam_delete_user"]) {
    await this.latency();
    return this.iam.deleteUser(args);
  }

  async iam_user_details(args: CommandArgs["iam_user_details"]) {
    await this.latency();
    return this.iam.details(args);
  }

  async iam_list_managed_policies(args: CommandArgs["iam_list_managed_policies"]) {
    await this.latency();
    return this.iam.managedPolicies(args);
  }

  async iam_attach_policy(args: CommandArgs["iam_attach_policy"]) {
    await this.latency();
    return this.iam.attach(args);
  }

  async iam_detach_policy(args: CommandArgs["iam_detach_policy"]) {
    await this.latency();
    return this.iam.detach(args);
  }

  async iam_get_inline_policy(args: CommandArgs["iam_get_inline_policy"]) {
    await this.latency();
    return this.iam.getInline(args);
  }

  async iam_put_inline_policy(args: CommandArgs["iam_put_inline_policy"]) {
    await this.latency();
    return this.iam.putInline(args);
  }

  async iam_delete_inline_policy(args: CommandArgs["iam_delete_inline_policy"]) {
    await this.latency();
    return this.iam.deleteInline(args);
  }

  async iam_create_access_key(args: CommandArgs["iam_create_access_key"]) {
    await this.latency();
    return this.iam.createKey(args);
  }

  async iam_list_access_keys(args: CommandArgs["iam_list_access_keys"]) {
    await this.latency();
    return this.iam.listKeys(args);
  }

  async iam_set_access_key_active(args: CommandArgs["iam_set_access_key_active"]) {
    await this.latency();
    return this.iam.setKeyActive(args);
  }

  async iam_delete_access_key(args: CommandArgs["iam_delete_access_key"]) {
    await this.latency();
    return this.iam.deleteKey(args);
  }

  async iam_create_login_profile(args: CommandArgs["iam_create_login_profile"]) {
    await this.latency();
    return this.iam.createProfile(args);
  }

  async iam_update_login_profile(args: CommandArgs["iam_update_login_profile"]) {
    await this.latency();
    return this.iam.updateProfile(args);
  }

  async iam_delete_login_profile(args: CommandArgs["iam_delete_login_profile"]) {
    await this.latency();
    return this.iam.deleteProfile(args);
  }

  async iam_validate_policy(args: CommandArgs["iam_validate_policy"]): Promise<PolicyIssue[]> {
    await this.latency();
    return checkUserPolicy(args.document);
  }

  async iam_format_policy(args: CommandArgs["iam_format_policy"]): Promise<string> {
    await this.latency();
    try {
      return JSON.stringify(JSON.parse(args.document), null, 2);
    } catch (error) {
      throw new BackendError("usage", `not valid JSON: ${String(error)}`);
    }
  }

  async list_filters(): Promise<SavedFilter[]> {
    await this.latency();
    return this.tools.listFilters();
  }

  async save_filter(args: CommandArgs["save_filter"]): Promise<SavedFilter[]> {
    await this.latency();
    return this.tools.saveFilter(args);
  }

  async delete_filter(args: CommandArgs["delete_filter"]): Promise<SavedFilter[]> {
    await this.latency();
    return this.tools.deleteFilter(args);
  }

  async mount_status(): Promise<MountStatus> {
    await this.latency();
    const supported = this.options.mountSupported;
    const windows = this.options.mountPlatform === "windows";
    return {
      supported,
      fuse_installed: supported,
      platform: this.options.mountPlatform,
      drive_letters: supported && windows ? [...MOCK_DRIVE_LETTERS] : [],
      mounts: [...this.mounts.values()],
    };
  }

  async mount_bucket(args: CommandArgs["mount_bucket"]): Promise<MountInfo> {
    await this.latency();
    const problem = validateBucketName(args.bucket);
    if (problem !== null) {
      fail("usage", `${args.bucket}: ${problem}`);
    }
    if (!this.options.mountSupported) {
      fail("usage", "this build of BucketBridge cannot mount buckets (build with the mount feature)");
    }
    let point: string | null;
    if (this.options.mountPlatform === "windows") {
      const letter = /^([A-Za-z]):?[\\/]?$/.exec(args.mount_point.trim())?.[1]?.toUpperCase();
      if (letter !== undefined) {
        if (!MOCK_DRIVE_LETTERS.includes(letter)) {
          fail("usage", `${letter}: is not a free drive letter`);
        }
        point = `${letter}:`;
      } else {
        point = /^[A-Za-z]:[\\/]/.test(args.mount_point) ? args.mount_point : null;
      }
    } else {
      point = normalizePath(args.mount_point);
    }
    if (point === null) {
      fail("usage", "the mount point must be an absolute path");
    }
    if (this.mounts.has(point)) {
      fail("conflict", `${point} is already a mount`);
    }
    this.connect(args.account_id);
    const info: MountInfo = {
      account_id: args.account_id,
      bucket: args.bucket,
      prefix: args.prefix ?? "",
      mount_point: point,
    };
    this.mounts.set(point, info);
    this.emit("mount://changed", { mounts: [...this.mounts.values()] });
    return info;
  }

  async unmount_bucket(args: CommandArgs["unmount_bucket"]): Promise<void> {
    await this.latency();
    const point =
      this.options.mountPlatform === "windows" ? args.mount_point : (normalizePath(args.mount_point) ?? args.mount_point);
    if (!this.mounts.delete(point)) {
      fail("not_found", `${args.mount_point} is not a mount`);
    }
    this.emit("mount://changed", { mounts: [...this.mounts.values()] });
  }

  async run_security_scan(args: CommandArgs["run_security_scan"]): Promise<ScanReport> {
    await this.latency();
    let names: string[];
    if (args.buckets === null) {
      names = (await this.list_buckets({ account_id: args.account_id })).map((bucket) => bucket.name);
    } else {
      if (args.buckets.length === 0) {
        fail("usage", "Choose at least one bucket.");
      }
      for (const name of args.buckets) {
        const problem = validateBucketName(name);
        if (problem !== null) {
          fail("usage", `${name}: ${problem}`);
        }
      }
      this.connect(args.account_id);
      names = args.buckets;
    }
    return scanReport(names, nowIso());
  }

  async export_scan_report(args: CommandArgs["export_scan_report"]): Promise<void> {
    await this.latency();
    const path = normalizePath(args.path);
    if (path === null) {
      fail("usage", "the report path must be an absolute path");
    }
    this.fs.writeFile(path, { size: reportBytes(args.report, args.format), modified: nowIso() });
  }

  async export_listing(args: CommandArgs["export_listing"]): Promise<ExportOutcome> {
    await this.latency();
    return this.tools.exportListing(args);
  }

  // ---------------------------------------------------------------- internals: access

  private async latency(): Promise<void> {
    if (this.disposed) {
      fail("internal", "The mock backend was shut down.");
    }
    if (this.options.latencyMs > 0) {
      await sleep(this.options.latencyMs * (0.5 + Math.random()));
    }
  }

  private securityStatus(): SecurityStatus {
    return { master_password_set: this.masterPassword !== null, unlocked: this.unlocked };
  }

  private requireUnlocked(): void {
    if (!this.unlocked) {
      fail("locked", "Secrets are locked. Enter the master password to unlock them.");
    }
  }

  private requireUniqueLabel(label: string, exceptId: string | null): void {
    const wanted = label.toLowerCase();
    if (this.accounts.some((account) => account.id !== exceptId && account.label.toLowerCase() === wanted)) {
      fail("conflict", `An account labelled "${label}" already exists.`);
    }
  }

  private checkRegion(region: string): void {
    const error = validateRegion(region);
    if (error) {
      fail("usage", error);
    }
  }

  private checkEndpoint(endpoint: string): void {
    const error = validateEndpoint(endpoint);
    if (error) {
      fail("usage", error);
    }
  }

  private checkPrefix(prefix: string): void {
    if (prefix.length > 0 && !prefix.endsWith("/")) {
      fail("usage", "A destination prefix must be empty or end with a slash.");
    }
  }

  private requireKeys(keys: readonly string[]): void {
    if (keys.length === 0) {
      fail("usage", "Select at least one item.");
    }
  }

  private getAccount(id: string): Account {
    return this.accounts.find((account) => account.id === id) ?? fail("not_found", "The account does not exist.");
  }

  private getTask(id: string): TaskRecord {
    return this.tasks.get(id) ?? fail("not_found", "The task does not exist.");
  }

  private getPlan(id: string): PlanRecord {
    return this.plans.get(id) ?? fail("not_found", "The sync plan does not exist.");
  }

  private checkCredentials(account: Account): { provider: Provider; owner: string } {
    const provider =
      this.providers.get(account.endpoint ?? AWS_PROVIDER) ??
      fail("remote", `Could not connect to ${account.endpoint ?? "the endpoint"}: connection refused.`);
    if (account.source.type !== "static_keys") {
      // Profiles, roles, SSO and the instance role all reach the demo identity in the mock.
      const demo = provider.credentials.get(MOCK_CREDENTIALS.demo.accessKeyId);
      if (demo === undefined) {
        fail("auth", "The credentials of this account could not be loaded.");
      }
      return { provider, owner: demo.owner };
    }
    if (account.secret_ref === "") {
      fail("usage", `account "${account.label}" has no secret access key yet (it was imported without secrets): enter it in the account settings`);
    }
    const credential = provider.credentials.get(account.access_key_id);
    if (!credential) {
      this.addLog("error", "s3", `Access key of "${account.label}" was rejected (InvalidAccessKeyId).`);
      fail("auth", "The access key ID does not exist (InvalidAccessKeyId).");
    }
    if (this.secrets.get(account.secret_ref) !== credential.secret) {
      this.addLog("error", "s3", `Request signature of "${account.label}" did not match (SignatureDoesNotMatch).`);
      fail("auth", "The request signature does not match. Check the secret access key (SignatureDoesNotMatch).");
    }
    return { provider, owner: credential.owner };
  }

  /** Resolves an account for an S3 call, applying lock, MFA and credential checks. */
  private connect(accountId: string): Connection {
    const account = this.getAccount(accountId);
    this.requireAccess(account);
    const { provider, owner } = this.checkCredentials(account);
    return { account, provider, owner };
  }

  /** The lock and MFA rules every account-bound command applies before it touches the account's server. */
  private requireAccess(account: Account): void {
    this.requireUnlocked();
    if (account.mfa_serial !== null) {
      const expires = this.mfaSessions.get(account.id);
      if (expires === undefined || expires < Date.now()) {
        fail("mfa_required", `"${account.label}" needs a code from its MFA device.`);
      }
    }
  }

  private bucketFor(connection: Connection, name: string): MockBucket {
    const target = connection.provider.buckets.get(name);
    if (!target) {
      fail("not_found", `The bucket ${name} does not exist (NoSuchBucket).`);
    }
    if (target.owner !== connection.owner && !(target.sharedWith ?? []).includes(connection.owner)) {
      fail("auth", `Access to ${name} was denied (AccessDenied).`);
    }
    return target;
  }

  private sortedKeys(target: MockBucket): string[] {
    return [...target.objects.keys()].sort(compareKeys);
  }

  private keysUnder(target: MockBucket, prefix: string): string[] {
    return this.sortedKeys(target).filter((key) => key.startsWith(prefix));
  }

  private findBucket(accountId: string, name: string): MockBucket | null {
    const account = this.accounts.find((item) => item.id === accountId);
    if (!account) {
      return null;
    }
    return this.providers.get(account.endpoint ?? AWS_PROVIDER)?.buckets.get(name) ?? null;
  }

  /** Where a bucket's configuration lives in the mock cloud. */
  private slot(connection: Connection, target: MockBucket): BucketSlot {
    return this.slotOf(connection, target.name);
  }

  private slotOf(connection: Connection, bucket: string): BucketSlot {
    const provider = connection.account.endpoint ?? AWS_PROVIDER;
    return { key: `${provider}/${bucket}`, bucket, custom: connection.account.endpoint !== null };
  }

  /** The slot for a configuration command: the account must reach the bucket, and the name must be valid. */
  private configSlot(accountId: string, bucket: string): BucketSlot {
    const connection = this.connect(accountId);
    this.checkExistingName(bucket);
    return this.slot(connection, this.bucketFor(connection, bucket));
  }

  private checkExistingName(name: string): void {
    const problem = checkExistingBucketName(name);
    if (problem !== null) {
      fail("usage", problem);
    }
  }

  // ---------------------------------------------------------------- internals: tasks

  private createTask(
    accountId: string,
    groupId: string | null,
    kind: TransferKind,
    src: TransferTask["src"],
    dst: TransferTask["dst"],
    total: number,
  ): TransferTask {
    const now = nowIso();
    const task: TransferTask = {
      id: uuid(),
      account_id: accountId,
      group_id: groupId,
      kind,
      src,
      dst,
      bytes_total: total,
      bytes_done: 0,
      speed: 0,
      eta: null,
      state: "queued",
      error: null,
      attempts: 0,
      created_at: now,
      updated_at: now,
    };
    return task;
  }

  /** Rejects with conflict when two selected sources would land on the same destination. */
  private requireDistinctDestinations(destinations: readonly string[], where: string): void {
    const [first] = collidingNames(destinations);
    if (first !== undefined) {
      fail(
        "conflict",
        `Two or more selected items would be written to the same place: "${first}" in ${where}. Nothing was queued.`,
      );
    }
  }

  private enqueueCopyOrMove(kind: "copy" | "move", args: CommandArgs["enqueue_copy"]): TransferTask[] {
    const connection = this.connect(args.account_id);
    const source = this.bucketFor(connection, args.src_bucket);
    const dstAccount = args.dst_account_id ?? null;
    const crossAccount = dstAccount !== null && dstAccount !== args.account_id;
    this.bucketFor(crossAccount ? this.connect(dstAccount) : connection, args.dst_bucket);
    this.checkPrefix(args.dst_prefix);
    this.requireKeys(args.keys);
    this.requireDistinctDestinations(
      args.keys.map((key) => destinationKey(key, args.dst_prefix)),
      `s3://${args.dst_bucket}/${args.dst_prefix}`,
    );
    const created: TransferTask[] = [];
    for (const key of args.keys) {
      if (args.src_bucket === args.dst_bucket && !crossAccount) {
        if (parentPrefix(key) === args.dst_prefix) {
          fail("usage", "The source and the destination are the same folder.");
        }
        if (isFolderKey(key) && args.dst_prefix.startsWith(key)) {
          fail("usage", "A folder cannot be copied or moved into itself.");
        }
      }
      if (isFolderKey(key)) {
        const keys = this.keysUnder(source, key);
        if (keys.length === 0) {
          fail("not_found", `No objects found under s3://${args.src_bucket}/${key}`);
        }
        const group = uuid();
        const root = `${args.dst_prefix}${baseName(key)}/`;
        for (const objectKey of keys) {
          created.push(
            this.createTask(connection.account.id, group, kind, {
              type: "s3",
              bucket: args.src_bucket,
              key: objectKey,
            }, {
              type: "s3",
              bucket: args.dst_bucket,
              key: `${root}${objectKey.slice(key.length)}`,
            }, source.objects.get(objectKey)?.size ?? 0),
          );
        }
      } else {
        const object = source.objects.get(key);
        if (!object) {
          fail("not_found", `The object does not exist: s3://${args.src_bucket}/${key}`);
        }
        created.push(
          this.createTask(connection.account.id, null, kind, { type: "s3", bucket: args.src_bucket, key }, {
            type: "s3",
            bucket: args.dst_bucket,
            key: `${args.dst_prefix}${baseName(key)}`,
          }, object.size),
        );
      }
    }
    this.addLog(
      "information",
      "transfer",
      `Queued ${created.length} ${kind} task(s) to s3://${args.dst_bucket}/${args.dst_prefix}.`,
    );
    return this.announce(created, { ifExists: args.if_exists ?? null, dstAccount: crossAccount ? dstAccount : null });
  }

  /** Queues the new tasks (only once every check passed), emits them and returns copies. */
  private announce(created: TransferTask[], options: JobOptions = NO_OPTIONS): TransferTask[] {
    for (const task of created) {
      this.tasks.set(task.id, { task, deleteKeys: null, options });
    }
    for (const task of created) {
      this.emit("transfer://update", task);
    }
    this.ensureTimer();
    return created.map((task) => clone(task));
  }

  private setState(record: TaskRecord, state: TransferTask["state"], error: string | null = null): void {
    const task = record.task;
    task.state = state;
    task.error = error;
    task.speed = 0;
    task.eta = null;
    task.updated_at = nowIso();
    this.emit("transfer://update", task);
  }

  private failTask(record: TaskRecord, message: string): void {
    this.setState(record, "failed", message);
    this.addLog("error", "transfer", `${this.describe(record.task)} failed: ${message}`);
  }

  private describe(task: TransferTask): string {
    const where = (location: TransferTask["src"] | null) =>
      location === null ? "" : location.type === "local" ? location.path : `s3://${location.bucket}/${location.key}`;
    const verb = { upload: "Upload", download: "Download", copy: "Copy", move: "Move", delete: "Delete" }[task.kind];
    return task.dst === null ? `${verb} of ${where(task.src)}` : `${verb} of ${where(task.src)} to ${where(task.dst)}`;
  }

  private ensureTimer(): void {
    if (!this.options.autoRun || this.timer !== null || this.disposed || !this.hasWork()) {
      return;
    }
    this.timer = setInterval(() => {
      this.step(this.options.tickMs);
      if (!this.hasWork()) {
        this.stopTimer();
      }
    }, this.options.tickMs);
  }

  private stopTimer(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  private hasWork(): boolean {
    for (const record of this.tasks.values()) {
      if (record.task.state === "queued" || record.task.state === "running") {
        return true;
      }
    }
    for (const record of this.plans.values()) {
      if (record.executing) {
        return true;
      }
    }
    return false;
  }

  /** One simulation step of `dt` milliseconds. */
  private step(dt: number): void {
    this.promote();
    const running = [...this.tasks.values()].filter((record) => record.task.state === "running");
    const limit = this.settings.transfer.bandwidth_limit_kibps;
    const bandwidthUsers = running.filter((record) => record.task.kind === "upload" || record.task.kind === "download");
    const share = limit === null ? Infinity : (limit * 1024) / Math.max(1, bandwidthUsers.length);
    for (const record of running) {
      this.progress(record, dt, share);
    }
    this.promote();
    for (const plan of this.plans.values()) {
      if (plan.executing) {
        this.stepPlan(plan, dt);
      }
    }
  }

  /** Starts queued tasks while fewer than max_concurrent_tasks are running. */
  private promote(): void {
    let running = 0;
    for (const record of this.tasks.values()) {
      if (record.task.state === "running") {
        running += 1;
      }
    }
    const limit = this.settings.transfer.max_concurrent_tasks;
    for (const record of this.tasks.values()) {
      if (running >= limit) {
        return;
      }
      if (record.task.state === "queued" && this.start(record)) {
        running += 1;
      }
    }
  }

  /** Checks preconditions and moves a task to running; returns false if it did not start. */
  private start(record: TaskRecord): boolean {
    const task = record.task;
    if (!this.unlocked) {
      return false; // waits in the queue until the secrets are unlocked
    }
    try {
      this.connect(task.account_id);
    } catch (error) {
      this.failTask(record, error instanceof Error ? error.message : "The account cannot be used.");
      return false;
    }
    const src = task.src;
    if (task.kind === "upload" && src.type === "local") {
      if (!this.fs.exists(src.path)) {
        this.failTask(record, "No such file or directory (os error 2)");
        return false;
      }
      if (nameMatches(src.path, "unreadable")) {
        this.failTask(record, "Permission denied (os error 13)");
        return false;
      }
      const file = this.fs.stat(src.path);
      task.bytes_total = file?.size ?? 0;
    } else if (src.type === "s3") {
      const source = this.findBucket(task.account_id, src.bucket);
      if (task.kind === "delete") {
        record.deleteKeys =
          source === null ? [] : isFolderKey(src.key) ? this.keysUnder(source, src.key) : [src.key];
        task.bytes_total = record.deleteKeys.length;
        task.bytes_done = Math.min(task.bytes_done, task.bytes_total);
      } else if (!source?.objects.has(src.key) && !this.versionTasks.has(task.id)) {
        this.failTask(record, "The specified key does not exist (NoSuchKey).");
        return false;
      }
      if ((task.kind === "copy" || task.kind === "move") && task.dst?.type === "s3") {
        if (this.findBucket(record.options?.dstAccount ?? task.account_id, task.dst.bucket) === null) {
          this.failTask(record, "The destination bucket does not exist (NoSuchBucket).");
          return false;
        }
      }
    }
    task.attempts += 1;
    task.state = "running";
    task.error = null;
    task.updated_at = nowIso();
    this.emit("transfer://update", task);
    this.placeTask(record);
    return record.task.state === "running";
  }

  // ---------------------------------------------------------------- internals: overwrite policy

  /** The time of what the task reads, and of what it would replace (ms since the epoch). */
  private timesOf(record: TaskRecord): { exists: boolean; source: number | null; destination: number | null } {
    const task = record.task;
    const parse = (value: string | undefined) => (value === undefined ? null : Date.parse(value));
    const dst = task.dst;
    const src = task.src;
    if (dst === null || isFolderKey(dst.type === "s3" ? dst.key : "") || (src.type === "s3" && isFolderKey(src.key))) {
      return { exists: false, source: null, destination: null };
    }
    if (task.kind === "upload" && src.type === "local" && dst.type === "s3") {
      const object = this.findBucket(task.account_id, dst.bucket)?.objects.get(dst.key);
      return {
        exists: object !== undefined,
        source: parse(this.fs.stat(src.path)?.modified),
        destination: parse(object?.last_modified),
      };
    }
    if (task.kind === "download" && src.type === "s3" && dst.type === "local") {
      const object = this.findBucket(task.account_id, src.bucket)?.objects.get(src.key);
      const file = this.fs.stat(dst.path);
      return { exists: file !== null, source: parse(object?.last_modified), destination: parse(file?.modified) };
    }
    if ((task.kind === "copy" || task.kind === "move") && src.type === "s3" && dst.type === "s3") {
      const object = this.findBucket(task.account_id, src.bucket)?.objects.get(src.key);
      const existing = this.findBucket(record.options?.dstAccount ?? task.account_id, dst.bucket)?.objects.get(dst.key);
      return { exists: existing !== undefined, source: parse(object?.last_modified), destination: parse(existing?.last_modified) };
    }
    return { exists: false, source: null, destination: null };
  }

  /** Applies the overwrite policy to a task that just started: it goes on, is skipped, is renamed, or waits. */
  private placeTask(record: TaskRecord): void {
    const task = record.task;
    if (task.kind === "delete" || task.dst === null) {
      return;
    }
    const policy = record.options?.ifExists ?? this.settings.transfer.if_exists;
    if (policy === "overwrite") {
      return;
    }
    const times = this.timesOf(record);
    if (!times.exists) {
      return;
    }
    if (policy === "ask") {
      const group = task.group_id;
      const remembered = group === null ? undefined : this.batchAnswers.get(group);
      if (remembered !== undefined) {
        this.applyChoice(record, remembered, times);
        return;
      }
      const side = (location: TransferTask["src"]) => (location.type === "local" ? location.path : `s3://${location.bucket}/${location.key}`);
      const pending: PendingConflict = {
        task_id: task.id,
        group_id: group,
        conflict: {
          kind: task.kind,
          source: side(task.src),
          destination: side(task.dst),
          source_info: {
            size: task.bytes_total,
            modified: times.source === null ? null : new Date(times.source).toISOString(),
          },
          destination_info: {
            size: null,
            modified: times.destination === null ? null : new Date(times.destination).toISOString(),
          },
        },
      };
      record.waiting = pending;
      this.emit("transfer://conflict", { type: "asked", ...pending });
      return;
    }
    this.applyChoice(record, policy, times);
  }

  private applyChoice(
    record: TaskRecord,
    choice: OverwriteChoice,
    times: { source: number | null; destination: number | null },
  ): void {
    const task = record.task;
    if (choice === "overwrite") {
      return;
    }
    const newer = times.source !== null && times.destination !== null && Math.floor(times.source / 1000) > Math.floor(times.destination / 1000);
    if (choice === "skip" || (choice === "overwrite_if_newer" && !newer)) {
      const reason =
        choice === "skip" ? "the destination already exists" : "the destination exists and the source is not newer";
      this.setState(record, "skipped", reason);
      this.addLog("information", "transfer", `${this.describe(task)} skipped: ${reason}.`);
      return;
    }
    if (choice === "rename" && task.dst !== null) {
      task.dst = this.renamedDestination(record, task.dst);
      task.updated_at = nowIso();
      this.emit("transfer://update", task);
    }
  }

  /** `name (1).ext`, `name (2).ext`, ... until the destination is free. */
  private renamedDestination(record: TaskRecord, dst: NonNullable<TransferTask["dst"]>): NonNullable<TransferTask["dst"]> {
    const numbered = (name: string, n: number) => {
      const dot = name.lastIndexOf(".");
      return dot > 0 && dot + 1 < name.length ? `${name.slice(0, dot)} (${n})${name.slice(dot)}` : `${name} (${n})`;
    };
    const free = (candidate: NonNullable<TransferTask["dst"]>): boolean => {
      if (candidate.type === "local") {
        return !this.fs.exists(candidate.path);
      }
      const account = record.options?.dstAccount ?? record.task.account_id;
      return !this.findBucket(account, candidate.bucket)?.objects.has(candidate.key);
    };
    for (let n = 1; n < 10_000; n += 1) {
      const candidate: NonNullable<TransferTask["dst"]> =
        dst.type === "local"
          ? { type: "local", path: `${dst.path.slice(0, dst.path.length - baseName(dst.path).length)}${numbered(baseName(dst.path), n)}` }
          : { type: "s3", bucket: dst.bucket, key: `${dst.key.slice(0, dst.key.length - baseName(dst.key).length)}${numbered(baseName(dst.key), n)}` };
      if (free(candidate)) {
        return candidate;
      }
    }
    return dst;
  }

  private settleConflict(record: TaskRecord, choice: OverwriteChoice): void {
    record.waiting = undefined;
    this.emit("transfer://conflict", { type: "resolved", task_id: record.task.id } satisfies ConflictEvent);
    this.applyChoice(record, choice, this.timesOf(record));
  }

  /** A task that stops waiting (paused or cancelled) no longer asks. */
  private dropQuestion(record: TaskRecord): void {
    if (record.waiting !== undefined) {
      record.waiting = undefined;
      this.emit("transfer://conflict", { type: "resolved", task_id: record.task.id } satisfies ConflictEvent);
    }
  }

  private progress(record: TaskRecord, dt: number, share: number): void {
    const task = record.task;
    if (record.waiting !== undefined) {
      return;
    }
    const total = task.bytes_total ?? 0;
    let rate: number;
    if (task.kind === "delete") {
      rate = DELETE_OBJECTS_PER_SECOND;
    } else if (task.kind === "copy" || task.kind === "move") {
      rate = this.options.bytesPerSecond * SERVER_COPY_FACTOR;
    } else {
      rate = Math.min(this.options.bytesPerSecond, share);
    }
    const before = task.bytes_done;
    const after = Math.min(total, before + (rate * dt) / 1000);
    const location = task.src.type === "local" ? task.src.path : task.src.key;
    if (nameMatches(location, "flaky") && task.attempts === 1 && total > 0 && after >= total / 2) {
      this.failTask(record, "Connection reset by peer (os error 54)");
      return;
    }
    if (after >= total) {
      this.complete(record);
      return;
    }
    task.bytes_done = Math.floor(after);
    const instant = ((task.bytes_done - before) * 1000) / dt;
    task.speed = Math.round(task.speed === 0 ? instant : task.speed * 0.7 + instant * 0.3);
    task.eta = task.speed > 0 ? Math.ceil((total - task.bytes_done) / task.speed) : null;
    task.updated_at = nowIso();
    this.emit("transfer://update", task);
  }

  private complete(record: TaskRecord): void {
    const task = record.task;
    const algorithm = this.settings.data_integrity.checksum_algorithm;
    const src = task.src;
    const dst = task.dst;
    if (task.kind === "upload" && src.type === "local" && dst?.type === "s3") {
      const target = this.findBucket(task.account_id, dst.bucket);
      if (!target) {
        this.failTask(record, "The bucket does not exist (NoSuchBucket).");
        return;
      }
      const file = this.fs.stat(src.path);
      target.objects.set(dst.key, makeObject(dst.key, file?.size ?? 0, nowIso(), algorithm));
    } else if (task.kind === "download" && src.type === "s3" && dst?.type === "local") {
      const object = this.findBucket(task.account_id, src.bucket)?.objects.get(src.key);
      try {
        if (isFolderKey(src.key)) {
          this.fs.mkdirp(dst.path);
        } else {
          const size = this.versionTasks.has(task.id) ? (task.bytes_total ?? 0) : (object?.size ?? 0);
          this.fs.writeFile(dst.path, { size, modified: nowIso() });
        }
      } catch (error) {
        this.failTask(record, error instanceof Error ? error.message : "Could not write the file.");
        return;
      }
    } else if ((task.kind === "copy" || task.kind === "move") && src.type === "s3" && dst?.type === "s3") {
      const source = this.findBucket(task.account_id, src.bucket);
      const target = this.findBucket(record.options?.dstAccount ?? task.account_id, dst.bucket);
      const object = source?.objects.get(src.key);
      if (!object || !target || !source) {
        this.failTask(record, "The specified key does not exist (NoSuchKey).");
        return;
      }
      target.objects.set(dst.key, { ...object, checksums: { ...object.checksums }, last_modified: nowIso() });
      if (task.kind === "move") {
        source.objects.delete(src.key);
      }
    } else if (task.kind === "delete" && src.type === "s3") {
      const source = this.findBucket(task.account_id, src.bucket);
      for (const key of record.deleteKeys ?? []) {
        source?.objects.delete(key);
      }
    }
    task.bytes_done = task.bytes_total ?? task.bytes_done;
    task.state = "completed";
    task.speed = 0;
    task.eta = null;
    task.error = null;
    task.updated_at = nowIso();
    this.emit("transfer://update", task);
    this.addLog("information", "transfer", `${this.describe(task)} completed.`);
  }

  // ---------------------------------------------------------------- internals: sync

  private planRow(
    rel: string,
    local: MockFile | null,
    remote: MockObject | null,
    prefix: string,
    args: CommandArgs["sync_analyze"],
  ): SyncRow | null {
    const localMeta = local ? { size: local.size, modified: local.modified } : null;
    const s3Meta = remote ? objectEntry(`${prefix}${rel}`, remote) : null;
    const row = (action: SyncRow["action"], target: SyncRow["target"], error: string | null = null): SyncRow => ({
      rel_path: rel,
      action,
      target,
      local_meta: localMeta,
      s3_meta: s3Meta,
      error,
      done: action === "equal",
    });
    if (local && nameMatches(rel, "unreadable")) {
      return row("failed", null, "Permission denied (os error 13)");
    }
    if (local && remote) {
      const localTime = parseTimestamp(local.modified) ?? 0;
      const remoteTime = parseTimestamp(remote.last_modified) ?? 0;
      if (local.size === remote.size && Math.abs(localTime - remoteTime) <= 1000) {
        return row("equal", null);
      }
      if (args.direction === "upload") return row("copy", "remote");
      if (args.direction === "download") return row("copy", "local");
      return row("copy", localTime > remoteTime ? "remote" : "local");
    }
    if (local) {
      if (args.direction === "download") return args.delete ? row("delete", "local") : null;
      return row("copy", "remote");
    }
    if (remote) {
      if (args.direction === "upload") return args.delete ? row("delete", "remote") : null;
      return row("copy", "local");
    }
    return null;
  }

  /** S3 to S3 analysis: the source listing stands in for the local folder (rows copy to the remote side). */
  private analyzeS3ToS3(
    connection: Connection,
    args: CommandArgs["sync_analyze"],
    prefix: string,
    remote: Map<string, MockObject>,
    included: (rel: string) => boolean,
  ): SyncPlan {
    const source = /^s3:\/\/([^/]+)\/(.*)$/.exec(args.source_prefix ?? "");
    if (!source) {
      fail("usage", "An S3 to S3 sync needs a source prefix like \"s3://bucket/prefix/\".");
    }
    const [, sourceBucketName = "", sourcePrefix = ""] = source;
    const sourceBucket = this.bucketFor(connection, sourceBucketName);
    if (sourceBucketName === (/^s3:\/\/([^/]+)/.exec(args.s3_prefix)?.[1] ?? "") && (sourcePrefix.startsWith(prefix) || prefix.startsWith(sourcePrefix))) {
      fail("usage", "The source and the destination prefix must not be the same or contain each other.");
    }
    const from = new Map<string, MockObject>();
    for (const key of this.keysUnder(sourceBucket, sourcePrefix)) {
      const object = sourceBucket.objects.get(key);
      if (object && !isFolderKey(key) && included(key.slice(sourcePrefix.length))) {
        from.set(key.slice(sourcePrefix.length), object);
      }
    }
    const rows: SyncRow[] = [];
    for (const rel of [...new Set([...from.keys(), ...remote.keys()])].sort(compareKeys)) {
      const a = from.get(rel) ?? null;
      const b = remote.get(rel) ?? null;
      const meta = a ? { size: a.size, modified: a.last_modified } : null;
      const entry = b ? objectEntry(`${prefix}${rel}`, b) : null;
      const base = { rel_path: rel, local_meta: meta, s3_meta: entry, error: null };
      if (a && b) {
        const newer = (parseTimestamp(a.last_modified) ?? 0) - (parseTimestamp(b.last_modified) ?? 0) > 2000;
        rows.push(
          a.size === b.size && !newer
            ? { ...base, action: "equal", target: null, done: true }
            : { ...base, action: "copy", target: "remote", done: false },
        );
      } else if (a) {
        rows.push({ ...base, action: "copy", target: "remote", done: false });
      } else if (b && args.delete) {
        rows.push({ ...base, action: "delete", target: "remote", done: false });
      }
    }
    const plan: SyncPlan = {
      id: uuid(),
      account_id: connection.account.id,
      local_root: "",
      s3_prefix: args.s3_prefix,
      source_prefix: args.source_prefix ?? "",
      direction: "s3_to_s3",
      delete: args.delete,
      created_at: nowIso(),
      rows,
    };
    this.plans.set(plan.id, { plan, executing: false, queue: [], attempts: new Map() });
    this.addLog("information", "sync", `Sync analysed: ${rows.length} row(s) from ${args.source_prefix} to ${args.s3_prefix}.`);
    return clone(plan);
  }

  private stepPlan(record: PlanRecord, dt: number): void {
    let budget = (this.options.bytesPerSecond * dt) / 1000;
    let processed = 0;
    while (record.queue.length > 0 && (budget > 0 || processed === 0)) {
      const index = record.queue.shift();
      if (index === undefined) {
        break;
      }
      const row = record.plan.rows[index];
      if (!row) {
        continue;
      }
      budget -= Math.max(row.local_meta?.size ?? row.s3_meta?.size ?? 0, 256 * 1024);
      processed += 1;
      const attempt = (record.attempts.get(index) ?? 0) + 1;
      record.attempts.set(index, attempt);
      const error = this.applyRow(record.plan, row, attempt);
      if (error === null) {
        row.done = true;
        row.error = null;
      } else {
        // The row keeps its planned action and stays not done, so a later run retries it.
        row.error = error;
        this.addLog("error", "sync", `Sync of ${row.rel_path} failed: ${error}`);
      }
      this.emit("sync://row", { plan_id: record.plan.id, index, row });
    }
    if (record.queue.length === 0) {
      this.finishPlan(record);
    } else if (processed > 0) {
      this.emitProgress(record, false);
    }
  }

  /** Applies one copy or delete row; returns an error message, or null on success. */
  private applyRow(plan: SyncPlan, row: SyncRow, attempt: number): string | null {
    const match = /^s3:\/\/([^/]+)\/(.*)$/.exec(plan.s3_prefix);
    const target = match ? this.findBucket(plan.account_id, match[1] ?? "") : null;
    if (!match || !target) {
      return "The bucket does not exist (NoSuchBucket).";
    }
    const key = `${match[2] ?? ""}${row.rel_path}`;
    const localPath = joinPath(plan.local_root, row.rel_path);
    if (nameMatches(row.rel_path, "flaky") && attempt === 1) {
      return "Connection reset by peer (os error 54)";
    }
    if (plan.direction === "s3_to_s3" && row.action === "copy") {
      const source = /^s3:\/\/([^/]+)\/(.*)$/.exec(plan.source_prefix ?? "");
      const from = source ? this.findBucket(plan.account_id, source[1] ?? "") : null;
      const object = from?.objects.get(`${source?.[2] ?? ""}${row.rel_path}`);
      if (!object) {
        return "The source object no longer exists (NoSuchKey).";
      }
      target.objects.set(key, { ...object, last_modified: nowIso() });
      return null;
    }
    if (row.action === "copy" && row.target === "remote") {
      const file = this.fs.stat(localPath);
      if (!file) {
        return "No such file or directory (os error 2)";
      }
      target.objects.set(key, makeObject(key, file.size, file.modified, this.settings.data_integrity.checksum_algorithm));
      return null;
    }
    if (row.action === "copy" && row.target === "local") {
      const object = target.objects.get(key);
      if (!object) {
        return "The specified key does not exist (NoSuchKey).";
      }
      this.fs.writeFile(localPath, { size: object.size, modified: object.last_modified });
      return null;
    }
    if (row.action === "delete" && row.target === "remote") {
      target.objects.delete(key);
      return null;
    }
    if (row.action === "delete" && row.target === "local") {
      this.fs.remove(localPath);
      return null;
    }
    return "This row cannot be executed.";
  }

  /**
   * Progress over the whole plan: total counts copy and delete rows, done those
   * applied, failed the failed rows plus not-done rows with an error.
   */
  private planProgress(plan: SyncPlan): { total: number; done: number; failed: number } {
    let total = 0;
    let done = 0;
    let failed = 0;
    for (const row of plan.rows) {
      if (row.action === "copy" || row.action === "delete") {
        total += 1;
        if (row.done) {
          done += 1;
        } else if (row.error !== null) {
          failed += 1;
        }
      } else if (row.action === "failed") {
        failed += 1;
      }
    }
    return { total, done, failed };
  }

  private emitProgress(record: PlanRecord, finished: boolean): void {
    this.emit("sync://progress", { plan_id: record.plan.id, ...this.planProgress(record.plan), finished });
  }

  private finishPlan(record: PlanRecord): void {
    record.executing = false;
    this.emitProgress(record, true);
    const cancelled = record.queue.length > 0 || record.cancelled === true;
    this.emit("sync://report", buildReport(record.plan, 0, cancelled));
    record.cancelled = false;
    const { total, done, failed } = this.planProgress(record.plan);
    this.addLog(
      failed > 0 ? "warning" : "information",
      "sync",
      `Sync finished: ${done} of ${total} rows applied, ${failed} failed.`,
    );
  }

  // ---------------------------------------------------------------- internals: log

  private addLog(level: LogLevel, source: string, message: string): void {
    if (LEVEL_RANK[level] > LEVEL_RANK[this.settings.logging.level]) {
      return;
    }
    this.logSequence += 1;
    const entry: LogEntry = { id: this.logSequence, ts: nowIso(), level, source, message };
    this.logEntries.push(entry);
    if (this.logEntries.length > LOG_CAPACITY) {
      this.logEntries.splice(0, this.logEntries.length - LOG_CAPACITY);
    }
    this.emit("log://entry", entry);
  }
}

/** An include glob of the mock: "*" within a folder, "**" across folders, "?" one character. */
function includeMatches(glob: string, rel: string): boolean {
  const escaped = glob
    .replace(/[.+^${}()|[\]\\]/g, "\\$&")
    .replace(/\*\*\//g, "@@SLASHES@@")
    .replace(/\*\*/g, "@@ANY@@")
    .replace(/\*/g, "[^/]*")
    .replace(/\?/g, "[^/]")
    .replace(/@@SLASHES@@/g, "(?:.*/)?")
    .replace(/@@ANY@@/g, ".*");
  return new RegExp(`^${escaped}$`).test(rel);
}
