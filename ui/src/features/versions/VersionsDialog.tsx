import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Checkbox,
  Group,
  Loader,
  NumberInput,
  SegmentedControl,
  Stack,
  Switch,
  Table,
  Text,
  TextInput,
  Tooltip,
} from "@mantine/core";
import {
  IconAlertCircle,
  IconArrowBackUp,
  IconDownload,
  IconEye,
  IconFileText,
  IconHistory,
  IconTrash,
  IconTrashX,
} from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { DeleteMarkerMode, ObjectVersion, VersionFilter, VersionGroup, VersionRef } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatBytes, formatCount, formatDateTime } from "../../lib/format";
import { s3Uri } from "../../lib/s3keys";
import { DEFAULT_FILTER, DEFAULT_FORM, toVersionFilter, versionFileName, versionId, type VersionFilterForm } from "./filters";

export interface VersionsDialogProps {
  accountId: string;
  bucket: string;
  prefix: string;
  /** Only the key `prefix` itself; otherwise every key that starts with it. */
  exact: boolean;
  onClose: () => void;
}

/** The first page of a listing, or the pages loaded so far, for the query with this `id`. */
interface Loaded {
  id: number;
  groups: VersionGroup[];
  token: string | null;
  scanned: number;
  error: string | null;
}

const EMPTY_SELECTION: ReadonlySet<string> = new Set();

function FilterPanel({
  form,
  onChange,
  error,
  onApply,
  onReset,
}: {
  form: VersionFilterForm;
  onChange: (form: VersionFilterForm) => void;
  error: string | null;
  onApply: () => void;
  onReset: () => void;
}) {
  return (
    <form
      aria-label={tr("Version filters")}
      onSubmit={(event) => {
        event.preventDefault();
        onApply();
      }}
    >
      <Stack gap="xs">
        <Group align="flex-end" gap="md" wrap="wrap">
          <Switch
            label={tr("Show versions")}
            checked={form.showVersions}
            onChange={(event) => onChange({ ...form, showVersions: event.currentTarget.checked })}
          />
          <Stack gap={2}>
            <Text size="xs" fw={500}>
              {tr("Delete markers")}
            </Text>
            <SegmentedControl<DeleteMarkerMode>
              size="xs"
              aria-label={tr("Delete markers")}
              value={form.deleteMarkers}
              onChange={(value) => onChange({ ...form, deleteMarkers: value })}
              data={[
                { value: "include", label: tr("Include") },
                { value: "exclude", label: tr("Hide") },
                { value: "only", label: tr("Only") },
              ]}
            />
          </Stack>
          <Switch
            label={tr("Only keys with several versions")}
            checked={form.multipleVersions}
            onChange={(event) => onChange({ ...form, multipleVersions: event.currentTarget.checked })}
          />
        </Group>
        <Group align="flex-end" gap="sm" wrap="wrap">
          <NumberInput
            label={tr("More than")}
            description={tr("versions")}
            w={110}
            min={0}
            allowDecimal={false}
            value={form.moreThan}
            onChange={(value) => onChange({ ...form, moreThan: typeof value === "number" ? value : "" })}
          />
          <NumberInput
            label={tr("Fewer than")}
            description={tr("versions")}
            w={110}
            min={0}
            allowDecimal={false}
            value={form.fewerThan}
            onChange={(value) => onChange({ ...form, fewerThan: typeof value === "number" ? value : "" })}
          />
          <TextInput
            type="date"
            label={tr("Modified from")}
            value={form.from}
            onChange={(event) => onChange({ ...form, from: event.currentTarget.value })}
          />
          <TextInput
            type="date"
            label={tr("Modified to")}
            value={form.to}
            onChange={(event) => onChange({ ...form, to: event.currentTarget.value })}
          />
          <Button type="submit" size="xs">
            {tr("Apply")}
          </Button>
          <Button type="button" size="xs" variant="default" onClick={onReset}>
            {tr("Reset")}
          </Button>
        </Group>
        {error !== null ? (
          <Text size="xs" c="red" role="alert">
            {error}
          </Text>
        ) : null}
      </Stack>
    </form>
  );
}

