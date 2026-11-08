import { isTauri } from "@tauri-apps/api/core";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App, type AppServices } from "./app/App";
import { tr } from "./i18n";
import { startLocationFromSearch } from "./lib/startLocation";

async function loadServices(): Promise<AppServices> {
  if (isTauri()) {
    const [{ createTauriBackend }, { createTauriPlatform }] = await Promise.all([
      import("./api/tauriBackend"),
      import("./api/tauriPlatform"),
    ]);
    return { backend: createTauriBackend(), platform: createTauriPlatform(), startAt: startLocationFromSearch(window.location.search) };
  }
  // Outside Tauri (a plain browser, the Vite dev server, end-to-end tests) the
  // in-memory mock backend stands in for the Rust host. It is a separate chunk
  // that the Tauri build never loads.
  const { createMockEnvironment, mockOptionsFromQuery } = await import("./mock");
  const environment = createMockEnvironment(mockOptionsFromQuery(window.location.search));
  return {
    backend: environment.backend,
    platform: environment.platform,
    platformUi: environment.platformUi,
    startAt: startLocationFromSearch(window.location.search),
  };
}

const container = document.getElementById("root");
if (container === null) {
  throw new Error("The page has no #root element.");
}
const root = createRoot(container);
loadServices().then(
  (services) => {
    root.render(
      <StrictMode>
        <App services={services} />
      </StrictMode>,
    );
  },
  (error: unknown) => {
    const detail = error instanceof Error ? error.message : String(error);
    container.textContent = tr("BucketBridge could not start: {message}", { message: detail });
  },
);
