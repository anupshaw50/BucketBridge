import { BackendError } from "../api/errors";
import type { SyncJob, SyncKindTotals, SyncPlan, SyncReport, SyncRow } from "../api/types";
import { clone, uuid } from "./util";

const MAX_NAME = 100;
const MAX_JOBS = 200;

function fail(code: "usage" | "not_found" | "conflict", message: string): never {
  throw new BackendError(code, message);
}

/** Converts one gitignore-style glob (without negation or trailing slash) to a regular expression. */
function globToRegExp(glob: string): RegExp {
  let out = "";
  for (let i = 0; i < glob.length; i += 1) {
    const c = glob.charAt(i);
    if (c === "*") {
      if (glob.charAt(i + 1) === "*") {
        // "**/" matches any number of folders (also none); a trailing "**" matches everything below.
        if (glob.charAt(i + 2) === "/") {
          out += "(?:.*/)?";
          i += 2;
        } else {
          out += ".*";
          i += 1;
        }
      } else {
        out += "[^/]*";
      }
    } else if (c === "?") {
      out += "[^/]";
    } else if (c === "\\") {
      i += 1;
      out += (glob.charAt(i) || "\\").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    } else {
      out += c.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }
  }
  return new RegExp(`^${out}$`);
}

interface Rule {
  negated: boolean;
  folderOnly: boolean;
  pattern: RegExp;
}

/** The gitignore subset of the sync exclusion rules (comments, blank lines, "!", "**", trailing "/"). */
export class ExcludeRules {
  private readonly rules: Rule[] = [];

  constructor(lines: readonly string[]) {
    for (const raw of lines) {
      let line = raw.replace(/\r$/, "").replace(/(?<!\\) +$/, "");
      if (line === "" || line.startsWith("#")) {
        continue;
      }
      let negated = false;
      if (line.startsWith("!")) {
        negated = true;
        line = line.slice(1);
      } else if (line.startsWith("\\#") || line.startsWith("\\!")) {
        line = line.slice(1);
      }
      let folderOnly = false;
      if (line.endsWith("/")) {
        folderOnly = true;
        line = line.slice(0, -1);
      }
      const anchored = line.startsWith("/") || line.replace(/\/\*\*$/, "").includes("/");
      line = line.replace(/^\/+/, "");
      if (line === "") {
        continue;
      }
      const glob = anchored || line.startsWith("**/") ? line : `**/${line}`;
      this.rules.push({ negated, folderOnly, pattern: globToRegExp(glob) });
    }
  }

  private decide(path: string, folder: boolean): boolean {
    for (let i = this.rules.length - 1; i >= 0; i -= 1) {
      const rule = this.rules[i];
      if (rule && (folder || !rule.folderOnly) && rule.pattern.test(path)) {
        return !rule.negated;
      }
    }
    return false;
  }

  /** Whether the file is excluded, directly or through one of its folders. */
  isExcluded(path: string): boolean {
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i += 1) {
      if (this.decide(parts.slice(0, i).join("/"), true)) {
        return true;
      }
    }
    return this.decide(path, false);
  }
}

/** Saved sync jobs with the validation of the real backend. */
export class MockSyncJobs {
  private jobs: SyncJob[] = [];

  list(): SyncJob[] {
    return clone(this.jobs);
  }

  find(id: string): SyncJob {
    const job = this.jobs.find((item) => item.id === id) ?? this.jobs.find((item) => item.name.toLowerCase() === id.toLowerCase());
    if (job === undefined) {
      fail("not_found", `there is no sync job '${id}'`);
    }
    return clone(job);
  }

  save(input: SyncJob, accountExists: (id: string) => boolean): SyncJob {
    const job = clone(input);
    job.name = job.name.trim();
    if (job.name === "") {
      fail("usage", "the sync job needs a name");
    }
    if ([...job.name].length > MAX_NAME) {
      fail("usage", `the name is longer than ${MAX_NAME} characters`);
    }
    if (job.direction === "s3_to_s3") {
      if (job.s3_source === undefined || job.s3_source === "") {
        fail("usage", "an S3 to S3 sync job needs a source prefix");
      }
      job.local_root = "";
    } else {
      if (job.local_root.trim() === "") {
        fail("usage", "a sync job needs a local folder");
      }
      delete job.s3_source;
    }
    if (job.delete && job.direction === "bidirectional") {
      fail("usage", "delete cannot be combined with bidirectional sync");
    }
    if (job.options.parallelism < 1 || job.options.parallelism > 32) {
      fail("usage", "parallelism must be between 1 and 32");
    }
    if (!accountExists(job.account_id)) {
      fail("not_found", `there is no account '${job.account_id}'`);
    }
    const clash = this.jobs.find((item) => item.id !== job.id && item.name.toLowerCase() === job.name.toLowerCase());
    if (clash !== undefined) {
      fail("conflict", `a sync job named '${clash.name}' already exists`);
    }
    if (job.id === "") {
      if (this.jobs.length >= MAX_JOBS) {
        fail("conflict", `at most ${MAX_JOBS} sync jobs can be saved; delete one first`);
      }
      job.id = uuid();
      this.jobs.push(job);
    } else {
      const at = this.jobs.findIndex((item) => item.id === job.id);
      if (at < 0) {
        fail("not_found", `there is no sync job '${job.id}'`);
      }
      this.jobs[at] = job;
    }
    return clone(job);
  }

  delete(id: string): SyncJob[] {
    const at = this.jobs.findIndex((item) => item.id === id);
    if (at < 0) {
      fail("not_found", `there is no sync job '${id}'`);
    }
    this.jobs.splice(at, 1);
    return this.list();
  }

  dropAccount(accountId: string): void {
    this.jobs = this.jobs.filter((job) => job.account_id !== accountId);
  }
}

function totals(): SyncKindTotals {
  return { files: 0, bytes: 0, failed: 0, pending: 0 };
}

/** The summary report of a plan's rows (same counting as the core's `SyncReport`). */
export function buildReport(plan: SyncPlan, durationMs: number, cancelled: boolean): SyncReport {
  const report: SyncReport = {
    plan_id: plan.id,
    direction: plan.direction,
    duration_ms: durationMs,
    cancelled,
    upload: totals(),
    download: totals(),
    copy: totals(),
    delete_local: totals(),
    delete_remote: totals(),
    equal: totals(),
    unsyncable: 0,
    failed: 0,
    failures: [],
  };
  const pick = (row: SyncRow): SyncKindTotals | null => {
    if (row.action === "equal") return report.equal;
    if (row.action === "copy" && row.target === "remote") return plan.direction === "s3_to_s3" ? report.copy : report.upload;
    if (row.action === "copy" && row.target === "local") return report.download;
    if (row.action === "delete" && row.target === "local") return report.delete_local;
    if (row.action === "delete" && row.target === "remote") return report.delete_remote;
    return null;
  };
  for (const row of plan.rows) {
    const kind = pick(row);
    if (kind === null) {
      report.unsyncable += 1;
      report.failed += 1;
      report.failures.push({ rel_path: row.rel_path, error: row.error ?? "unknown error" });
      continue;
    }
    if (row.done) {
      kind.files += 1;
      kind.bytes += row.local_meta?.size ?? row.s3_meta?.size ?? 0;
    } else if (row.error !== null) {
      kind.failed += 1;
      report.failed += 1;
      report.failures.push({ rel_path: row.rel_path, error: row.error });
    } else {
      kind.pending += 1;
    }
  }
  return report;
}