/** Lists the versions and delete markers under a prefix, grouped by key, with filters and actions. */
export function VersionsDialog({ accountId, bucket, prefix, exact, onClose }: VersionsDialogProps) {
  const model = useModel();
  const [form, setForm] = useState<VersionFilterForm>(DEFAULT_FORM);
  const [formError, setFormError] = useState<string | null>(null);
  const [query, setQuery] = useState<{ id: number; filter: VersionFilter }>({ id: 0, filter: DEFAULT_FILTER });
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [moreBusy, setMoreBusy] = useState(false);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [selection, setSelection] = useState<{ id: number; keys: ReadonlySet<string> }>({ id: 0, keys: EMPTY_SELECTION });
  const [working, setWorking] = useState(false);

  useEffect(() => {
    let active = true;
    model.api
      .list_versions({ account_id: accountId, bucket, prefix, exact, filter: query.filter, token: null })
      .then(
        (page) => {
          if (active) {
            setLoaded({ id: query.id, groups: page.groups, token: page.next_token, scanned: page.scanned, error: null });
          }
        },
        (raw: unknown) => {
          if (active) {
            setLoaded({ id: query.id, groups: [], token: null, scanned: 0, error: toBbError(raw).message });
          }
        },
      );
    return () => {
      active = false;
    };
  }, [model, accountId, bucket, prefix, exact, query]);

  const loading = loaded?.id !== query.id;
  const groups = loading ? [] : loaded.groups;
  const selected = selection.id === query.id ? selection.keys : EMPTY_SELECTION;
  const everyVersion = groups.flatMap((group) => group.versions);
  const allSelected = everyVersion.length > 0 && everyVersion.every((v) => selected.has(versionId(v.key, v.version_id)));

  const reload = () => setQuery((current) => ({ id: current.id + 1, filter: current.filter }));
  const apply = () => {
    const result = toVersionFilter(form);
    if (result.error !== null) {
      setFormError(result.error);
      return;
    }
    setFormError(null);
    setQuery((current) => ({ id: current.id + 1, filter: result.filter }));
  };
  const reset = () => {
    setForm(DEFAULT_FORM);
    setFormError(null);
    setQuery((current) => ({ id: current.id + 1, filter: DEFAULT_FILTER }));
  };
  const toggle = (key: string, version: string) => {
    const id = versionId(key, version);
    const next = new Set(selected);
    if (!next.delete(id)) {
      next.add(id);
    }
    setSelection({ id: query.id, keys: next });
  };
  const toggleAll = () =>
    setSelection({
      id: query.id,
      keys: allSelected ? EMPTY_SELECTION : new Set(everyVersion.map((v) => versionId(v.key, v.version_id))),
    });

  const loadMore = async () => {
    const token = loaded !== null && loaded.id === query.id ? loaded.token : null;
    if (token === null) {
      return;
    }
    const id = query.id;
    setMoreBusy(true);
    setMoreError(null);
    try {
      const page = await model.api.list_versions({
        account_id: accountId,
        bucket,
        prefix,
        exact,
        filter: query.filter,
        token,
      });
      setLoaded((current) =>
        current !== null && current.id === id
          ? {
              ...current,
              groups: [...current.groups, ...page.groups],
              token: page.next_token,
              scanned: current.scanned + page.scanned,
            }
          : current,
      );
    } catch (raw) {
      setMoreError(toBbError(raw).message);
    } finally {
      setMoreBusy(false);
    }
  };

  const work = async (what: string, run: () => Promise<void>) => {
    setWorking(true);
    try {
      await run();
    } catch (error) {
      model.status.error(error, { notify: tr("{what} failed", { what: tr(what) }) });
    } finally {
      setWorking(false);
    }
  };

  const restore = (version: ObjectVersion) =>
    work("Restore", async () => {
      await model.api.restore_version({
        account_id: accountId,
        bucket,
        key: version.key,
        version_id: version.version_id,
      });
      model.status.info(
        tr("Restored the version of {key} from {time}; it is now the current version.", {
          key: version.key,
          time: formatDateTime(version.last_modified),
        }),
      );
      reload();
      void model.browser.refresh();
    });

  const purge = async (versions: readonly ObjectVersion[]) => {
    const first = versions[0];
    if (first === undefined) {
      return;
    }
    const single = versions.length === 1;
    const what = single
      ? first.is_delete_marker
        ? tr("the delete marker {id} of \"{key}\"", { id: first.version_id, key: first.key })
        : tr("version {id} of \"{key}\" ({modified}, {size})", {
            id: first.version_id,
            key: first.key,
            modified: formatDateTime(first.last_modified),
            size: formatBytes(first.size),
          })
      : tr("{count} versions (delete markers included)", { count: formatCount(versions.length) });
    const confirmed = await model.dialogs.confirm({
      title: tr("Purge versions"),
      message: tr("Permanently delete {what} from {bucket}?\nPurged versions cannot be recovered.", { what, bucket }),
      confirmLabel: tr("Purge"),
      danger: true,
      typedConfirmation: "purge",
    });
    if (!confirmed) {
      return;
    }
    await work("Purge", async () => {
      const refs: VersionRef[] = versions.map((v) => ({ key: v.key, version_id: v.version_id }));
      const outcome = await model.api.purge_versions({ account_id: accountId, bucket, versions: refs });
      const failure = outcome.failures[0];
      if (failure !== undefined) {
        model.status.error(
          tr("{count} versions could not be purged: {reason}", {
            count: formatCount(outcome.failures.length),
            reason: `${failure.message}${failure.code === null ? "" : ` (${failure.code})`}`,
          }),
          { notify: tr("Purge incomplete") },
        );
      } else if (outcome.deleted === 1) {
        model.status.info(tr("Purged 1 version."));
      } else {
        model.status.info(tr("Purged {count} versions.", { count: formatCount(outcome.deleted) }));
      }
      reload();
      void model.browser.refresh();
    });
  };

  const download = (version: ObjectVersion) =>
    work("Download", async () => {
      const folder = model.settings.current.general.default_download_dir;
      const name = versionFileName(version.key, version.last_modified);
      const path = await model.platform.pickSavePath({
        title: tr("Save this version as"),
        defaultPath: folder === null ? name : `${folder.replace(/[\\/]+$/, "")}/${name}`,
        filters: [],
      });
      if (path === null) {
        return;
      }
      const tasks = await model.api.enqueue_download_version({
        account_id: accountId,
        bucket,
        key: version.key,
        version_id: version.version_id,
        dest_path: path,
      });
      model.transfers.addReturned(tasks);
      model.status.info(
        tr("Download of the version from {time}: added to the transfer queue.", {
          time: formatDateTime(version.last_modified),
        }),
      );
    });

  const preview = (version: ObjectVersion) =>
    model.dialogs.open({ kind: "preview", accountId, bucket, key: version.key, versionId: version.version_id });

  const entries = groups.reduce((sum, group) => sum + group.versions.length, 0);
  const selectedVersions = everyVersion.filter((v) => selected.has(versionId(v.key, v.version_id)));

  let body: React.ReactNode;
  if (loading) {
    body = (
      <Group justify="center" p="lg">
        <Loader size="sm" aria-label={tr("Loading versions")} />
      </Group>
    );
  } else if (loaded.error !== null) {
    body = (
      <Alert color="red" icon={<IconAlertCircle size={16} />} title={tr("The versions could not be listed")} role="alert">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {loaded.error}
        </Text>
        <Button mt="xs" size="xs" variant="default" onClick={reload}>
          {tr("Try again")}
        </Button>
      </Alert>
    );
  } else if (groups.length === 0) {
    body = (
      <Text size="sm" c="dimmed" ta="center" p="lg">
        {loaded.token === null
          ? tr("No versions match.")
          : tr("No key matched in the {count} entries checked so far.", { count: formatCount(loaded.scanned) })}
      </Text>
    );
  } else {
    body = (
      <div className="bb-versions-scroll">
        <Table verticalSpacing={4} fz="sm" aria-label={tr("Versions")}>
          <Table.Thead>
            <Table.Tr>
              <Table.Th w={32}>
                <Checkbox size="xs" aria-label={tr("Select all shown")} checked={allSelected} onChange={toggleAll} />
              </Table.Th>
              <Table.Th>{tr("Version")}</Table.Th>
              <Table.Th>{tr("Modified")}</Table.Th>
              <Table.Th ta="right">{tr("Size")}</Table.Th>
              <Table.Th w={150}>{tr("Actions")}</Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {groups.map((group) => (
              <GroupRows
                key={group.key}
                group={group}
                selected={selected}
                busy={working}
                onToggle={toggle}
                onRestore={(version) => void restore(version)}
                onPurge={(version) => void purge([version])}
                onDownload={(version) => void download(version)}
                onPreview={preview}
              />
            ))}
          </Table.Tbody>
        </Table>
      </div>
    );
  }

  return (
    <AppModal
      opened
      onClose={onClose}
      size="xl"
      title={
        <Group gap="xs">
          <IconHistory size={18} aria-hidden />
          <Text fw={600}>{tr("Versions")}</Text>
        </Group>
      }
    >
      <Stack gap="sm">
        <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere" }}>
          {exact ? tr("Versions of") : tr("Versions under")} {s3Uri(bucket, prefix)}
        </Text>
        <FilterPanel form={form} onChange={setForm} error={formError} onApply={apply} onReset={reset} />
        {body}
        {moreError !== null ? (
          <Text size="xs" c="red" role="alert">
            {moreError}
          </Text>
        ) : null}
        <Group justify="space-between">
          <Group gap="xs">
            <Text size="xs" c="dimmed" aria-live="polite">
              {loading
                ? ""
                : groups.length === 1
                  ? tr("1 key, {count} entries shown", { count: formatCount(entries) })
                  : tr("{keys} keys, {count} entries shown", {
                      keys: formatCount(groups.length),
                      count: formatCount(entries),
                    })}
            </Text>
            {!loading && loaded.token !== null ? (
              <Button size="compact-xs" variant="light" loading={moreBusy} onClick={() => void loadMore()}>
                {tr("Load more")}
              </Button>
            ) : null}
          </Group>
          <Group gap="xs">
            <Button
              color="red"
              variant="light"
              size="xs"
              leftSection={<IconTrash size={14} />}
              disabled={selectedVersions.length === 0 || working}
              onClick={() => void purge(selectedVersions)}
            >
              {tr("Purge selected ({count})...", { count: selectedVersions.length })}
            </Button>
            <Button variant="default" size="xs" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Group>
      </Stack>
    </AppModal>
  );
}

