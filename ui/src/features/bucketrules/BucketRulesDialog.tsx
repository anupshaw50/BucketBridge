import { Alert, Button, Group, Select, Skeleton, Stack, Tabs } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import type { Backend } from "../../api/backend";
import { toBbError } from "../../api/errors";
import type { BucketRules, Section, TieringConfig } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import type { BucketRulesTab } from "../../state/dialogs";
import { SectionGate } from "../bucketcfg/common";
import { DocEditor } from "./DocEditor";
import { pretty } from "./pretty";

const TABS: { id: BucketRulesTab; label: string }[] = [
  { id: "cors", label: "CORS" },
  { id: "lifecycle", label: "Lifecycle" },
  { id: "tiering", label: "Intelligent-Tiering" },
  { id: "replication", label: "Replication" },
];

const EXAMPLES: Record<BucketRulesTab, string> = {
  cors: pretty({ rules: [{ id: "web", allowed_origins: ["https://example.org"], allowed_methods: ["GET"], allowed_headers: ["*"], expose_headers: ["ETag"], max_age_seconds: 3000 }] }),
  lifecycle: pretty({
    rules: [{ id: "expire-tmp", status: "enabled", filter: { prefix: "tmp/" }, expiration: { days: 30 }, abort_incomplete_multipart_upload_days: 7 }],
  }),
  tiering: pretty({ id: "archive", status: "enabled", filter: { prefix: "cold/", tags: [] }, tierings: [{ access_tier: "ARCHIVE_ACCESS", days: 90 }] }),
  replication: pretty({
    role: "arn:aws:iam::123456789012:role/replicate",
    rules: [{ id: "all", priority: 1, status: "enabled", filter: { prefix: null, tags: [] }, destination_bucket: "arn:aws:s3:::backup", storage_class: null, delete_marker_replication: false }],
  }),
};

interface Ref {
  account_id: string;
  bucket: string;
}

async function settle<T>(read: () => Promise<T>): Promise<Section<T>> {
  try {
    return { type: "loaded", value: await read() };
  } catch (raw) {
    const error = toBbError(raw);
    return { type: "failed", error: { code: error.code, message: error.message } };
  }
}

async function readTab(api: Backend, tab: BucketRulesTab, ref: Ref): Promise<Partial<BucketRules>> {
  switch (tab) {
    case "cors":
      return { cors: await settle(() => api.get_bucket_cors(ref)) };
    case "lifecycle":
      return { lifecycle: await settle(() => api.get_bucket_lifecycle(ref)) };
    case "tiering":
      return { tiering: await settle(() => api.list_bucket_tiering(ref)) };
    case "replication":
      return { replication: await settle(() => api.get_bucket_replication(ref)) };
  }
}

const NEW_CONFIGURATION = "";

/** Intelligent-Tiering has several named configurations: pick one, or start a new one. */
function TieringEditor({ accountId, bucket, configs, reload }: { accountId: string; bucket: string; configs: TieringConfig[]; reload: () => Promise<void> }) {
  const model = useModel();
  const ref = { account_id: accountId, bucket };
  const [selected, setSelected] = useState(configs[0]?.id ?? NEW_CONFIGURATION);
  const current = configs.find((config) => config.id === selected) ?? null;
  return (
    <Stack gap="sm">
      <Select
        label={tr("Configuration")}
        allowDeselect={false}
        value={selected}
        onChange={(value) => setSelected(value ?? NEW_CONFIGURATION)}
        data={[{ value: NEW_CONFIGURATION, label: tr("New configuration") }, ...configs.map((config) => ({ value: config.id, label: config.id }))]}
      />
      <DocEditor
        key={`${selected}|${current === null ? "" : pretty(current)}`}
        label={tr("Intelligent-Tiering configuration (JSON)")}
        noun={tr("Intelligent-Tiering configuration")}
        bucket={bucket}
        initial={current === null ? null : pretty(current)}
        example={EXAMPLES.tiering}
        validate={(document) => model.api.validate_bucket_tiering({ document })}
        save={async (value) => {
          const config = value as TieringConfig;
          await model.api.put_bucket_tiering({ ...ref, config });
          setSelected(config.id);
        }}
        remove={
          current === null
            ? null
            : async () => {
                await model.api.delete_bucket_tiering({ ...ref, id: current.id });
                setSelected(NEW_CONFIGURATION);
              }
        }
        reload={reload}
      />
    </Stack>
  );
}

/**
 * The rules of a bucket: CORS, lifecycle, Intelligent-Tiering and replication, one tab each with a JSON view. The
 * dialog reads every setting once (a setting the server does not offer does not hide the others); a tab reads its own
 * setting again after a save.
 */
