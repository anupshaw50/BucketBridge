import type { ChecksumAlgorithm, LogLevel, ProxyKind, Settings, ThemeSetting } from "../api/types";
import { validateBypassEntry, validateHost } from "./validation";

/** Defaults documented in docs/desktop-api.md. */
export const DEFAULT_SETTINGS: Settings = {
  general: { confirm_delete: true, default_download_dir: null, open_with_recent: [] },
  interface: { theme: "system", flat_browsing: false, show_transfer_panel: true, show_log_panel: false },
  advanced: { max_attempts: 5, connect_timeout_secs: 10, read_timeout_secs: 60, list_page_size: 1000 },
  data_integrity: { checksum_algorithm: "crc32", validate_downloads: true, preserve_mtime: true },
  logging: { level: "information", file_logging: true, max_file_mib: 10 },
  proxy: {
    enabled: false,
    kind: "http",
    host: "",
    port: 8080,
    username: null,
    password_ref: null,
    bypass: [],
  },
  transfer: {
    max_concurrent_tasks: 4,
    max_parts_per_task: 4,
    multipart_threshold_mib: 8,
    part_size_mib: 8,
    bandwidth_limit_kibps: null,
    retry_attempts: 3,
    if_exists: "overwrite",
  },
  shortener: { provider: "none", token_ref: null },
};

export function cloneSettings(settings: Settings): Settings {
  return {
    general: { ...settings.general, open_with_recent: [...settings.general.open_with_recent] },
    interface: { ...settings.interface },
    advanced: { ...settings.advanced },
    data_integrity: { ...settings.data_integrity },
    logging: { ...settings.logging },
    proxy: { ...settings.proxy, bypass: [...settings.proxy.bypass] },
    transfer: { ...settings.transfer },
    shortener: { ...settings.shortener },
  };
}

export interface NumberRange {
  min: number;
  max: number;
}

/** Accepted integer ranges, exactly as core's Settings::validate enforces them (see the contract). */
export const SETTINGS_RANGES = {
  "advanced.max_attempts": { min: 1, max: 20 },
  "advanced.connect_timeout_secs": { min: 1, max: 300 },
  "advanced.read_timeout_secs": { min: 1, max: 3600 },
  "advanced.list_page_size": { min: 1, max: 1000 },
  "logging.max_file_mib": { min: 1, max: 1024 },
  "proxy.port": { min: 1, max: 65535 },
  "transfer.max_concurrent_tasks": { min: 1, max: 32 },
  "transfer.max_parts_per_task": { min: 1, max: 32 },
  "transfer.multipart_threshold_mib": { min: 5, max: 5120 },
  "transfer.part_size_mib": { min: 5, max: 5120 },
  "transfer.bandwidth_limit_kibps": { min: 1, max: 104_857_600 },
  "transfer.retry_attempts": { min: 0, max: 20 },
} as const satisfies Record<string, NumberRange>;

export type RangedField = keyof typeof SETTINGS_RANGES;

export type SettingsField =
  | RangedField
  | "general.default_download_dir"
  | "interface.theme"
  | "data_integrity.checksum_algorithm"
  | "logging.level"
  | "proxy.kind"
  | "proxy.host"
  | "proxy.username"
  | "proxy.bypass";

export type SettingsErrors = Partial<Record<SettingsField, string>>;

const THEMES: readonly ThemeSetting[] = ["system", "light", "dark"];
const CHECKSUMS: readonly ChecksumAlgorithm[] = ["crc32", "crc32c", "crc64nvme", "sha1", "sha256", "none"];
const LOG_LEVELS: readonly LogLevel[] = ["error", "warning", "information"];
const PROXY_KINDS: readonly ProxyKind[] = ["http", "socks5"];

