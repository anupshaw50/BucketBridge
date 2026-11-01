const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] as const;

/** Formats a byte count with binary units: "0 B", "512 B", "1.5 KiB", "320 MiB". */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes < 0) {
    return "";
  }
  if (bytes < 1024) {
    return `${Math.round(bytes)} B`;
  }
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Keep three significant digits at most, and avoid "1024.0 KiB" after rounding.
  let text = value < 100 ? value.toFixed(1) : value.toFixed(0);
  if (Number(text) >= 1024 && unit < BYTE_UNITS.length - 1) {
    unit += 1;
    text = (Number(text) / 1024).toFixed(1);
  }
  return `${text} ${BYTE_UNITS[unit] ?? "B"}`;
}

/** Formats a transfer speed in bytes per second; idle speeds render as an empty string. */
export function formatSpeed(bytesPerSecond: number | null | undefined): string {
  if (bytesPerSecond === null || bytesPerSecond === undefined || !(bytesPerSecond > 0)) {
    return "";
  }
  return `${formatBytes(bytesPerSecond)}/s`;
}

function pad2(value: number): string {
  return value.toString().padStart(2, "0");
}

/** Formats a remaining time in seconds: "45s", "3m 05s", "2h 03m", "3d 04h". */
export function formatEta(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds < 0) {
    return "";
  }
  const total = Math.ceil(seconds);
  if (total < 60) {
    return `${total}s`;
  }
  const minutes = Math.floor(total / 60);
  if (minutes < 60) {
    return `${minutes}m ${pad2(total % 60)}s`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `${hours}h ${pad2(minutes % 60)}m`;
  }
  const days = Math.floor(hours / 24);
  return `${days}d ${pad2(hours % 24)}h`;
}

const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(?:([Zz])|([+-])(\d{2}):?(\d{2}))$/;

/**
 * Parses an RFC 3339 timestamp into epoch milliseconds. Unlike `Date.parse`,
 * it accepts any number of fractional digits on every engine (Rust emits
 * nanoseconds). Returns `null` for anything that is not a valid timestamp.
 */
export function parseTimestamp(value: string | null | undefined): number | null {
  if (!value) {
    return null;
  }
  const match = RFC3339.exec(value.trim());
  if (!match) {
    return null;
  }
  const [, y, mo, d, h, mi, s, fraction, zulu, sign, offH, offM] = match;
  const year = Number(y);
  const month = Number(mo);
  const day = Number(d);
  const hour = Number(h);
  const minute = Number(mi);
  const second = Number(s);
  if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59 || second > 60) {
    return null;
  }
  const millis = fraction ? Number(fraction.slice(0, 3).padEnd(3, "0")) : 0;
  let epoch = Date.UTC(year, month - 1, day, hour, minute, Math.min(second, 59), millis);
  const check = new Date(Date.UTC(year, month - 1, day));
  if (check.getUTCDate() !== day) {
    return null; // e.g. 2026-02-30
  }
  if (!zulu && sign) {
    const offset = (Number(offH) * 60 + Number(offM)) * 60_000;
    epoch += sign === "+" ? -offset : offset;
  }
  return epoch;
}

/** Local date and time as "YYYY-MM-DD HH:MM:SS"; unparseable input is shown as is. */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return "";
  }
  const epoch = parseTimestamp(value);
  if (epoch === null) {
    return value;
  }
  const date = new Date(epoch);
  return (
    `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ` +
    `${pad2(date.getHours())}:${pad2(date.getMinutes())}:${pad2(date.getSeconds())}`
  );
}

const COUNT_FORMAT = new Intl.NumberFormat("en-US");

/** Integer with thousands separators: 10000 becomes "10,000". */
export function formatCount(value: number): string {
  return COUNT_FORMAT.format(value);
}

/** "1 item", "3 items". */
export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${formatCount(count)} ${count === 1 ? singular : pluralForm}`;
}

/** Percentage 0..100 of done/total, `null` when the total is unknown or zero. */
export function percent(done: number, total: number | null): number | null {
  if (total === null || !Number.isFinite(total) || total <= 0) {
    return null;
  }
  return Math.max(0, Math.min(100, (done / total) * 100));
}