export function BucketRulesDialog({ accountId, bucket, initialTab = "cors", onClose }: { accountId: string; bucket: string; initialTab?: BucketRulesTab; onClose: () => void }) {
  const model = useModel();
  const ref = { account_id: accountId, bucket };
  const [rules, setRules] = useState<BucketRules | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [revisions, setRevisions] = useState<Record<BucketRulesTab, number>>({ cors: 0, lifecycle: 0, tiering: 0, replication: 0 });
  const [attempt, setAttempt] = useState(0);
  const [tab, setTab] = useState<BucketRulesTab>(initialTab);

  useEffect(() => {
    const request = { current: true };
    void (async () => {
      try {
        const read = await model.api.get_bucket_rules({ account_id: accountId, bucket });
        if (request.current) {
          setRules(read);
          setFailure(null);
        }
      } catch (raw) {
        if (request.current) {
          setFailure(toBbError(raw).message);
        }
      }
    })();
    return () => {
      request.current = false;
    };
  }, [model, accountId, bucket, attempt]);

  const reload = async (which: BucketRulesTab) => {
    const patch = await readTab(model.api, which, { account_id: accountId, bucket });
    setRules((previous) => (previous === null ? previous : { ...previous, ...patch }));
    setRevisions((previous) => ({ ...previous, [which]: previous[which] + 1 }));
  };

  let content;
  if (failure !== null) {
    content = (
      <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title={tr("The bucket rules could not be read")}>
        <div style={{ overflowWrap: "anywhere" }}>{failure}</div>
        <Button
          mt="xs"
          size="compact-xs"
          variant="default"
          onClick={() => {
            setFailure(null);
            setAttempt((count) => count + 1);
          }}
        >
          {tr("Try again")}
        </Button>
      </Alert>
    );
  } else if (rules === null) {
    content = (
      <Stack gap={8} aria-busy="true" aria-label={tr("Loading the bucket rules")}>
        {[90, 80, 70].map((width) => (
          <Skeleton key={width} height={18} width={`${width}%`} />
        ))}
      </Stack>
    );
  } else {
    content = (
      <Tabs value={tab} onChange={(value) => TABS.some((entry) => entry.id === value) && setTab(value as BucketRulesTab)}>
        <Tabs.List>
          {TABS.map(({ id, label }) => (
            <Tabs.Tab key={id} value={id}>
              {tr(label)}
            </Tabs.Tab>
          ))}
        </Tabs.List>
        <Tabs.Panel value="cors" pt="md">
          <SectionGate section={rules.cors} reload={() => reload("cors")}>
            {(config) => (
              <DocEditor
                key={revisions.cors}
                label={tr("CORS rules (JSON)")}
                noun={tr("CORS rules")}
                bucket={bucket}
                initial={config === null ? null : pretty(config)}
                example={EXAMPLES.cors}
                validate={(document) => model.api.validate_bucket_cors({ document })}
                save={(value) => model.api.put_bucket_cors({ ...ref, config: value as never })}
                remove={() => model.api.delete_bucket_cors(ref)}
                reload={() => reload("cors")}
              />
            )}
          </SectionGate>
        </Tabs.Panel>
        <Tabs.Panel value="lifecycle" pt="md">
          <SectionGate section={rules.lifecycle} reload={() => reload("lifecycle")}>
            {(config) => (
              <DocEditor
                key={revisions.lifecycle}
                label={tr("Lifecycle rules (JSON)")}
                noun={tr("lifecycle rules")}
                bucket={bucket}
                initial={config === null ? null : pretty(config)}
                example={EXAMPLES.lifecycle}
                validate={(document) => model.api.validate_bucket_lifecycle({ document })}
                save={(value) => model.api.put_bucket_lifecycle({ ...ref, config: value as never })}
                remove={() => model.api.delete_bucket_lifecycle(ref)}
                reload={() => reload("lifecycle")}
              />
            )}
          </SectionGate>
        </Tabs.Panel>
        <Tabs.Panel value="tiering" pt="md">
          <SectionGate section={rules.tiering} reload={() => reload("tiering")}>
            {(configs) => <TieringEditor key={revisions.tiering} accountId={accountId} bucket={bucket} configs={configs} reload={() => reload("tiering")} />}
          </SectionGate>
        </Tabs.Panel>
        <Tabs.Panel value="replication" pt="md">
          <SectionGate section={rules.replication} reload={() => reload("replication")}>
            {(config) => (
              <DocEditor
                key={revisions.replication}
                label={tr("Replication (JSON)")}
                noun={tr("replication configuration")}
                bucket={bucket}
                initial={config === null ? null : pretty(config)}
                example={EXAMPLES.replication}
                validate={(document) => model.api.validate_bucket_replication({ document })}
                save={(value) => model.api.put_bucket_replication({ ...ref, config: value as never })}
                remove={() => model.api.delete_bucket_replication(ref)}
                reload={() => reload("replication")}
              />
            )}
          </SectionGate>
        </Tabs.Panel>
      </Tabs>
    );
  }

  return (
    <AppModal opened onClose={onClose} title={tr("Bucket rules: {bucket}", { bucket })} size="xl">
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
