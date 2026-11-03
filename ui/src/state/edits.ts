import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import type { EditResolution, EditResolved, EditSession } from "../api/types";
import type { DialogController } from "./dialogs";
import type { StatusController } from "./status";

export interface EditsState {
  /** Active sessions, oldest first. */
  sessions: EditSession[];
}

/**
 * The edit-in-place sessions of the app, kept in step through `edit://changed`. A session that turns
 * into a conflict opens the conflict dialog once; sessions that ended (`stopped`) leave the list.
 */
export class EditController {
  readonly store: StoreApi<EditsState>;

  private readonly api: Backend;
  private readonly dialogs: DialogController;
  private readonly status: StatusController;

  constructor(api: Backend, dialogs: DialogController, status: StatusController) {
    this.api = api;
    this.dialogs = dialogs;
    this.status = status;
    this.store = createStore<EditsState>(() => ({ sessions: [] }));
  }

  get sessions(): EditSession[] {
    return this.store.getState().sessions;
  }

  /** The session with this id, if it is still active. */
  find(id: string): EditSession | undefined {
    return this.sessions.find((session) => session.id === id);
  }

  /** Reads the active sessions (at startup, and when a window opens). */
  async load(): Promise<void> {
    try {
      this.store.setState({ sessions: await this.api.list_edits() });
    } catch (error) {
      this.status.error(error);
    }
  }

  /** The handler of `edit://changed`. */
  onChanged(session: EditSession): void {
    const before = this.find(session.id);
    if (session.state === "stopped") {
      this.store.setState({ sessions: this.sessions.filter((item) => item.id !== session.id) });
      return;
    }
    const sessions =
      before === undefined
        ? [...this.sessions, session]
        : this.sessions.map((item) => (item.id === session.id ? session : item));
    this.store.setState({ sessions });
    if (session.state === "conflict" && before?.state !== "conflict") {
      this.dialogs.open({ kind: "edit-conflict", sessionId: session.id });
    }
  }

  /** Downloads the object, opens the copy and starts uploading its saves. */
  async start(accountId: string, bucket: string, key: string, app: string | null = null): Promise<EditSession | null> {
    try {
      const session = await this.api.start_edit({ account_id: accountId, bucket, key, app });
      // The event may not have arrived yet.
      if (this.find(session.id) === undefined) {
        this.onChanged(session);
      }
      this.status.info(`Editing ${key}. Saves are uploaded automatically.`);
      return session;
    } catch (error) {
      this.status.error(error, { notify: "The object could not be opened for editing" });
      return null;
    }
  }

  /** Ends a session; a pending save is uploaded first unless `discard`. Returns whether it ended. */
  async stop(id: string, discard = false): Promise<boolean> {
    try {
      await this.api.stop_edit({ id, discard });
      this.store.setState({ sessions: this.sessions.filter((item) => item.id !== id) });
      return true;
    } catch (error) {
      this.status.error(error, { notify: "Editing could not be stopped" });
      return false;
    }
  }

  /** Settles a conflict or retries a failed upload. Returns the result, or null when it failed. */
  async resolve(id: string, resolution: EditResolution): Promise<EditResolved | null> {
    try {
      const resolved = await this.api.resolve_edit_conflict({ id, resolution });
      if (resolved.session === null) {
        this.store.setState({ sessions: this.sessions.filter((item) => item.id !== id) });
      }
      return resolved;
    } catch (error) {
      this.status.error(error, { notify: "The conflict could not be resolved" });
      return null;
    }
  }
}
