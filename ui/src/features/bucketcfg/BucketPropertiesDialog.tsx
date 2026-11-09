import { Alert, Button, Group, Skeleton, Stack, Tabs } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { Backend } from "../../api/backend";
import { toBbError } from "../../api/errors";
import type { BucketConfig, Section } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import type { BucketPropertiesTab } from "../../state/dialogs";
import { AccelerationTab } from "./AccelerationTab";
import { LoggingTab } from "./LoggingTab";
import { PolicyTab } from "./PolicyTab";
import { PublicAccessTab } from "./PublicAccessTab";
import { UsageTab } from "./UsageTab";
import { VersioningTab } from "./VersioningTab";
import { WebsiteTab } from "./WebsiteTab";

type TabId = BucketPropertiesTab;

const TABS: { id: TabId; label: string }[] = [
  { id: "policy", label: "Policy" },
  { id: "public-access", label: "Public access" },
  { id: "versioning", label: "Versioning" },
  { id: "logging", label: "Logging" },
  { id: "website", label: "Website" },
  { id: "acceleration", label: "Acceleration" },
  { id: "usage", label: "Usage" },
];

function isTab(value: string): value is TabId {
  return TABS.some((tab) => tab.id === value);
}

/** Counts how often each tab's setting was read again; a tab starts over (new draft) when it changes. */
type Revisions = Record<TabId, number>;

const FIRST_REVISIONS: Revisions = {
  policy: 0,
  "public-access": 0,
  versioning: 0,
  logging: 0,
  website: 0,
  acceleration: 0,
  usage: 0,
};

interface BucketRef {
  account_id: string;
  bucket: string;
}

type LoadState =
  | { status: "loading" }
  | { status: "failed"; message: string }
  | { status: "ready"; config: BucketConfig; revisions: Revisions };

/** Runs one read; a failure is the answer of this setting, not an error of the dialog. */
async function settle<T>(read: () => Promise<T>): Promise<Section<T>> {
  try {
    return { type: "loaded", value: await read() };
  } catch (raw) {
    const error = toBbError(raw);
    return { type: "failed", error: { code: error.code, message: error.message } };
  }
}

/** Reads the setting of one tab on its own. */
async function readSection(api: Backend, tab: TabId, ref: BucketRef): Promise<Partial<BucketConfig>> {
  switch (tab) {
    case "policy":
      return { policy: await settle(() => api.get_bucket_policy(ref)) };
    case "public-access":
      return { public_access_block: await settle(() => api.get_public_access_block(ref)) };
    case "versioning":
      return { versioning: await settle(() => api.get_bucket_versioning(ref)) };
    case "logging":
      return { logging: await settle(() => api.get_bucket_logging(ref)) };
    case "website":
      return { website: await settle(() => api.get_bucket_website(ref)) };
    case "acceleration":
      return { acceleration: await settle(() => api.get_bucket_acceleration(ref)) };
    case "usage":
      return {};
  }
}

/**
 * The properties of a bucket: one tab per setting. The dialog reads every setting once when it opens
 * (`get_bucket_config`, so a setting that fails does not hide the others); a tab reads its own setting
 * again after it saved a change, and when the user retries.
 */
export function BucketPropertiesDialog({
  accountId,
  bucket,
  initialTab = "policy",
  onClose,
}: {
  accountId: string;
  bucket: string;
  /** The tab that is shown when the dialog opens. */
  initialTab?: BucketPropertiesTab;
  onClose: () => void;
}) {
  const model = useModel();
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [tab, setTab] = useState<TabId>(initialTab);

  useEffect(() => {
    const request = { current: true };
    void (async () => {
      try {
        const config = await model.api.get_bucket_config({ account_id: accountId, bucket });
        if (request.current) {
          setState({ status: "ready", config, revisions: FIRST_REVISIONS });
        }
      } catch (raw) {
        if (request.current) {
          setState({ status: "failed", message: toBbError(raw).message });
        }
      }
    })();
    return () => {
      request.current = false;
    };
  }, [model, accountId, bucket, attempt]);

  const reload = async (which: TabId) => {
    const patch = await readSection(model.api, which, { account_id: accountId, bucket });
    setState((previous) =>
      previous.status === "ready"
        ? {
            status: "ready",
            config: { ...previous.config, ...patch },
            revisions: { ...previous.revisions, [which]: previous.revisions[which] + 1 },
          }
        : previous,
    );
  };

  let content;
  if (state.status === "loading") {
    content = (
      <Stack gap={8} aria-busy="true" aria-label={tr("Loading the bucket settings")}>
        {Array.from({ length: 5 }, (_, index) => (
          <Skeleton key={index} height={18} width={`${90 - index * 10}%`} />
        ))}
      </Stack>
    );
  } else if (state.status === "failed") {
    content = (
      <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title={tr("The bucket settings could not be read")}>
        <div style={{ overflowWrap: "anywhere" }}>{state.message}</div>
        <Button
          mt="xs"
          size="compact-xs"
          variant="default"
          onClick={() => {
            setState({ status: "loading" });
            setAttempt((count) => count + 1);
          }}
        >
          {tr("Try again")}
        </Button>
      </Alert>
    );
  } else {
    const { config, revisions } = state;
    content = (
      <Tabs
        value={tab}
        onChange={(value) => {
          if (value !== null && isTab(value)) {
            setTab(value);
          }
        }}
      >
        <Tabs.List>
          {TABS.map(({ id, label }) => (
            <Tabs.Tab key={id} value={id}>
              {tr(label)}
            </Tabs.Tab>
          ))}
        </Tabs.List>
        <Tabs.Panel value="policy" pt="md">
          <PolicyTab
            key={revisions.policy}
            accountId={accountId}
            bucket={bucket}
            section={config.policy}
            reload={() => reload("policy")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="public-access" pt="md">
          <PublicAccessTab
            key={revisions["public-access"]}
            accountId={accountId}
            bucket={bucket}
            section={config.public_access_block}
            reload={() => reload("public-access")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="versioning" pt="md">
          <VersioningTab
            key={revisions.versioning}
            accountId={accountId}
            bucket={bucket}
            section={config.versioning}
            reload={() => reload("versioning")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="logging" pt="md">
          <LoggingTab
            key={revisions.logging}
            accountId={accountId}
            bucket={bucket}
            section={config.logging}
            reload={() => reload("logging")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="website" pt="md">
          <WebsiteTab
            key={revisions.website}
            accountId={accountId}
            bucket={bucket}
            section={config.website}
            reload={() => reload("website")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="acceleration" pt="md">
          <AccelerationTab
            key={revisions.acceleration}
            accountId={accountId}
            bucket={bucket}
            section={config.acceleration}
            reload={() => reload("acceleration")}
          />
        </Tabs.Panel>
        <Tabs.Panel value="usage" pt="md">
          <UsageTab accountId={accountId} bucket={bucket} versioning={config.versioning} />
        </Tabs.Panel>
      </Tabs>
    );
  }

  return (
    <AppModal opened onClose={onClose} title={tr("Bucket properties: {bucket}", { bucket })} size="xl">
      <Stack gap="md">
        {content}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Close")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
