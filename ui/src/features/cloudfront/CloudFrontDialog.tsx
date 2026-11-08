import { Alert, Badge, Button, Checkbox, Group, Select, Stack, Table, Tabs, Text, Textarea, TextInput } from "@mantine/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { CloudFrontDistribution, CloudFrontInvalidation, PathSet } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatDateTime } from "../../lib/format";
import type { CloudFrontView } from "../../state/dialogs";
import { AttachDialog, EditDialog } from "./CloudFrontForms";
import { pathsFromText, validatePaths } from "./paths";

const STATUS_COLORS: Record<string, string> = { Completed: "green", InProgress: "yellow", Deployed: "green" };

function StatusBadge({ status }: { status: string }) {
  return (
    <Badge color={STATUS_COLORS[status] ?? "gray"} variant="light">
      {status}
    </Badge>
  );
}

/**
 * The CloudFront manager: the distributions of the account (or the ones that serve one bucket), enable and
 * disable, invalidations with wildcard paths and saved path sets, and the invalidation history as a timeline.
 */
export function CloudFrontDialog({
  accountId,
  bucket,
  view,
  onClose,
}: {
  accountId: string;
  bucket: string | null;
  view: CloudFrontView;
  onClose: () => void;
}) {
  const model = useModel();
  const [tab, setTab] = useState<CloudFrontView>(view === "invalidate" || view === "history" ? view : "distributions");
  const [all, setAll] = useState(bucket === null);
  const [items, setItems] = useState<CloudFrontDistribution[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  // The chosen distribution, or the first one while the choice is not (or no longer) in the list.
  const selected = chosen !== null && items.some((item) => item.id === chosen) ? chosen : (items[0]?.id ?? null);

  const load = useCallback(
    async (marker: string | null) => {
      setLoading(true);
      setError(null);
      try {
        if (all) {
          const page = await model.api.list_cloudfront_distributions({ account_id: accountId, marker });
          setItems((current) => (marker === null ? page.items : [...current, ...page.items]));
          setNext(page.next_marker);
        } else if (bucket !== null) {
          setItems(await model.api.find_cloudfront_distributions({ account_id: accountId, bucket }));
          setNext(null);
        }
      } catch (raw) {
        setError(toBbError(raw).message);
      } finally {
        setLoading(false);
      }
    },
    [model, accountId, bucket, all],
  );

  useEffect(() => {
    queueMicrotask(() => void load(null));
  }, [load]);

  const replace = (updated: CloudFrontDistribution) =>
    setItems((current) => current.map((item) => (item.id === updated.id ? { ...updated, default_root_object: null, in_progress_invalidations: null, etag: null } : item)));

  const toggle = useCallback(
    async (item: CloudFrontDistribution) => {
      const enable = !item.enabled;
      const confirmed = await model.dialogs.confirm(
        enable
          ? { title: tr("Enable distribution"), message: tr("Enable {id}? It starts serving requests again.", { id: item.id }), confirmLabel: tr("Enable") }
          : {
              title: tr("Disable distribution"),
              message: tr("Disable {id}{comment}? Viewers get errors until it is enabled again.\n\nType {id} to confirm.", {
                id: item.id,
                comment: item.comment === "" ? "" : ` (${item.comment})`,
              }),
              confirmLabel: tr("Disable"),
              danger: true,
              typedConfirmation: item.id,
            },
      );
      if (!confirmed) {
        return;
      }
      try {
        const current = await model.api.get_cloudfront_distribution({ account_id: accountId, id: item.id });
        const updated = await model.api.set_cloudfront_enabled({
          account_id: accountId,
          id: item.id,
          enabled: enable,
          expected_etag: current.etag,
        });
        replace(updated);
        model.status.info(enable ? tr("Distribution {id} enabled.", { id: item.id }) : tr("Distribution {id} disabled.", { id: item.id }));
      } catch (raw) {
        model.status.error(raw, { notify: tr("The distribution could not be changed") });
      }
    },
    [model, accountId],
  );

  const detach = useCallback(
    async (item: CloudFrontDistribution) => {
      if (bucket === null) {
        return;
      }
      const confirmed = await model.dialogs.confirm({
        title: tr("Detach distribution"),
        message: tr("Remove the origin of s3://{bucket} from {id}? The distribution and the bucket are kept, but the paths it served from this bucket stop working.\n\nType {bucket} to confirm.", { bucket, id: item.id }),
        confirmLabel: tr("Detach"),
        danger: true,
        typedConfirmation: bucket,
      });
      if (!confirmed) {
        return;
      }
      try {
        const current = await model.api.get_cloudfront_distribution({ account_id: accountId, id: item.id });
        await model.api.detach_cloudfront_bucket({ account_id: accountId, id: item.id, bucket, expected_etag: current.etag });
        model.status.info(tr("Distribution {id} detached from {bucket}.", { id: item.id, bucket }));
        await load(null);
      } catch (raw) {
        model.status.error(raw, { notify: tr("The distribution could not be detached") });
      }
    },
    [model, accountId, bucket, load],
  );

  const [form, setForm] = useState<{ kind: "attach" } | { kind: "edit"; id: string } | null>(view === "attach" ? { kind: "attach" } : null);
  const startedIntent = useRef(false);
  useEffect(() => {
    if ((view !== "edit" && view !== "detach") || startedIntent.current || items.length !== 1) {
      return;
    }
    startedIntent.current = true;
    const [only] = items;
    if (only !== undefined) {
      queueMicrotask(() => (view === "edit" ? setForm({ kind: "edit", id: only.id }) : void detach(only)));
    }
  }, [view, items, detach]);

  const startedToggle = useRef(false);
  useEffect(() => {
    if (view !== "toggle" || startedToggle.current || items.length === 0) {
      return;
    }
    startedToggle.current = true;
    const [first] = items;
    if (first !== undefined && items.length === 1) {
      queueMicrotask(() => void toggle(first));
    }
  }, [view, items, toggle]);

  const choices = items.map((item) => ({ value: item.id, label: `${item.id}${item.comment === "" ? "" : ` ${item.comment}`}` }));

  return (
    <AppModal opened onClose={onClose} title={tr("CloudFront manager")} size="xl">
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          {bucket !== null && !all ? tr("Distributions that serve s3://{bucket}", { bucket }) : tr("Distributions of this account")}
        </Text>
        {bucket !== null && (
          <Checkbox label={tr("All distributions")} checked={all} onChange={(event) => setAll(event.currentTarget.checked)} />
        )}
        {error !== null && (
          <Alert color="red" title={tr("CloudFront request failed")}>
            {error}
          </Alert>
        )}
        {bucket !== null && (
          <Group>
            <Button variant="default" size="compact-sm" onClick={() => setForm({ kind: "attach" })}>
              {tr("Attach distribution…")}
            </Button>
          </Group>
        )}
        <Select
          label={tr("Distribution")}
          data={choices}
          value={selected}
          onChange={setChosen}
          allowDeselect={false}
          comboboxProps={{ withinPortal: false }}
          placeholder={loading ? tr("Loading…") : tr("No distributions")}
        />
        <Tabs keepMounted={false} value={tab} onChange={(next) => setTab((next as CloudFrontView | null) ?? "distributions")}>
          <Tabs.List>
            <Tabs.Tab value="distributions">{tr("Distributions")}</Tabs.Tab>
            <Tabs.Tab value="invalidate">{tr("Invalidate")}</Tabs.Tab>
            <Tabs.Tab value="history">{tr("History")}</Tabs.Tab>
          </Tabs.List>
          <Tabs.Panel value="distributions" pt="sm">
            <Table aria-label={tr("Distributions")}>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>{tr("Id")}</Table.Th>
                  <Table.Th>{tr("Comment")}</Table.Th>
                  <Table.Th>{tr("Domain")}</Table.Th>
                  <Table.Th>{tr("Status")}</Table.Th>
                  <Table.Th>{tr("State")}</Table.Th>
                  <Table.Th />
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {items.map((item) => (
                  <Table.Tr key={item.id} aria-label={tr("Distribution {id}", { id: item.id })}>
                    <Table.Td>{item.id}</Table.Td>
                    <Table.Td>{item.comment}</Table.Td>
                    <Table.Td>{item.domain_name}</Table.Td>
                    <Table.Td>
                      <StatusBadge status={item.status} />
                    </Table.Td>
                    <Table.Td>{item.enabled ? tr("Enabled") : tr("Disabled")}</Table.Td>
                    <Table.Td>
                      <Group gap="xs" wrap="nowrap">
                        <Button
                          size="compact-sm"
                          variant="default"
                          aria-label={item.enabled ? tr("Disable {id}", { id: item.id }) : tr("Enable {id}", { id: item.id })}
                          onClick={() => void toggle(item)}
                        >
                          {item.enabled ? tr("Disable") : tr("Enable")}
                        </Button>
                        <Button size="compact-sm" variant="default" aria-label={tr("Edit {id}", { id: item.id })} onClick={() => setForm({ kind: "edit", id: item.id })}>
                          {tr("Edit")}
                        </Button>
                        {bucket !== null && item.origins.some((origin) => origin.bucket === bucket) && (
                          <Button size="compact-sm" variant="default" color="red" aria-label={tr("Detach {id}", { id: item.id })} onClick={() => void detach(item)}>
                            {tr("Detach")}
                          </Button>
                        )}
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
            {items.length === 0 && !loading && <Text size="sm">{tr("No distributions.")}</Text>}
            {next !== null && (
              <Button mt="xs" variant="default" loading={loading} onClick={() => void load(next)}>
                {tr("Load more")}
              </Button>
            )}
          </Tabs.Panel>
          <Tabs.Panel value="invalidate" pt="sm">
            <InvalidatePanel accountId={accountId} distributionId={selected} />
          </Tabs.Panel>
          <Tabs.Panel value="history" pt="sm">
            <HistoryPanel accountId={accountId} distributionId={selected} />
          </Tabs.Panel>
        </Tabs>
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Close")}
          </Button>
        </Group>
      </Stack>
      {form?.kind === "attach" && bucket !== null && (
        <AttachDialog accountId={accountId} bucket={bucket} onClose={() => setForm(null)} onCreated={() => void load(null)} />
      )}
      {form?.kind === "edit" && <EditDialog accountId={accountId} id={form.id} onClose={() => setForm(null)} onSaved={() => void load(null)} />}
    </AppModal>
  );
}

function InvalidatePanel({ accountId, distributionId }: { accountId: string; distributionId: string | null }) {
  const model = useModel();
  const [text, setText] = useState("/*");
  const [sets, setSets] = useState<PathSet[]>([]);
  const [setName, setSetName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CloudFrontInvalidation | null>(null);

  useEffect(() => {
    model.api
      .list_invalidation_path_sets()
      .then(setSets)
      .catch(() => undefined);
  }, [model]);

  const check = validatePaths(pathsFromText(text));
  const problem = check.ok ? null : check.message;

  const create = async () => {
    if (distributionId === null || !check.ok) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setCreated(await model.api.create_cloudfront_invalidation({ account_id: accountId, distribution_id: distributionId, paths: check.paths }));
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const saveSet = async () => {
    if (!check.ok) {
      return;
    }
    try {
      setSets(await model.api.save_invalidation_path_set({ set: { name: setName, paths: check.paths }, replaces: sets.some((entry) => entry.name.toLowerCase() === setName.trim().toLowerCase()) ? setName : null }));
      model.status.info(tr("Path set {name} saved.", { name: setName.trim() }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const deleteSet = async (name: string) => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Delete path set"),
      message: tr("Delete the path set {name}? Type {name} to confirm.", { name }),
      confirmLabel: tr("Delete"),
      danger: true,
      typedConfirmation: name,
    });
    if (confirmed) {
      try {
        setSets(await model.api.delete_invalidation_path_set({ name }));
      } catch (raw) {
        setError(toBbError(raw).message);
      }
    }
  };

  return (
    <Stack gap="xs">
      <Textarea
        label={tr("Paths")}
        description={tr("One path per line. Start with /, use * only at the end (/* invalidates everything).")}
        value={text}
        onChange={(event) => {
          setText(event.currentTarget.value);
          setCreated(null);
        }}
        rows={4}
        error={problem}
      />
      <Group align="flex-end" gap="xs">
        <Select
          label={tr("Saved path sets")}
          placeholder={sets.length === 0 ? tr("None saved") : tr("Load a set")}
          data={sets.map((entry) => entry.name)}
          value={null}
          onChange={(name) => {
            const found = sets.find((entry) => entry.name === name);
            if (found !== undefined) {
              setText(found.paths.join("\n"));
              setSetName(found.name);
            }
          }}
          comboboxProps={{ withinPortal: false }}
        />
        <TextInput label={tr("Set name")} value={setName} onChange={(event) => setSetName(event.currentTarget.value)} />
        <Button variant="default" disabled={!check.ok || setName.trim() === ""} onClick={() => void saveSet()}>
          {tr("Save set")}
        </Button>
        <Button variant="default" color="red" disabled={!sets.some((entry) => entry.name === setName)} onClick={() => void deleteSet(setName)}>
          {tr("Delete set")}
        </Button>
      </Group>
      {error !== null && <Alert color="red">{error}</Alert>}
      {created !== null && (
        <Alert color="green" title={tr("Invalidation created")}>
          {tr("{id} is {status} for {paths}.", { id: created.id, status: created.status, paths: created.paths.join(", ") })}
        </Alert>
      )}
      <Group>
        <Button loading={busy} disabled={distributionId === null || !check.ok} onClick={() => void create()}>
          {tr("Create invalidation")}
        </Button>
      </Group>
    </Stack>
  );
}

function HistoryPanel({ accountId, distributionId }: { accountId: string; distributionId: string | null }) {
  const model = useModel();
  const [items, setItems] = useState<CloudFrontInvalidation[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paths, setPaths] = useState<Record<string, string[]>>({});

  const load = useCallback(
    async (marker: string | null) => {
      if (distributionId === null) {
        setItems([]);
        return;
      }
      setLoading(true);
      setError(null);
      try {
        const page = await model.api.list_cloudfront_invalidations({ account_id: accountId, distribution_id: distributionId, marker });
        setItems((current) => (marker === null ? page.items : [...current, ...page.items]));
        setNext(page.next_marker);
      } catch (raw) {
        setError(toBbError(raw).message);
      } finally {
        setLoading(false);
      }
    },
    [model, accountId, distributionId],
  );

  useEffect(() => {
    queueMicrotask(() => void load(null));
  }, [load]);

  const show = async (id: string) => {
    if (distributionId === null) {
      return;
    }
    try {
      const one = await model.api.get_cloudfront_invalidation({ account_id: accountId, distribution_id: distributionId, invalidation_id: id });
      setPaths((current) => ({ ...current, [id]: one.paths }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  return (
    <Stack gap="xs">
      {error !== null && <Alert color="red">{error}</Alert>}
      <Group justify="space-between">
        <Text size="sm" c="dimmed">
          {tr("Newest first. An invalidation is InProgress until CloudFront has removed the paths from every edge, then Completed.")}
        </Text>
        <Button variant="default" size="compact-sm" loading={loading} onClick={() => void load(null)}>
          {tr("Refresh")}
        </Button>
      </Group>
      <Table aria-label={tr("Invalidation history")}>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>{tr("Id")}</Table.Th>
            <Table.Th>{tr("Created")}</Table.Th>
            <Table.Th>{tr("Status")}</Table.Th>
            <Table.Th>{tr("Paths")}</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {items.map((item) => (
            <Table.Tr key={item.id} aria-label={tr("Invalidation {id}", { id: item.id })}>
              <Table.Td>{item.id}</Table.Td>
              <Table.Td>{formatDateTime(item.create_time)}</Table.Td>
              <Table.Td>
                <StatusBadge status={item.status} />
              </Table.Td>
              <Table.Td>
                {paths[item.id] !== undefined ? (
                  paths[item.id]?.join(", ")
                ) : (
                  <Button size="compact-sm" variant="subtle" aria-label={tr("Show paths of {id}", { id: item.id })} onClick={() => void show(item.id)}>
                    {tr("Show paths")}
                  </Button>
                )}
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
      {items.length === 0 && !loading && <Text size="sm">{tr("No invalidations yet.")}</Text>}
      {next !== null && (
        <Button variant="default" loading={loading} onClick={() => void load(next)}>
          {tr("Load more")}
        </Button>
      )}
    </Stack>
  );
}
