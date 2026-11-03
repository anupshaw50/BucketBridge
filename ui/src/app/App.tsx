import "@mantine/core/styles.css";
import "@mantine/notifications/styles.css";
import "./app.css";
import { MantineProvider } from "@mantine/core";
import { useColorScheme } from "@mantine/hooks";
import { Notifications, notifications } from "@mantine/notifications";
import { type ReactNode, useEffect, useState } from "react";
import type { Backend } from "../api/backend";
import type { Platform } from "../api/platform";
import type { WindowLocation } from "../api/types";
import { ModalLayersProvider } from "../components/AppModal";
import { DialogHost } from "../features/DialogHost";
import { ConflictHost } from "../features/transfers/ConflictDialog";
import { SessionPrompts } from "../features/accounts/SessionPrompts";
import { AppModel } from "../state/model";
import type { FrameScheduler } from "../state/scheduler";
import type { Notice, Notifier } from "../state/status";
import { ModelContext, useModel, useSlice } from "./context";
import { MainWindow } from "./MainWindow";
import { appTheme } from "./theme";

export interface AppServices {
  backend: Backend;
  platform: Platform;
  /** UI the platform needs (the browser mock renders its file chooser). */
  platformUi?: ReactNode;
  /** Location this window opens at (a window opened with "Open in new window"). */
  startAt?: WindowLocation | null;
}

export interface AppProps {
  services: AppServices;
  /** "test" disables transitions and portals (component tests). */
  env?: "default" | "test";
  /** Overrides toast notifications (tests record them). */
  notify?: Notifier;
  /** Overrides the per-frame batching of live updates (tests). */
  scheduler?: FrameScheduler;
}

const showNotice: Notifier = (notice: Notice) => {
  notifications.show({
    title: notice.title,
    message: notice.text,
    color: notice.level === "error" ? "red" : "indigo",
    autoClose: notice.level === "error" ? 8000 : 4000,
  });
};

function Themed({ env, platformUi }: { env: "default" | "test"; platformUi: ReactNode }) {
  const model = useModel();
  const saved = useSlice(model.settings.store, (state) => state.settings.interface.theme);
  const preview = useSlice(model.settings.store, (state) => state.previewTheme);
  const system = useColorScheme("light");
  const theme = preview ?? saved;
  const scheme = theme === "system" ? system : theme;
  return (
    <MantineProvider theme={appTheme} forceColorScheme={scheme} env={env}>
      <Notifications position="bottom-right" limit={4} />
      <ModalLayersProvider>
        <MainWindow />
        <DialogHost />
        <ConflictHost />
        <SessionPrompts />
        {platformUi}
      </ModalLayersProvider>
    </MantineProvider>
  );
}

/** Root component: owns the application model for its lifetime. */
export function App({ services, env = "default", notify, scheduler }: AppProps) {
  const [model] = useState(
    () =>
      new AppModel({
        backend: services.backend,
        platform: services.platform,
        startAt: services.startAt ?? null,
        notify: notify ?? showNotice,
        ...(scheduler ? { scheduler } : {}),
      }),
  );
  useEffect(() => model.start(), [model]);
  return (
    <ModelContext value={model}>
      <Themed env={env} platformUi={services.platformUi ?? null} />
    </ModelContext>
  );
}
