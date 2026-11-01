// A tiny in-memory POSIX file system used by the mock backend (uploads read
// from it, downloads and log exports write to it) and by the mock file chooser.

export interface MockFile {
  size: number;
  modified: string;
}

export interface DirEntry {
  name: string;
  path: string;
  kind: "file" | "dir";
  size: number | null;
  modified: string | null;
}

export interface WalkResult {
  files: { path: string; rel: string; file: MockFile }[];
  /** Directories without any entries, relative to the walked root. */
  emptyDirs: { path: string; rel: string }[];
}

/** Normalises an absolute POSIX path: collapses "//", drops "." and trailing "/". */
export function normalizePath(path: string): string | null {
  if (!path.startsWith("/")) {
    return null;
  }
  const parts: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") {
      continue;
    }
    if (part === "..") {
      return null;
    }
    parts.push(part);
  }
  return `/${parts.join("/")}`;
}

export function joinPath(base: string, ...parts: string[]): string {
  const joined = [base, ...parts].join("/");
  return normalizePath(joined) ?? joined;
}

export function dirname(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

export function basename(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export class MockFs {
  private readonly files = new Map<string, MockFile>();
  private readonly dirs = new Set<string>(["/"]);
  private readonly listeners = new Set<() => void>();
  private changeCount = 0;

  get version(): number {
    return this.changeCount;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  mkdirp(path: string): void {
    const normalized = normalizePath(path);
    if (normalized === null) {
      throw new Error(`Invalid path: ${path}`);
    }
    let current = "";
    for (const part of normalized.split("/").filter(Boolean)) {
      current = `${current}/${part}`;
      if (this.files.has(current)) {
        throw new Error(`Not a directory: ${current}`);
      }
      this.dirs.add(current);
    }
    this.changed();
  }

  writeFile(path: string, file: MockFile): void {
    const normalized = normalizePath(path);
    if (normalized === null || normalized === "/") {
      throw new Error(`Invalid path: ${path}`);
    }
    if (this.dirs.has(normalized)) {
      throw new Error(`Is a directory: ${normalized}`);
    }
    this.mkdirp(dirname(normalized));
    this.files.set(normalized, { ...file });
    this.changed();
  }

  remove(path: string): boolean {
    const normalized = normalizePath(path);
    if (normalized === null) {
      return false;
    }
    const removed = this.files.delete(normalized);
    if (removed) {
      this.changed();
    }
    return removed;
  }

  stat(path: string): MockFile | null {
    const normalized = normalizePath(path);
    return normalized === null ? null : (this.files.get(normalized) ?? null);
  }

  isDir(path: string): boolean {
    const normalized = normalizePath(path);
    return normalized !== null && this.dirs.has(normalized);
  }

  isFile(path: string): boolean {
    return this.stat(path) !== null;
  }

  exists(path: string): boolean {
    return this.isDir(path) || this.isFile(path);
  }

  /** Direct children of a directory, folders first, then by name. */
  readDir(path: string): DirEntry[] {
    const dir = normalizePath(path);
    if (dir === null || !this.dirs.has(dir)) {
      return [];
    }
    const base = dir === "/" ? "/" : `${dir}/`;
    const entries: DirEntry[] = [];
    for (const child of this.dirs) {
      if (child !== dir && child.startsWith(base) && !child.slice(base.length).includes("/")) {
        entries.push({ name: child.slice(base.length), path: child, kind: "dir", size: null, modified: null });
      }
    }
    for (const [child, file] of this.files) {
      if (child.startsWith(base) && !child.slice(base.length).includes("/")) {
        entries.push({
          name: child.slice(base.length),
          path: child,
          kind: "file",
          size: file.size,
          modified: file.modified,
        });
      }
    }
    return entries.sort((a, b) => {
      if (a.kind !== b.kind) {
        return a.kind === "dir" ? -1 : 1;
      }
      return a.name.localeCompare(b.name, undefined, { numeric: true });
    });
  }

  /** Every file below a directory, with "/"-separated paths relative to it. */
  walk(path: string): WalkResult {
    const root = normalizePath(path);
    const result: WalkResult = { files: [], emptyDirs: [] };
    if (root === null || !this.dirs.has(root)) {
      return result;
    }
    const base = root === "/" ? "/" : `${root}/`;
    for (const [filePath, file] of this.files) {
      if (filePath.startsWith(base)) {
        result.files.push({ path: filePath, rel: filePath.slice(base.length), file: { ...file } });
      }
    }
    for (const dir of this.dirs) {
      if (dir.startsWith(base) && dir !== root && this.isEmptyDir(dir)) {
        result.emptyDirs.push({ path: dir, rel: dir.slice(base.length) });
      }
    }
    result.files.sort((a, b) => (a.rel < b.rel ? -1 : a.rel > b.rel ? 1 : 0));
    result.emptyDirs.sort((a, b) => (a.rel < b.rel ? -1 : a.rel > b.rel ? 1 : 0));
    return result;
  }

  private isEmptyDir(dir: string): boolean {
    const base = `${dir}/`;
    for (const child of this.dirs) {
      if (child.startsWith(base)) {
        return false;
      }
    }
    for (const file of this.files.keys()) {
      if (file.startsWith(base)) {
        return false;
      }
    }
    return true;
  }

  private changed(): void {
    this.changeCount += 1;
    for (const listener of this.listeners) {
      listener();
    }
  }
}
