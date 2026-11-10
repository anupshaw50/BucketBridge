import { Alert, Badge, Button, Group, MultiSelect, NumberInput, Select, Stack, Table, Text, TextInput } from "@mantine/core";
import { IconPlus } from "@tabler/icons-react";
import { useEffect, useMemo, useState } from "react";
import { toBbError } from "../../api/errors";
import type { SavedFilter } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { compileFilter, compileGlob, describeFilter, matchesObject } from "../../lib/filters";
import { formatCount } from "../../lib/format";
import { SIZE_UNITS, type SizeUnit } from "../../lib/tools";
import { draftFromSaved, emptyDraft, type FilterDraft, specFromDraft, STORAGE_CLASS_OPTIONS } from "./filterDraft";

const UNIT_OPTIONS = SIZE_UNITS.map((entry) => entry.unit);

function SizeField({
  label,
  value,
  unit,
  onChange,
}: {
  label: string;
  value: number | string;
  unit: SizeUnit;
  onChange: (value: number | string, unit: SizeUnit) => void;
}) {
  return (
    <Group align="flex-end" gap={6} wrap="nowrap" style={{ flex: 1 }}>
      <NumberInput
        label={label}
        value={value}
        onChange={(next) => onChange(next, unit)}
        min={0}
        allowNegative={false}
        style={{ flex: 1 }}
      />
      <Select
        aria-label={tr("{label} unit", { label })}
        data={UNIT_OPTIONS}
        value={unit}
        allowDeselect={false}
        onChange={(next) => onChange(value, next ?? "B")}
        w={90}
        comboboxProps={{ withinPortal: false }}
      />
    </Group>
  );
}

function FilterForm({
  replaces,
  initial,
  onDone,
}: {
  /** The filter being edited (renames are allowed), or `null` for a new one. */
  replaces: string | null;
  initial: FilterDraft;
  onDone: () => void;
}) {
  const model = useModel();
  const listing = useSlice(model.browser.store, (state) => state.listing);
  const [draft, setDraft] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const patch = (change: Partial<FilterDraft>) => {
    setDraft((current) => ({ ...current, ...change }));
    setError(null);
  };
  const spec = useMemo(() => specFromDraft(draft), [draft]);
  const compiled = useMemo(() => compileFilter(spec), [spec]);
  const problem = compiled.ok ? null : compiled.message;
  // A glob that does not parse is reported at its field, any other problem (sizes, dates, classes) below the form.
  const globResult = draft.glob.length > 0 ? compileGlob(draft.glob) : null;
  const globProblem = globResult !== null && !globResult.ok ? globResult.message : null;
  const otherProblem = globProblem === null ? problem : null;
  const name = draft.name.trim();
  // How many of the entries loaded in the object list the filter keeps.
  const preview = useMemo(() => {
    if (!compiled.ok || listing.bucket === null || listing.entries.length === 0) {
      return null;
    }
    const kept = listing.entries.filter((entry) => matchesObject(compiled.filter, entry, listing.prefix)).length;
    return tr("Matches {kept} of {total} items loaded in s3://{bucket}/{prefix}.", {
      kept: formatCount(kept),
      total: formatCount(listing.entries.length),
      bucket: listing.bucket,
      prefix: listing.prefix,
    });
  }, [compiled, listing]);
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const classOptions = useMemo(
    () => [
      ...new Set([
        ...STORAGE_CLASS_OPTIONS,
        ...accounts.flatMap((account) => account.options.compat.storage_classes),
        ...draft.classes,
      ]),
    ],
    [accounts, draft.classes],
  );

  const save = async () => {
    if (name.length === 0 || problem !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await model.filters.save({ name, filter: spec }, replaces);
      model.status.info(tr("Filter \"{name}\" saved.", { name }));
      onDone();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
      noValidate
    >
      <Stack gap="sm">
        <Text size="sm" fw={600}>
          {replaces === null ? tr("New filter") : tr("Edit filter {name}", { name: replaces })}
        </Text>
        <TextInput
          label={tr("Filter name")}
          value={draft.name}
          onChange={(event) => patch({ name: event.currentTarget.value })}
          maxLength={100}
          required
          autoComplete="off"
          data-autofocus
        />
        <Group align="flex-start" gap="sm" grow>
          <TextInput
            label={tr("Prefix")}
            description={tr("The path below the folder you browse starts with this text.")}
            value={draft.prefix}
            onChange={(event) => patch({ prefix: event.currentTarget.value })}
            autoComplete="off"
            spellCheck={false}
          />
          <TextInput
            label={tr("Glob pattern")}
            description={tr("For example **/*.jpg. * stays within a folder, ** crosses folders.")}
            value={draft.glob}
            onChange={(event) => patch({ glob: event.currentTarget.value })}
            error={globProblem}
            autoComplete="off"
            spellCheck={false}
          />
        </Group>
        <Group align="flex-start" gap="sm" grow>
          <SizeField
            label={tr("Minimum size")}
            value={draft.minSize}
            unit={draft.minUnit}
            onChange={(minSize, minUnit) => patch({ minSize, minUnit })}
          />
          <SizeField
            label={tr("Maximum size")}
            value={draft.maxSize}
            unit={draft.maxUnit}
            onChange={(maxSize, maxUnit) => patch({ maxSize, maxUnit })}
          />
        </Group>
        <Group align="flex-start" gap="sm" grow>
          <TextInput
            type="date"
            label={tr("Modified on or after")}
            value={draft.after}
            onChange={(event) => patch({ after: event.currentTarget.value })}
          />
          <TextInput
            type="date"
            label={tr("Modified before")}
            value={draft.before}
            onChange={(event) => patch({ before: event.currentTarget.value })}
          />
        </Group>
        <MultiSelect
          label={tr("Storage classes")}
          placeholder={draft.classes.length === 0 ? tr("Any storage class") : undefined}
          data={classOptions}
          value={draft.classes}
          onChange={(classes) => patch({ classes })}
          comboboxProps={{ withinPortal: false }}
        />
        {otherProblem !== null ? (
          <Alert color="red" role="alert" py="xs">
            {otherProblem}
          </Alert>
        ) : null}
        {error !== null ? (
          <Alert color="red" role="alert" py="xs">
            {error}
          </Alert>
        ) : null}
        {preview !== null ? (
          <Text size="xs" c="dimmed" aria-live="polite">
            {preview}
          </Text>
        ) : null}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onDone}>
            {tr("Cancel")}
          </Button>
          <Button type="submit" loading={busy} disabled={name.length === 0 || problem !== null}>
            {tr("Save filter")}
          </Button>
        </Group>
      </Stack>
    </form>
  );
}