function GroupRows({
  group,
  selected,
  busy,
  onToggle,
  onRestore,
  onPurge,
  onDownload,
  onPreview,
}: {
  group: VersionGroup;
  selected: ReadonlySet<string>;
  busy: boolean;
  onToggle: (key: string, version: string) => void;
  onRestore: (version: ObjectVersion) => void;
  onPurge: (version: ObjectVersion) => void;
  onDownload: (version: ObjectVersion) => void;
  onPreview: (version: ObjectVersion) => void;
}) {
  return (
    <>
      <Table.Tr className="bb-version-group" aria-label={tr("Key {key}", { key: group.key })}>
        <Table.Td colSpan={5}>
          <Group gap="xs" wrap="nowrap">
            {group.is_deleted ? <IconTrashX size={16} aria-hidden /> : <IconFileText size={16} aria-hidden />}
            <Text ff="monospace" size="sm" fw={600} className="bb-ellipsis" title={group.key}>
              {group.key}
            </Text>
            {group.is_deleted ? (
              <Badge color="red" variant="light" size="sm">
                {tr("Deleted")}
              </Badge>
            ) : null}
            <Badge variant="light" size="sm">
              {group.matching === group.total
                ? group.total === 1
                  ? tr("1 version")
                  : tr("{count} versions", { count: formatCount(group.total) })
                : tr("{matching} of {count} versions", { matching: group.matching, count: formatCount(group.total) })}
            </Badge>
            {group.truncated ? (
              <Badge color="yellow" variant="light" size="sm">
                {tr("More not shown")}
              </Badge>
            ) : null}
          </Group>
        </Table.Td>
      </Table.Tr>
      {group.versions.map((version) => {
        const marker = version.is_delete_marker;
        return (
          <Table.Tr
            key={version.version_id}
            className="bb-version-row"
            data-marker={marker || undefined}
            aria-label={
              marker
                ? tr("Delete marker {id} of {key}", { id: version.version_id, key: group.key })
                : tr("Version {id} of {key}", { id: version.version_id, key: group.key })
            }
          >
            <Table.Td>
              <Checkbox
                size="xs"
                aria-label={marker ? tr("Select delete marker {id}", { id: version.version_id }) : tr("Select version {id}", { id: version.version_id })}
                checked={selected.has(versionId(version.key, version.version_id))}
                onChange={() => onToggle(version.key, version.version_id)}
              />
            </Table.Td>
            <Table.Td>
              <Group gap={6} wrap="nowrap">
                {marker ? <IconTrashX size={14} aria-hidden /> : <IconFileText size={14} aria-hidden />}
                <Text size="xs" ff="monospace" className="bb-ellipsis" title={version.version_id}>
                  {version.version_id}
                </Text>
                {marker ? (
                  <Badge color="red" size="xs" variant="filled">
                    {tr("Delete marker")}
                  </Badge>
                ) : null}
                {version.is_latest ? (
                  <Badge size="xs" variant="light">
                    {tr("Latest")}
                  </Badge>
                ) : null}
              </Group>
            </Table.Td>
            <Table.Td>{formatDateTime(version.last_modified)}</Table.Td>
            <Table.Td ta="right">{marker ? "" : formatBytes(version.size)}</Table.Td>
            <Table.Td>
              <Group gap={2} wrap="nowrap">
                {marker ? null : (
                  <>
                    <Tooltip label={tr("Preview")}>
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        aria-label={tr("Preview version {id}", { id: version.version_id })}
                        onClick={() => onPreview(version)}
                      >
                        <IconEye size={15} />
                      </ActionIcon>
                    </Tooltip>
                    <Tooltip label={tr("Download this version")}>
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        aria-label={tr("Download version {id}", { id: version.version_id })}
                        disabled={busy}
                        onClick={() => onDownload(version)}
                      >
                        <IconDownload size={15} />
                      </ActionIcon>
                    </Tooltip>
                    <Tooltip label={version.is_latest ? tr("This is already the current version") : tr("Restore as the current version")}>
                      <ActionIcon
                        size="sm"
                        variant="subtle"
                        aria-label={tr("Restore version {id}", { id: version.version_id })}
                        disabled={busy || version.is_latest}
                        onClick={() => onRestore(version)}
                      >
                        <IconArrowBackUp size={15} />
                      </ActionIcon>
                    </Tooltip>
                  </>
                )}
                <Tooltip label={marker ? tr("Remove this delete marker for good") : tr("Purge this version for good")}>
                  <ActionIcon
                    size="sm"
                    variant="subtle"
                    color="red"
                    aria-label={
                      marker
                        ? tr("Remove delete marker {id}", { id: version.version_id })
                        : tr("Purge version {id}", { id: version.version_id })
                    }
                    disabled={busy}
                    onClick={() => onPurge(version)}
                  >
                    <IconTrash size={15} />
                  </ActionIcon>
                </Tooltip>
              </Group>
            </Table.Td>
          </Table.Tr>
        );
      })}
    </>
  );
}
