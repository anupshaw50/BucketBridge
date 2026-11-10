import { Alert, Button, Checkbox, Group, Loader, Progress, Stack, Table, Text, TextInput } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type {
  CloneBucketResult,
  ClonePreview,
  ClonePreviewWarning,
  CloneSetting,
  CloneSettingPreview,
  CloneSettingResult,
  BucketUsage,
} from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";
import { validateBucketName, validateRegion } from "../../lib/validation";

let jobCounter = 0;

function newJobId(): string {
  jobCounter += 1;
  return `clone-${Date.now().toString(36)}-${jobCounter}`;
}

function settingLabel(setting: CloneSetting): string {
  switch (setting) {
    case "versioning":
      return tr("Versioning");
    case "encryption":
      return tr("Default encryption");
    case "public_access_block":
      return tr("Public access block");
    case "policy":
      return tr("Bucket policy");
    case "website":
      return tr("Static website hosting");
    case "cors":
      return tr("CORS rules");
    case "tags":
      return tr("Bucket tags");
    case "lifecycle":
      return tr("Lifecycle rules");
  }
}

/** What the source has for one setting, in words. */
function sourceText(item: CloneSettingPreview): string {
  switch (item.state) {
    case "not_configured":
      return tr("Not set on the source");
    case "unsupported":
      return tr("Not supported by this server");
    case "failed":
      return tr("Could not be read: {message}", { message: item.error?.message ?? "" });
    case "configured":
      break;
  }
  const count = item.count ?? 0;
  switch (item.setting) {
    case "versioning":
      return item.detail === "suspended" ? tr("Suspended") : tr("Enabled");
    case "encryption":
      return item.detail ?? "";
    case "public_access_block":
      return tr("Blocking {count} of 4", { count });
    case "policy":
      return tr("Statements: {count}", { count });
    case "website":
      return tr("Index document: {name}", { name: item.detail ?? "" });
    case "cors":
    case "lifecycle":
      return tr("Rules: {count}", { count });
    case "tags":
      return tr("Tags: {count}", { count });
  }
}

function warningText(warning: ClonePreviewWarning): string {
  switch (warning) {
    case "policy_denies":
      return tr("The policy has Deny statements. Settings are applied before the objects are copied, so a policy that denies you access can block the copy. Clear Bucket policy and copy it afterwards if the copy fails.");
    case "kms_key":
      return tr("The default encryption uses a KMS key. The key must exist in the region of the new bucket (or be a multi-region key), or this setting is refused.");
    case "versions_not_copied":
      return tr("Versioning is on in the source. Only the current version of each object is copied, not the older versions.");
  }
}

function resultText(result: CloneSettingResult): string {
  switch (result.state) {
    case "applied":
      return tr("Copied");
    case "skipped":
      return tr("Nothing to copy");
    case "failed":
      return tr("Failed: {message}", { message: result.error?.message ?? "" });
  }
}

/** The copy tasks of one clone, with how far they are. */
function CopyProgress({ result }: { result: CloneBucketResult }) {
  const model = useModel();
  const tasks = useSlice(model.transfers.store, (state) => state.tasks);
  const mine = result.tasks.map((task) => tasks.get(task.id) ?? task);
  const done = mine.filter((task) => task.state === "completed" || task.state === "skipped").length;
  const failed = mine.filter((task) => task.state === "failed").length;
  const total = mine.length;
  return (
    <Stack gap={4} aria-live="polite">
      <Text size="sm">
        {tr("{done} of {total} objects copied. The copy runs in the transfer queue, so you can close this window.", {
          done: formatCount(done),
          total: formatCount(total),
        })}
      </Text>
      <Progress value={total === 0 ? 100 : (done / total) * 100} aria-label={tr("Copy progress")} />
      {failed > 0 ? (
        <Text size="sm" c="red">
          {tr("{count} copies failed. Retry them from the transfers list.", { count: formatCount(failed) })}
        </Text>
      ) : null}
    </Stack>
  );
}

/**
 * Clones a bucket: a new bucket with the chosen settings of this one (policy, CORS, lifecycle, versioning,
 * tags, encryption, website, public access block), and optionally its objects, copied by the transfer queue.
 */
