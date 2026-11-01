// Helpers for S3 keys and prefixes. Keys ending in "/" denote prefixes
// ("folders"). Keys may contain spaces, any Unicode and empty segments ("a//b").

/** Shown wherever a key segment is empty, for example the middle of "a//b". */
export const EMPTY_SEGMENT_LABEL = "(empty name)";

/** S3 limits keys to 1024 bytes of UTF-8. */
export const MAX_KEY_BYTES = 1024;

export function isFolderKey(key: string): boolean {
  return key.endsWith("/");
}

/** Parent prefix of a key or prefix: "a/b/c.txt" and "a/b/c/" both give "a/b/". */
export function parentPrefix(key: string): string {
  const trimmed = isFolderKey(key) ? key.slice(0, -1) : key;
  const index = trimmed.lastIndexOf("/");
  return index === -1 ? "" : trimmed.slice(0, index + 1);
}

/** Last segment without the trailing slash: "a/b/" gives "b", "a//" gives "". */
export function baseName(key: string): string {
  const trimmed = isFolderKey(key) ? key.slice(0, -1) : key;
  return trimmed.slice(trimmed.lastIndexOf("/") + 1);
}

/**
 * Name of a listed key relative to the listing prefix, without the trailing
 * slash of folders. In flat listings this keeps the inner path ("2024/a.jpg").
 */
export function relativeName(key: string, prefix: string): string {
  const relative = key.startsWith(prefix) ? key.slice(prefix.length) : key;
  return isFolderKey(relative) ? relative.slice(0, -1) : relative;
}

/** Text for a key segment, making empty segments visible. */
export function displaySegment(name: string): string {
  return name === "" ? EMPTY_SEGMENT_LABEL : name;
}

export interface Crumb {
  name: string;
  prefix: string;
}

/** Breadcrumb segments of a prefix: "a//b/" gives a, (empty), b with their prefixes. */
export function breadcrumbs(prefix: string): Crumb[] {
  const crumbs: Crumb[] = [];
  let start = 0;
  for (let index = 0; index < prefix.length; index += 1) {
    if (prefix[index] === "/") {
      crumbs.push({ name: prefix.slice(start, index), prefix: prefix.slice(0, index + 1) });
      start = index + 1;
    }
  }
  return crumbs;
}

export function childKey(prefix: string, name: string, folder: boolean): string {
  return `${prefix}${name}${folder ? "/" : ""}`;
}

export function utf8Length(value: string): number {
  return new TextEncoder().encode(value).length;
}

/**
 * Validates a single new object or folder name typed by the user. Returns an
 * error message, or `null` when the name is acceptable.
 */
export function validateNewName(
  name: string,
  options: { prefix: string; folder: boolean; existingKeys?: ReadonlySet<string> },
): string | null {
  if (name.length === 0) {
    return "Enter a name.";
  }
  if (name.trim().length === 0) {
    return "The name cannot consist of spaces only.";
  }
  if (name.includes("/")) {
    return "A name cannot contain a slash.";
  }
  if (name === "." || name === "..") {
    return "This name is reserved.";
  }
  const key = childKey(options.prefix, name, options.folder);
  if (utf8Length(key) > MAX_KEY_BYTES) {
    return `The full key would exceed ${MAX_KEY_BYTES} bytes.`;
  }
  if (options.existingKeys?.has(key)) {
    return options.folder ? "A folder with this name already exists." : "An object with this name already exists.";
  }
  return null;
}

/** Where a selected key lands under a destination prefix (the parent part is stripped). */
export function destinationKey(key: string, destinationPrefix: string): string {
  return childKey(destinationPrefix, baseName(key), isFolderKey(key));
}

/** Destination names that more than one source would take, in first-seen order. */
export function collidingNames(destinations: readonly string[]): string[] {
  const seen = new Set<string>();
  const colliding = new Set<string>();
  for (const destination of destinations) {
    if (seen.has(destination)) {
      colliding.add(destination);
    } else {
      seen.add(destination);
    }
  }
  return [...colliding];
}

/** Message for sources that would land on the same destination name. */
export function sameNameMessage(name: string, verb: string): string {
  return `Two or more selected items have the same name "${displaySegment(name)}" and would ${verb} to the same place. Select them separately.`;
}

/**
 * Why downloading `keys` into one folder is not possible: two of them would
 * get the same local name (a file and a folder with one name also collide).
 */
export function downloadProblem(keys: readonly string[]): string | null {
  const [first] = collidingNames(keys.map((key) => baseName(key)));
  return first === undefined ? null : sameNameMessage(first, "download");
}

/**
 * Explains why pasting `keys` from `sourceBucket` into the destination is not
 * possible, or returns `null` when it is.
 */
export function pasteProblem(
  sourceBucket: string,
  keys: readonly string[],
  destinationBucket: string,
  destinationPrefix: string,
): string | null {
  if (keys.length === 0) {
    return "The clipboard is empty.";
  }
  if (sourceBucket === destinationBucket) {
    for (const key of keys) {
      if (parentPrefix(key) === destinationPrefix) {
        return "The items are already in this folder.";
      }
      if (isFolderKey(key) && destinationPrefix.startsWith(key)) {
        return "A folder cannot be pasted into itself or one of its subfolders.";
      }
    }
  }
  const [first] = collidingNames(keys.map((key) => destinationKey(key, destinationPrefix)));
  return first === undefined ? null : sameNameMessage(baseName(first), "be pasted");
}

/** Ensures a typed prefix ends with "/" unless it is empty (the bucket root). */
export function normalizePrefixInput(value: string): string {
  if (value === "" || value.endsWith("/")) {
    return value;
  }
  return `${value}/`;
}

export function s3Uri(bucket: string, prefix: string): string {
  return `s3://${bucket}/${prefix}`;
}

/** Last component of a local path, accepting both "/" and "\\" separators. */
export function localBaseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const index = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const name = index === -1 ? trimmed : trimmed.slice(index + 1);
  return name === "" ? path : name;
}
