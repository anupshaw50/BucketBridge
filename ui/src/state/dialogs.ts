import { createStore, type StoreApi } from "zustand/vanilla";
import type { CrashReport, ObjectEntry, QuitRequest, SyncJob } from "../api/types";
import type { SettingsPage } from "../lib/settings";

export interface ConfirmOptions {
  title: string;
  message: string;
  confirmLabel: string;
  /** Styles the confirm button as destructive. */
  danger?: boolean;
  /** When set, the confirm button stays disabled until exactly this text is typed. */
  typedConfirmation?: string;
}

/** Tabs of the bucket properties dialog. */
export type BucketPropertiesTab = "policy" | "public-access" | "versioning" | "logging" | "website" | "acceleration" | "usage";

/** Tabs of the bucket rules dialog. */
export type BucketRulesTab = "cors" | "lifecycle" | "tiering" | "replication";

/** Tabs of the object properties dialog. */
export type ObjectPropertiesTab = "general" | "headers" | "metadata" | "tags" | "class" | "encryption" | "permissions";

/** Tabs of the CloudFront manager; "toggle" opens the distributions and asks to enable or disable the only one listed. */
export type CloudFrontView = "distributions" | "invalidate" | "history" | "toggle" | "attach" | "edit" | "detach";

export type DialogRequest =
  | { kind: "accounts" }
  | { kind: "add-account" }
  | { kind: "import-aws" }
  | { kind: "import-accounts" }
  | { kind: "account-folder"; /** Set: renames this folder instead of creating one. */ folder?: string }
  | { kind: "edit-account"; accountId: string; /** Puts the cursor in the MFA device field. */ focus?: "mfa" }
  | { kind: "master-password" }
  | { kind: "settings"; /** Page shown first (default: general). */ page?: SettingsPage }
  | { kind: "about" }
  | {
      kind: "sync";
      /** Preselects this folder as the S3 side. */
      at?: { bucket: string; prefix: string };
      /** Fills the form with this saved job; a job with an id is edited and saved back. */
      job?: SyncJob;
      /** Opens on the results of the plan in the sync state (a saved job that was just analysed). */
      review?: boolean;
    }
  | { kind: "sync-jobs" }
  | { kind: "create-bucket"; accountId: string }
  | { kind: "delete-bucket"; accountId: string; bucket: string }
  | { kind: "bucket-properties"; accountId: string; bucket: string; /** Tab shown first (default: policy). */ tab?: BucketPropertiesTab }
  | { kind: "bucket-rules"; accountId: string; bucket: string; /** Tab shown first (default: cors). */ tab?: BucketRulesTab }
  | { kind: "external-bucket"; accountId: string }
  | { kind: "bookmark"; accountId: string; bucket: string; prefix: string; /** Set: renames this bookmark instead of adding one. */ bookmarkId?: string; label?: string }
  | { kind: "new-folder"; accountId: string; bucket: string; prefix: string }
  | { kind: "rename"; accountId: string; bucket: string; key: string }
  | {
      kind: "properties";
      accountId: string;
      bucket: string;
      entries: ObjectEntry[];
      /** Tab shown first (default: general). */
      tab?: ObjectPropertiesTab;
    }
  | { kind: "share"; accountId: string; bucket: string; key: string }
  | { kind: "encryption"; mode: "encrypt" | "decrypt"; accountId: string; bucket: string; keys: string[] }
  | { kind: "diagnostics"; /** Account probed first (default: the active account). */ accountId?: string }
  | { kind: "uploads" }
  | { kind: "iam" }
  | { kind: "security-scan" }
  | { kind: "mount"; accountId: string; bucket: string }
  | { kind: "filters" }
  | { kind: "export"; accountId: string; bucket: string; prefix: string }
  | { kind: "cloudfront"; accountId: string; bucket: string | null; view: CloudFrontView }
  | { kind: "versions"; accountId: string; bucket: string; prefix: string; exact: boolean }
  | { kind: "preview"; accountId: string; bucket: string; key: string; versionId: string | null }
  | { kind: "sharing-wizard"; accountId: string; bucket: string }
  | { kind: "bucket-lock"; accountId: string; bucket: string }
  | { kind: "bucket-ownership"; accountId: string; bucket: string }
  | { kind: "object-lock"; accountId: string; bucket: string; keys: string[] }
  | { kind: "object-restore"; accountId: string; bucket: string; keys: string[] }
  | {
      kind: "destination-picker";
      mode: "copy" | "move";
      accountId: string;
      bucket: string;
      keys: string[];
    }
  | { kind: "open-with"; accountId: string; bucket: string; key: string }
  | { kind: "website-redirect"; accountId: string; bucket: string; key: string }
  | { kind: "edit-sessions" }
  | { kind: "edit-conflict"; sessionId: string }
  | { kind: "crash"; report: CrashReport }
  | { kind: "quit"; request: QuitRequest }
  | { kind: "confirm"; options: ConfirmOptions; resolve: (confirmed: boolean) => void };

export type DialogKind = DialogRequest["kind"];

export type OpenDialog = DialogRequest & { id: number };

export interface DialogState {
  /** Open dialogs, bottom-most first. */
  open: OpenDialog[];
}

/** Single-instance dialogs: opening one again does not stack a second copy. */
const SINGLETONS: ReadonlySet<DialogKind> = new Set([
  "accounts",
  "add-account",
  "import-aws",
  "master-password",
  "settings",
  "about",
  "sync",
  "sync-jobs",
  "versions",
  "cloudfront",
  "create-bucket",
  "bucket-properties",
  "bucket-rules",
  "external-bucket",
  "bucket-lock",
  "bucket-ownership",
  "diagnostics",
  "uploads",
  "iam",
  "security-scan",
  "filters",
  "export",
  "edit-sessions",
  "crash",
  "quit",
]);

export class DialogController {
  readonly store: StoreApi<DialogState>;
  private sequence = 0;

  constructor() {
    this.store = createStore<DialogState>(() => ({ open: [] }));
  }

  open(request: DialogRequest): number {
    if (SINGLETONS.has(request.kind)) {
      const existing = this.store.getState().open.find((dialog) => dialog.kind === request.kind);
      if (existing) {
        return existing.id;
      }
    }
    this.sequence += 1;
    const dialog: OpenDialog = { ...request, id: this.sequence };
    this.store.setState({ open: [...this.store.getState().open, dialog] });
    return dialog.id;
  }

  close(id: number): void {
    const dialog = this.store.getState().open.find((item) => item.id === id);
    if (!dialog) {
      return;
    }
    this.store.setState({ open: this.store.getState().open.filter((item) => item.id !== id) });
    if (dialog.kind === "confirm") {
      dialog.resolve(false);
    }
  }

  isOpen(kind: DialogKind): boolean {
    return this.store.getState().open.some((dialog) => dialog.kind === kind);
  }

  /** Shows a confirmation dialog; resolves `true` only when the user confirms. */
  confirm(options: ConfirmOptions): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const once = (confirmed: boolean) => {
        if (!settled) {
          settled = true;
          resolve(confirmed);
        }
      };
      this.open({ kind: "confirm", options, resolve: once });
    });
  }

  /** Resolves a confirmation with the user's answer and closes it. */
  answer(id: number, confirmed: boolean): void {
    const dialog = this.store.getState().open.find((item) => item.id === id);
    if (dialog?.kind !== "confirm") {
      return;
    }
    dialog.resolve(confirmed);
    this.close(id);
  }

  /** Closes every dialog (confirmations resolve as cancelled). */
  closeAll(): void {
    for (const dialog of this.store.getState().open) {
      this.close(dialog.id);
    }
  }
}
