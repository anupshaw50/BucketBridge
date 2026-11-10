/** Broad kinds of files, for the icon and colour of a row in the object list. */
export type FileKind = "image" | "video" | "audio" | "archive" | "pdf" | "code" | "data" | "text" | "other";

const KINDS: Record<Exclude<FileKind, "other">, readonly string[]> = {
  image: ["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg", "ico", "tif", "tiff", "heic", "heif", "raw", "cr2", "nef", "dng"],
  video: ["mp4", "mov", "m4v", "mkv", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "ts"],
  audio: ["mp3", "wav", "flac", "aac", "ogg", "oga", "m4a", "opus", "wma"],
  archive: ["zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "tar", "jar", "war", "dmg", "iso", "deb", "rpm", "msi", "appimage"],
  pdf: ["pdf"],
  code: ["json", "xml", "yaml", "yml", "toml", "js", "mjs", "ts", "tsx", "jsx", "py", "rs", "go", "java", "kt", "c", "h", "cpp", "cs", "rb", "php", "sh", "ps1", "sql", "html", "htm", "css", "scss", "tf", "ini", "conf"],
  data: ["csv", "tsv", "tab", "parquet", "parq", "xlsx", "xls", "ods", "avro", "orc", "jsonl", "ndjson", "db", "sqlite"],
  text: ["txt", "md", "markdown", "log", "rtf", "doc", "docx", "odt"],
};

const BY_EXTENSION = new Map<string, FileKind>();
for (const [kind, extensions] of Object.entries(KINDS) as [FileKind, readonly string[]][]) {
  for (const extension of extensions) {
    if (!BY_EXTENSION.has(extension)) BY_EXTENSION.set(extension, kind);
  }
}

/** The kind of a file from the extension of its name (case-insensitive). */
export function fileKind(name: string): FileKind {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "other";
  return BY_EXTENSION.get(name.slice(dot + 1).toLowerCase()) ?? "other";
}
