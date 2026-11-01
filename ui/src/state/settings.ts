import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { BackendError, errorText } from "../api/errors";
import type { Settings, ThemeSetting } from "../api/types";
import { cloneSettings, DEFAULT_SETTINGS, hasErrors, validateSettings } from "../lib/settings";

export interface SettingsState {
  /** Last settings loaded from or saved to the backend (defaults until loaded). */
  settings: Settings;
  loaded: boolean;
  loadError: string | null;
  /** Theme picked in the open settings dialog but not saved yet. */
  previewTheme: ThemeSetting | null;
}

export class SettingsController {
  readonly store: StoreApi<SettingsState>;
  private readonly listeners = new Set<(next: Settings, previous: Settings) => void>();
  /** Tail of the save/update queue. */
  private queue: Promise<void> = Promise.resolve();

  private readonly api: Backend;

  constructor(api: Backend) {
    this.api = api;
    this.store = createStore<SettingsState>(() => ({
      settings: cloneSettings(DEFAULT_SETTINGS),
      loaded: false,
      loadError: null,
      previewTheme: null,
    }));
  }

  get current(): Settings {
    return this.store.getState().settings;
  }

  /** Notified after settings are loaded or saved. */
  onChange(listener: (next: Settings, previous: Settings) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async load(): Promise<Settings> {
    try {
      const settings = await this.api.get_settings();
      this.replace(settings);
      this.store.setState({ loaded: true, loadError: null });
      return settings;
    } catch (error) {
      this.store.setState({ loadError: errorText(error) });
      throw error;
    }
  }

  /**
   * Validates and saves; throws a `usage` error without calling the backend
   * when invalid. Saves and updates run one after another.
   */
  save(next: Settings): Promise<void> {
    return this.serialized(() => this.saveNow(next));
  }

  /**
   * Applies a change on top of the latest saved settings (toolbar toggles).
   * Updates queue behind each other, so quick successive changes all persist.
   * If the settings were never loaded they are loaded first: a toggle must
   * never write defaults over the stored settings.
   */
  update(change: (draft: Settings) => void): Promise<void> {
    return this.serialized(async () => {
      if (!this.store.getState().loaded) {
        await this.load();
      }
      const draft = cloneSettings(this.current);
      change(draft);
      await this.saveNow(draft);
    });
  }

  private serialized<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.queue.then(operation, operation);
    this.queue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private async saveNow(next: Settings): Promise<void> {
    const errors = validateSettings(next);
    if (hasErrors(errors)) {
      const first = Object.values(errors)[0] ?? "The settings are not valid.";
      throw new BackendError("usage", first);
    }
    const copy = cloneSettings(next);
    await this.api.save_settings({ settings: copy });
    this.replace(copy);
  }

  setPreviewTheme(theme: ThemeSetting | null): void {
    this.store.setState({ previewTheme: theme });
  }

  /** Re-reads the proxy password reference after set_proxy_password, keeping everything else. */
  async refreshProxyPasswordRef(): Promise<string | null> {
    const fresh = await this.api.get_settings();
    const merged = cloneSettings(this.current);
    merged.proxy.password_ref = fresh.proxy.password_ref;
    this.replace(merged);
    return fresh.proxy.password_ref;
  }

  /** Re-reads the shortener token reference after set_shortener_token, keeping everything else. */
  async refreshShortenerTokenRef(): Promise<string | null> {
    const fresh = await this.api.get_settings();
    const merged = cloneSettings(this.current);
    merged.shortener.token_ref = fresh.shortener.token_ref;
    this.replace(merged);
    return fresh.shortener.token_ref;
  }

  private replace(settings: Settings): void {
    const previous = this.current;
    this.store.setState({ settings });
    for (const listener of this.listeners) {
      listener(settings, previous);
    }
  }
}
