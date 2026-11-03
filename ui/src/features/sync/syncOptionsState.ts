import type { FilterSpec, SyncOptions } from "../../api/types";

/** The sync options as the form edits them (text areas instead of lists). */
export interface SyncOptionsState {
  includeText: string;
  excludeText: string;
  excludeFile: string;
  skipEmptyFolders: boolean;
  skipDuplicateContent: boolean;
  useCache: boolean;
  parallelism: number;
  /** Criteria the form does not edit; kept so that editing a job does not drop them. */
  filter: FilterSpec;
}

export const EMPTY_FILTER: FilterSpec = {
  prefix: null,
  glob: null,
  min_size: null,
  max_size: null,
  modified_after: null,
  modified_before: null,
  storage_classes: [],
};

export function defaultOptionsState(): SyncOptionsState {
  return {
    includeText: "",
    excludeText: "",
    excludeFile: "",
    skipEmptyFolders: true,
    skipDuplicateContent: false,
    useCache: true,
    parallelism: 4,
    filter: EMPTY_FILTER,
  };
}

export function optionsToState(options: SyncOptions): SyncOptionsState {
  return {
    includeText: options.include.join("\n"),
    excludeText: options.exclude.join("\n"),
    excludeFile: options.exclude_file ?? "",
    skipEmptyFolders: options.skip_empty_folders,
    skipDuplicateContent: options.skip_duplicate_content,
    useCache: options.use_cache,
    parallelism: options.parallelism,
    filter: options.filter,
  };
}

function lines(text: string, trim: boolean): string[] {
  return text
    .split("\n")
    .map((line) => line.replace(/\r$/, ""))
    .map((line) => (trim ? line.trim() : line))
    .filter((line) => line.trim() !== "");
}

export function stateToOptions(state: SyncOptionsState): SyncOptions {
  return {
    filter: state.filter,
    include: lines(state.includeText, true),
    exclude: lines(state.excludeText, false),
    exclude_file: state.excludeFile.trim() === "" ? null : state.excludeFile.trim(),
    skip_empty_folders: state.skipEmptyFolders,
    skip_duplicate_content: state.skipDuplicateContent,
    use_cache: state.useCache,
    parallelism: state.parallelism,
  };
}
