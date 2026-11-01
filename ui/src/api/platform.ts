export interface FileFilter {
  name: string;
  extensions: string[];
}

export interface CopyOptions {
  sensitive?: boolean;
}

export interface SavePathOptions {
  title: string;
  defaultPath: string;
  filters: FileFilter[];
}

/**
 * Native integrations that are not IPC commands: pickers, opening URLs,
 * revealing files and the system clipboard. Inside Tauri they go through the
 * official plugins; in a plain browser the mock environment provides them.
 * Every picker resolves `null` when the user cancels.
 */
export interface Platform {
  readonly kind: "tauri" | "browser";
  pickFiles(title: string): Promise<string[] | null>;
  pickFolder(title: string): Promise<string | null>;
  /** Native picker for an application: an .app bundle on macOS, an .exe on Windows, any executable on Linux. */
  pickApplication(title: string): Promise<string | null>;
  pickSavePath(options: SavePathOptions): Promise<string | null>;
  openUrl(url: string): Promise<void>;
  revealPath(path: string): Promise<void>;
  /**
   * Writes text to the system clipboard. `sensitive` marks values such as
   * presigned links and passwords so clipboard history and sync skip them.
   */
  copyText(text: string, options?: CopyOptions): Promise<void>;
}
