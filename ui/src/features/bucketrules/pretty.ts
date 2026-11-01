/** Pretty-prints a document that was read from the bucket. */
export function pretty(value: unknown): string {
  return JSON.stringify(value, null, 2);
}
