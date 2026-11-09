import { Button, EmptyState, Group, Loader, Stack, Text } from "@mantine/core";
import { IconUserPlus } from "@tabler/icons-react";
import { useEffect } from "react";
import { BrandLogo } from "../components/BrandLogo";
import { BookmarksSection } from "../features/bookmarks/BookmarksSection";
import { BucketTree } from "../features/buckets/BucketTree";
import { LogPanel } from "../features/log/LogPanel";
import { ObjectBrowser } from "../features/objects/ObjectBrowser";
import { TransferPanel } from "../features/transfers/TransferPanel";
import { tr } from "../i18n";
import { useModel, useSlice } from "./context";
import { StatusBar } from "./StatusBar";
import { Toolbar } from "./Toolbar";

function Welcome() {
  const model = useModel();
  return (
    <EmptyState
      icon={<BrandLogo height={40} />}
      title={tr("Welcome to BucketBridge")}
      description={tr("Add an S3 account to browse buckets, transfer files and keep folders in sync. Secret keys are kept in the operating system's credential store.")}
      py="xl"
    >
      <Group justify="center" mt="md">
        <Button leftSection={<IconUserPlus size={16} />} onClick={() => model.dialogs.open({ kind: "add-account" })}>
          {tr("Add account")}
        </Button>
      </Group>
    </EmptyState>
  );
}

/** The main window layout: toolbar, tree and list, optional bottom panels, status bar. */
export function MainWindow() {
  const model = useModel();
  const ready = useSlice(model.app, (state) => state.ready);
  const accountsLoaded = useSlice(model.session.store, (state) => state.accountsLoaded);
  const accountCount = useSlice(model.session.store, (state) => state.accounts.length);
  const accountsError = useSlice(model.session.store, (state) => state.accountsError);
  const showTransfers = useSlice(model.settings.store, (state) => state.settings.interface.show_transfer_panel);
  const showLog = useSlice(model.settings.store, (state) => state.settings.interface.show_log_panel);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "F5" && !event.defaultPrevented) {
        event.preventDefault();
        if (model.session.store.getState().activeAccountId !== null) {
          void model.browser.refresh();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [model]);

  let workspace: React.ReactNode;
  if (!ready && !accountsLoaded) {
    workspace = (
      <Stack align="center" justify="center" h="100%">
        <Loader size="sm" aria-label={tr("Loading")} />
      </Stack>
    );
  } else if (accountCount === 0) {
    workspace = accountsError !== null ? (
      <Stack align="center" justify="center" h="100%" p="md">
        <Text c="red" role="alert">
          {accountsError}
        </Text>
        <Button variant="default" onClick={() => void model.session.refreshAccounts().catch(() => undefined)}>
          {tr("Try again")}
        </Button>
      </Stack>
    ) : (
      <Welcome />
    );
  } else {
    workspace = (
      <div className="bb-workspace">
        <aside className="bb-sidebar" aria-label={tr("Buckets")}>
          <BookmarksSection />
          <BucketTree />
        </aside>
        <main className="bb-content">
          <ObjectBrowser />
        </main>
      </div>
    );
  }

  return (
    <div className="bb-app">
      <Toolbar />
      <div className="bb-main">{workspace}</div>
      {showTransfers ? <TransferPanel /> : null}
      {showLog ? <LogPanel /> : null}
      <StatusBar />
    </div>
  );
}