/** Creates, edits and deletes the saved filters, and applies one to the object list. */
export function FiltersDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const saved = useSlice(model.filters.store, (state) => state.saved);
  const loadError = useSlice(model.filters.store, (state) => state.error);
  const active = useSlice(model.browser.store, (state) => state.filter);
  const [editing, setEditing] = useState<{ replaces: string | null; initial: FilterDraft } | null>(null);
  useEffect(() => {
    void model.filters.reload();
  }, [model]);

  const remove = async (filter: SavedFilter) => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Delete filter"),
      message: tr("Delete the saved filter \"{name}\"?", { name: filter.name }),
      confirmLabel: tr("Delete filter"),
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    try {
      await model.filters.remove(filter.name);
      model.status.info(tr("Filter \"{name}\" deleted.", { name: filter.name }));
    } catch (error) {
      model.status.error(error, { notify: tr("The filter could not be deleted") });
    }
  };

  return (
    <AppModal opened onClose={onClose} title={tr("Saved filters")} size="xl" closeOnClickOutside={false}>
      {editing !== null ? (
        <FilterForm replaces={editing.replaces} initial={editing.initial} onDone={() => setEditing(null)} />
      ) : (
        <Stack gap="sm">
          <Text size="sm">
            {tr(
              "A saved filter narrows the object list to the items you have loaded that match it: a prefix or glob pattern, a size range, a modification date range and storage classes. Pick one from the Filter menu of the toolbar.",
            )}
          </Text>
          {loadError !== null ? (
            <Alert color="red" role="alert" py="xs">
              {loadError}
            </Alert>
          ) : null}
          {saved.length === 0 ? (
            <Text size="sm" c="dimmed">
              {tr("No saved filters yet.")}
            </Text>
          ) : (
            <Table aria-label={tr("Stored filters")} verticalSpacing={6} fz="sm" highlightOnHover>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>{tr("Name")}</Table.Th>
                  <Table.Th>{tr("What it matches")}</Table.Th>
                  <Table.Th>
                    <span className="bb-visually-hidden">{tr("Actions")}</span>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {saved.map((filter) => (
                  <Table.Tr key={filter.name}>
                    <Table.Td>
                      <Group gap={6} wrap="nowrap">
                        <Text size="sm" fw={500} style={{ overflowWrap: "anywhere" }}>
                          {filter.name}
                        </Text>
                        {active?.name.toLowerCase() === filter.name.toLowerCase() ? (
                          <Badge size="xs" variant="light">
                            {tr("Applied")}
                          </Badge>
                        ) : null}
                      </Group>
                    </Table.Td>
                    <Table.Td style={{ overflowWrap: "anywhere" }}>{describeFilter(filter.filter)}</Table.Td>
                    <Table.Td>
                      <Group gap={4} justify="flex-end" wrap="nowrap">
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          aria-label={tr("Apply filter {name}", { name: filter.name })}
                          onClick={() => model.filters.apply(filter.name)}
                        >
                          {tr("Apply")}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          aria-label={tr("Edit filter {name}", { name: filter.name })}
                          onClick={() => setEditing({ replaces: filter.name, initial: draftFromSaved(filter) })}
                        >
                          {tr("Edit")}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          color="red"
                          aria-label={tr("Delete filter {name}", { name: filter.name })}
                          onClick={() => void remove(filter)}
                        >
                          {tr("Delete")}
                        </Button>
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          )}
          <Group justify="space-between">
            <Button leftSection={<IconPlus size={14} />} onClick={() => setEditing({ replaces: null, initial: emptyDraft() })}>
              {tr("New filter")}
            </Button>
            <Button variant="default" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
