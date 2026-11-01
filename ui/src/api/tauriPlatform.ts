import { writeSecret, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { toBbError } from "./errors";
import type { Platform } from "./platform";

async function guarded<T>(operation: () => Promise<T>): Promise<T> {
  try {
    return await operation();
  } catch (error) {
    throw toBbError(error);
  }
}

/** Platform backed by the Tauri dialog, opener and clipboard-manager plugins. */
export function createTauriPlatform(): Platform {
  return {
    kind: "tauri",
    pickFiles: (title) =>
      guarded(async () => {
        const picked = await open({ title, multiple: true, directory: false });
        return picked && picked.length > 0 ? picked : null;
      }),
    pickFolder: (title) =>
      guarded(async () => {
        const picked = await open({ title, multiple: false, directory: true });
        return picked ?? null;
      }),
    pickApplication: (title) =>
      guarded(async () => {
        const system = navigator.userAgent;
        const filters = system.includes("Mac")
          ? [{ name: "Applications", extensions: ["app"] }]
          : system.includes("Windows")
            ? [{ name: "Programs", extensions: ["exe", "bat", "cmd", "com"] }]
            : [];
        const picked = await open({ title, multiple: false, directory: false, filters });
        return picked ?? null;
      }),
    pickSavePath: ({ title, defaultPath, filters }) =>
      guarded(async () => {
        const picked = await save({ title, defaultPath, filters });
        return picked ?? null;
      }),
    openUrl: (url) => guarded(() => openUrl(url)),
    revealPath: (path) => guarded(() => revealItemInDir(path)),
    // Sensitive values (presigned links, passwords) are only ever written with
    // writeSecret; a failure is reported instead of falling back to writeText,
    // which would put them into clipboard history.
    copyText: (text, options) => guarded(() => (options?.sensitive ? writeSecret(text) : writeText(text))),
  };
}