function rangedValue(settings: Settings, field: RangedField): number | null {
  switch (field) {
    case "advanced.max_attempts":
      return settings.advanced.max_attempts;
    case "advanced.connect_timeout_secs":
      return settings.advanced.connect_timeout_secs;
    case "advanced.read_timeout_secs":
      return settings.advanced.read_timeout_secs;
    case "advanced.list_page_size":
      return settings.advanced.list_page_size;
    case "logging.max_file_mib":
      return settings.logging.max_file_mib;
    case "proxy.port":
      return settings.proxy.port;
    case "transfer.max_concurrent_tasks":
      return settings.transfer.max_concurrent_tasks;
    case "transfer.max_parts_per_task":
      return settings.transfer.max_parts_per_task;
    case "transfer.multipart_threshold_mib":
      return settings.transfer.multipart_threshold_mib;
    case "transfer.part_size_mib":
      return settings.transfer.part_size_mib;
    case "transfer.bandwidth_limit_kibps":
      return settings.transfer.bandwidth_limit_kibps;
    case "transfer.retry_attempts":
      return settings.transfer.retry_attempts;
  }
}

export function rangeMessage(field: RangedField): string {
  const { min, max } = SETTINGS_RANGES[field];
  return `Enter a whole number from ${min.toLocaleString("en-US")} to ${max.toLocaleString("en-US")}.`;
}

export function inRange(field: RangedField, value: number): boolean {
  const { min, max } = SETTINGS_RANGES[field];
  return Number.isInteger(value) && value >= min && value <= max;
}

/** Validates a complete settings object. An empty result means it can be saved. */
export function validateSettings(settings: Settings): SettingsErrors {
  const errors: SettingsErrors = {};
  for (const field of Object.keys(SETTINGS_RANGES) as RangedField[]) {
    const value = rangedValue(settings, field);
    if (value === null) {
      if (field !== "transfer.bandwidth_limit_kibps") {
        errors[field] = rangeMessage(field);
      }
      continue;
    }
    if (!inRange(field, value)) {
      errors[field] = rangeMessage(field);
    }
  }
  const downloadDir = settings.general.default_download_dir;
  if (downloadDir !== null && downloadDir.trim().length === 0) {
    errors["general.default_download_dir"] = "Choose a folder or select ask every time.";
  }
  if (!THEMES.includes(settings.interface.theme)) {
    errors["interface.theme"] = "Choose a theme.";
  }
  if (!CHECKSUMS.includes(settings.data_integrity.checksum_algorithm)) {
    errors["data_integrity.checksum_algorithm"] = "Choose a checksum algorithm.";
  }
  if (!LOG_LEVELS.includes(settings.logging.level)) {
    errors["logging.level"] = "Choose a log level.";
  }
  if (!PROXY_KINDS.includes(settings.proxy.kind)) {
    errors["proxy.kind"] = "Choose a proxy type.";
  }
  // The host is required while the proxy is enabled, and must be valid whenever it is filled in.
  if (settings.proxy.enabled || settings.proxy.host.length > 0) {
    const hostError = validateHost(settings.proxy.host);
    if (hostError) {
      errors["proxy.host"] = hostError;
    }
  }
  if (settings.proxy.username !== null && settings.proxy.username.trim().length === 0) {
    errors["proxy.username"] = "Leave the user name empty or enter a value.";
  }
  for (const entry of settings.proxy.bypass) {
    const entryError = validateBypassEntry(entry);
    if (entryError) {
      errors["proxy.bypass"] = entryError;
      break;
    }
  }
  return errors;
}

export function hasErrors(errors: SettingsErrors): boolean {
  return Object.keys(errors).length > 0;
}

/** Which settings page shows a field (used to jump to the first invalid page). */
export function pageOfField(field: SettingsField): SettingsPage {
  const section = field.slice(0, field.indexOf("."));
  switch (section) {
    case "general":
      return "general";
    case "interface":
      return "interface";
    case "advanced":
      return "advanced";
    case "data_integrity":
      return "data_integrity";
    case "logging":
      return "logging";
    case "proxy":
      return "proxy";
    case "shortener":
      return "sharing";
    default:
      return "transfer";
  }
}

export type SettingsPage =
  | "general"
  | "interface"
  | "transfer"
  | "advanced"
  | "data_integrity"
  | "logging"
  | "proxy"
  | "sharing";
