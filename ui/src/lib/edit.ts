import type { EditState } from "../api/types";

/** `name (edited).ext`, or `name (edited N).ext` from 2 on: the key "Save as a copy" uses. Mirrors the core. */
export function copyKey(key: string, n = 1): string {
  const slash = key.lastIndexOf("/");
  const dir = key.slice(0, slash + 1);
  const name = key.slice(slash + 1);
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const extension = dot > 0 ? name.slice(dot) : "";
  return n <= 1 ? `${dir}${stem} (edited)${extension}` : `${dir}${stem} (edited ${n})${extension}`;
}

/** What a session state is called in the lists. */
export function editStateLabel(state: EditState): string {
  switch (state) {
    case "watching":
      return "Waiting for a save";
    case "uploading":
      return "Uploading";
    case "uploaded":
      return "Uploaded";
    case "conflict":
      return "Conflict";
    case "error":
      return "Upload failed";
    case "stopped":
      return "Stopped";
  }
}
