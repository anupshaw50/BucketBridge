/** The last segment of an application path, without the `.app` or `.exe` ending, for display. */
export function applicationName(path: string): string {
  const name = path.split(/[\\/]/).filter((part) => part.length > 0).pop() ?? path;
  return name.replace(/\.(app|exe|bat|cmd|com)$/i, "");
}

/** Whether `value` is a valid redirect location: a path or an http(s) URL (empty clears it). */
export function redirectProblem(value: string): string | null {
  const text = value.trim();
  if (text.length === 0 || /^(\/|https?:\/\/)/.test(text)) {
    return null;
  }
  return "Use a path that starts with / or an http:// or https:// URL.";
}