export function CloneBucketDialog({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const account = useSlice(model.session.store, (state) => state.accounts.find((item) => item.id === accountId));
  const [preview, setPreview] = useState<ClonePreview | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [name, setName] = useState(`${bucket}-copy`.slice(0, 63));
  const [region, setRegion] = useState("");
  const [chosen, setChosen] = useState<ReadonlySet<CloneSetting>>(new Set());
  const [copyObjects, setCopyObjects] = useState(true);
  const [prefix, setPrefix] = useState("");
  const [usage, setUsage] = useState<{ status: "idle" | "running" | "done" | "failed"; usage?: BucketUsage; message?: string }>({ status: "idle" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CloneBucketResult | null>(null);
  const job = useRef<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    model.api
      .preview_bucket_clone({ account_id: accountId, bucket })
      .then((found) => {
        if (!mounted.current) {
          return;
        }
        setPreview(found);
        setChosen(new Set(found.settings.filter((item) => item.state === "configured").map((item) => item.setting)));
      })
      .catch((raw: unknown) => {
        if (mounted.current) {
          setLoadError(toBbError(raw).message);
        }
      });
    return () => {
      mounted.current = false;
      if (job.current !== null) {
        void model.api.cancel_bucket_usage({ job_id: job.current }).catch(() => undefined);
      }
    };
  }, [model, accountId, bucket]);

  const nameError = name.length > 0 ? validateBucketName(name) : null;
  const regionError = region.trim().length > 0 ? validateRegion(region.trim()) : null;
  const prefixError = prefix !== "" && !prefix.endsWith("/") ? tr("A prefix is a folder: it ends with /.") : null;
  const ready = preview !== null && validateBucketName(name) === null && regionError === null && (!copyObjects || prefixError === null);

  const countObjects = async () => {
    const id = newJobId();
    job.current = id;
    setUsage({ status: "running" });
    try {
      const found = await model.api.compute_bucket_usage({
        account_id: accountId,
        bucket,
        prefix: copyObjects ? prefix : "",
        versions: false,
        multipart: false,
        job_id: id,
      });
      if (mounted.current && job.current === id) {
        setUsage({ status: "done", usage: found });
      }
    } catch (raw) {
      if (mounted.current && job.current === id) {
        setUsage({ status: "failed", message: toBbError(raw).message });
      }
    } finally {
      if (job.current === id) {
        job.current = null;
      }
    }
  };

  const submit = async () => {
    if (!ready) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const cloned = await model.api.clone_bucket({
        account_id: accountId,
        source_bucket: bucket,
        new_bucket: name,
        region: region.trim().length > 0 ? region.trim() : null,
        prefix: copyObjects ? prefix : "",
        copy_objects: copyObjects,
        settings: [...chosen],
      });
      setResult(cloned);
      model.status.info(tr("Bucket {name} created.", { name }));
      void model.browser.loadBuckets(accountId);
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const total = usage.status === "done" ? usage.usage : undefined;
  const settingsChosen = chosen.size;
  return (
    <AppModal opened onClose={onClose} title={tr("Clone bucket: {bucket}", { bucket })} size="lg" closeOnClickOutside={false}>
      <Stack gap="sm">
        {result === null ? (
          <>
            <Text size="sm">
              {tr("Creates a new bucket with the settings you choose, and can copy the objects into it. The source bucket is not changed.")}
            </Text>
            <Group grow align="flex-start">
              <TextInput
                label={tr("New bucket name")}
                description={tr("3 to 63 characters: lowercase letters, digits, dots and hyphens.")}
                value={name}
                onChange={(event) => setName(event.currentTarget.value.trim())}
                error={nameError}
                required
                spellCheck={false}
                autoComplete="off"
                data-autofocus
              />
              <TextInput
                label={tr("Region (optional)")}
                description={tr("Leave empty to use the region of the source{details}.", {
                  details: preview?.region !== null && preview?.region !== undefined ? ` (${preview.region})` : account ? ` (${account.region})` : "",
                })}
                value={region}
                onChange={(event) => setRegion(event.currentTarget.value)}
                error={regionError}
                spellCheck={false}
              />
            </Group>
            {loadError !== null ? (
              <Alert color="red" role="alert" py="xs">
                {loadError}
              </Alert>
            ) : preview === null ? (
              <Group gap="xs">
                <Loader size="xs" />
                <Text size="sm" c="dimmed">
                  {tr("Reading the settings of the source…")}
                </Text>
              </Group>
            ) : (
              <>
                <Table aria-label={tr("Settings to copy")} verticalSpacing={4} fz="sm">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th w={36} />
                      <Table.Th>{tr("Setting")}</Table.Th>
                      <Table.Th>{tr("On the source")}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {preview.settings.map((item) => (
                      <Table.Tr key={item.setting}>
                        <Table.Td>
                          <Checkbox
                            aria-label={settingLabel(item.setting)}
                            checked={chosen.has(item.setting)}
                            disabled={item.state !== "configured"}
                            onChange={(event) => {
                              const on = event.currentTarget.checked;
                              setChosen((current) => {
                                const next = new Set(current);
                                if (on) {
                                  next.add(item.setting);
                                } else {
                                  next.delete(item.setting);
                                }
                                return next;
                              });
                            }}
                          />
                        </Table.Td>
                        <Table.Td>{settingLabel(item.setting)}</Table.Td>
                        <Table.Td c={item.state === "configured" ? undefined : "dimmed"} style={{ overflowWrap: "anywhere" }}>
                          {sourceText(item)}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
                {preview.warnings
                  .filter((warning) => warning !== "policy_denies" || chosen.has("policy"))
                  .filter((warning) => warning !== "kms_key" || chosen.has("encryption"))
                  .map((warning) => (
                    <Alert key={warning} color="yellow" py="xs">
                      {warningText(warning)}
                    </Alert>
                  ))}
                <Stack gap={4}>
                  <Checkbox
                    label={tr("Copy the objects")}
                    description={tr("The objects are copied on the server by the transfer queue, with their keys unchanged. Older versions are not copied.")}
                    checked={copyObjects}
                    onChange={(event) => {
                      setCopyObjects(event.currentTarget.checked);
                      setUsage({ status: "idle" });
                    }}
                  />
                  {copyObjects ? (
                    <Group align="flex-end" gap="xs" wrap="nowrap">
                      <TextInput
                        label={tr("Only objects under this prefix (optional)")}
                        placeholder="photos/2024/"
                        value={prefix}
                        onChange={(event) => {
                          setPrefix(event.currentTarget.value);
                          setUsage({ status: "idle" });
                        }}
                        error={prefixError}
                        spellCheck={false}
                        style={{ flex: 1 }}
                      />
                      <Button variant="default" loading={usage.status === "running"} disabled={prefixError !== null} onClick={() => void countObjects()}>
                        {tr("Count objects")}
                      </Button>
                    </Group>
                  ) : null}
                  {usage.status === "done" && total !== undefined ? (
                    <Text size="sm" aria-live="polite">
                      {tr("{count} objects, {size}", { count: formatCount(total.objects), size: formatBytes(total.bytes) })}
                    </Text>
                  ) : null}
                  {usage.status === "failed" ? (
                    <Text size="sm" c="red" role="alert">
                      {usage.message}
                    </Text>
                  ) : null}
                </Stack>
                <Text size="sm" fw={500} aria-live="polite">
                  {copyObjects
                    ? tr("What will happen: create {name}, copy {count} settings and the objects.", { name: name === "" ? "…" : name, count: settingsChosen })
                    : tr("What will happen: create {name} and copy {count} settings.", { name: name === "" ? "…" : name, count: settingsChosen })}
                </Text>
              </>
            )}
            {error !== null ? (
              <Alert color="red" role="alert" py="xs">
                {error}
              </Alert>
            ) : null}
            <Group justify="flex-end">
              <Button variant="default" onClick={onClose}>
                {tr("Cancel")}
              </Button>
              <Button disabled={!ready} loading={busy} onClick={() => void submit()}>
                {tr("Clone bucket")}
              </Button>
            </Group>
          </>
        ) : (
          <>
            <Text size="sm">{tr("Bucket {name} was created.", { name: result.outcome.bucket })}</Text>
            {result.outcome.results.length > 0 ? (
              <Table aria-label={tr("Settings copied")} verticalSpacing={4} fz="sm">
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>{tr("Setting")}</Table.Th>
                    <Table.Th>{tr("Result")}</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {result.outcome.results.map((item) => (
                    <Table.Tr key={item.setting}>
                      <Table.Td>{settingLabel(item.setting)}</Table.Td>
                      <Table.Td c={item.state === "failed" ? "red" : undefined} style={{ overflowWrap: "anywhere" }}>
                        {resultText(item)}
                      </Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            ) : null}
            {result.copy_error !== null ? (
              <Alert color="red" role="alert" py="xs" title={tr("The objects could not be queued")}>
                {result.copy_error.message}
              </Alert>
            ) : result.tasks.length > 0 ? (
              <CopyProgress result={result} />
            ) : copyObjects ? (
              <Text size="sm" c="dimmed">
                {tr("There are no objects to copy.")}
              </Text>
            ) : null}
            <Group justify="flex-end">
              <Button
                variant="default"
                onClick={() => {
                  onClose();
                  if (model.session.store.getState().activeAccountId === accountId) {
                    void model.browser.openLocation(result.outcome.bucket, "");
                  }
                }}
              >
                {tr("Open the new bucket")}
              </Button>
              <Button onClick={onClose}>{tr("Close")}</Button>
            </Group>
          </>
        )}
      </Stack>
    </AppModal>
  );
}
