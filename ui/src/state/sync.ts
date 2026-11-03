import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { errorText } from "../api/errors";
import { runGuarded, type SessionPrompts } from "../api/guard";
import type { SyncDirection, SyncJob, SyncOptions, SyncPlan, SyncProgress, SyncReport, SyncRowEvent } from "../api/types";
import { type FrameScheduler, FrameTask } from "./scheduler";

export interface SyncState {
  plan: SyncPlan | null;
  progress: SyncProgress | null;
  analyzing: boolean;
  /** sync_execute was called and no final progress event has arrived yet. */
  running: boolean;
  cancelling: boolean;
  error: string | null;
  /** Summary report of the last finished run of the shown plan. */
  report: SyncReport | null;
  /** Saved sync jobs. */
  jobs: SyncJob[];
}

export interface AnalyzeRequest {
  accountId: string;
  localRoot: string;
  s3Prefix: string;
  direction: SyncDirection;
  deleteExtra: boolean;
  /** Filters, exclusion rules and analysis options. */
  options?: SyncOptions;
  /** Source of an s3_to_s3 sync. */
  sourcePrefix?: string;
}

/** The plan shown in the sync wizard, its live rows and progress. */
export class SyncController {
  readonly store: StoreApi<SyncState>;
  private pendingRows = new Map<number, SyncRowEvent["row"]>();
  private readonly frame: FrameTask;
  private analyzeRequest = 0;

  private readonly api: Backend;
  private readonly prompts: SessionPrompts;

  constructor(api: Backend, prompts: SessionPrompts, scheduler: FrameScheduler) {
    this.api = api;
    this.prompts = prompts;
    this.frame = new FrameTask(scheduler, () => this.flushRows());
    this.store = createStore<SyncState>(() => ({
      plan: null,
      progress: null,
      analyzing: false,
      running: false,
      cancelling: false,
      error: null,
      report: null,
      jobs: [],
    }));
  }

  get state(): SyncState {
    return this.store.getState();
  }

  async analyze(request: AnalyzeRequest): Promise<boolean> {
    const id = ++this.analyzeRequest;
    this.dropPendingRows();
    this.store.setState({ plan: null, progress: null, report: null, analyzing: true, running: false, error: null });
    try {
      const plan = await this.api.sync_analyze({
        account_id: request.accountId,
        local_root: request.localRoot,
        s3_prefix: request.s3Prefix,
        direction: request.direction,
        delete: request.deleteExtra,
        ...(request.options === undefined ? {} : { options: request.options }),
        ...(request.sourcePrefix === undefined ? {} : { source_prefix: request.sourcePrefix }),
      });
      if (id !== this.analyzeRequest) {
        return false;
      }
      this.store.setState({ plan, analyzing: false });
      return true;
    } catch (error) {
      if (id === this.analyzeRequest) {
        this.store.setState({ analyzing: false, error: errorText(error) });
      }
      return false;
    }
  }

  /** Starts (or resumes) execution; only rows with done = false are processed. */
  async execute(): Promise<void> {
    const { plan, running } = this.state;
    if (plan === null || running) {
      return;
    }
    this.store.setState({ running: true, cancelling: false, error: null, progress: null, report: null });
    try {
      await runGuarded(() => this.api.sync_execute({ plan_id: plan.id }), plan.account_id, this.prompts);
    } catch (error) {
      this.store.setState({ running: false, error: errorText(error) });
    }
  }

  async cancel(): Promise<void> {
    const { plan, running } = this.state;
    if (plan === null || !running) {
      return;
    }
    this.store.setState({ cancelling: true });
    try {
      await this.api.sync_cancel({ plan_id: plan.id });
    } catch (error) {
      this.store.setState({ cancelling: false, error: errorText(error) });
    }
  }

  /** Loads the saved sync jobs. */
  async loadJobs(): Promise<void> {
    try {
      this.setJobs(await this.api.list_sync_jobs());
    } catch (error) {
      this.store.setState({ error: errorText(error) });
    }
  }

  setJobs(jobs: SyncJob[]): void {
    this.store.setState({ jobs });
  }

  /** Creates (empty id) or replaces a job; rejects with the backend error. */
  async saveJob(job: SyncJob): Promise<SyncJob> {
    const saved = await this.api.save_sync_job({ job });
    this.setJobs(await this.api.list_sync_jobs());
    return saved;
  }

  /** Deletes a job; rejects with the backend error. */
  async deleteJob(id: string): Promise<void> {
    this.setJobs(await this.api.delete_sync_job({ id }));
  }

  /** Analyses a saved job and shows its plan, ready for review and Execute. Returns whether it worked. */
  async runJob(id: string): Promise<boolean> {
    const request = ++this.analyzeRequest;
    this.dropPendingRows();
    this.store.setState({ plan: null, progress: null, report: null, analyzing: true, running: false, error: null });
    try {
      const plan = await runGuardedJob(this.api, id);
      if (request !== this.analyzeRequest) {
        return false;
      }
      this.store.setState({ plan, analyzing: false });
      return true;
    } catch (error) {
      if (request === this.analyzeRequest) {
        this.store.setState({ analyzing: false, error: errorText(error) });
      }
      return false;
    }
  }

  onReport(report: SyncReport): void {
    if (this.state.plan?.id === report.plan_id) {
      this.store.setState({ report });
    }
  }

  /** Re-reads the plan (rows and their done flags) from the backend. */
  async reload(): Promise<void> {
    const { plan } = this.state;
    if (plan === null) {
      return;
    }
    try {
      const fresh = await this.api.sync_get({ plan_id: plan.id });
      if (this.state.plan?.id === fresh.id) {
        this.dropPendingRows();
        this.store.setState({ plan: fresh });
      }
    } catch (error) {
      this.store.setState({ error: errorText(error) });
    }
  }

  onProgress(progress: SyncProgress): void {
    const { plan } = this.state;
    if (plan?.id !== progress.plan_id) {
      return;
    }
    if (progress.finished) {
      this.flushRows();
      this.store.setState({ progress, running: false, cancelling: false });
    } else {
      this.store.setState({ progress, running: true });
    }
  }

  onRow(event: SyncRowEvent): void {
    const { plan } = this.state;
    if (plan?.id !== event.plan_id || event.index < 0 || event.index >= plan.rows.length) {
      return;
    }
    this.pendingRows.set(event.index, event.row);
    this.frame.request();
  }

  flushRows(): void {
    this.frame.cancel();
    const { plan } = this.state;
    if (plan === null || this.pendingRows.size === 0) {
      this.pendingRows.clear();
      return;
    }
    const rows = plan.rows.slice();
    for (const [index, row] of this.pendingRows) {
      rows[index] = row;
    }
    this.pendingRows.clear();
    this.store.setState({ plan: { ...plan, rows } });
  }

  reset(): void {
    this.analyzeRequest += 1;
    this.dropPendingRows();
    this.store.setState({
      plan: null,
      progress: null,
      report: null,
      analyzing: false,
      running: false,
      cancelling: false,
      error: null,
    });
  }

  private dropPendingRows(): void {
    this.frame.cancel();
    this.pendingRows.clear();
  }
}

function runGuardedJob(api: Backend, id: string): Promise<SyncPlan> {
  return api.run_sync_job({ id });
}
