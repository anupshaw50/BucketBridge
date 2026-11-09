import { Alert, Checkbox, Stack, Switch, Text, TextInput } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useState } from "react";
import type { HeaderChanges, ObjectMeta, TagEdit } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import {
  headerChange,
  metaPatch,
  recordToRows,
  type Row,
  rowsToRecord,
  validateHeaderValue,
  validateMediaType,
  validateMetaRows,
  validateTagRows,
} from "../../lib/objectMeta";
import { splitNames, sseKeyArg, sseKeyProblem, type TabContext, useApply } from "./apply";
import { ApplyBar, CurrentKeyField, KeyValueEditor } from "./parts";

const STANDARD_HEADERS = [
  { id: "content_type", label: "Content-Type", placeholder: "for example text/plain; charset=utf-8" },
  { id: "cache_control", label: "Cache-Control", placeholder: "for example public, max-age=3600" },
  { id: "content_disposition", label: "Content-Disposition", placeholder: 'for example attachment; filename="a.pdf"' },
  { id: "content_encoding", label: "Content-Encoding", placeholder: "for example gzip" },
] as const;

type HeaderId = (typeof STANDARD_HEADERS)[number]["id"];

function initialHeaders(meta: ObjectMeta | null): Record<HeaderId, string> {
  return {
    content_type: meta?.content_type ?? "",
    cache_control: meta?.cache_control ?? "",
    content_disposition: meta?.content_disposition ?? "",
    content_encoding: meta?.content_encoding ?? "",
  };
}

const NO_USER_META = { user_meta_set: {}, user_meta_remove: [] };

/** Content-Type, Cache-Control, Content-Disposition and Content-Encoding. */
export function HeadersTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const initial = initialHeaders(ctx.details?.meta ?? null);
  const [values, setValues] = useState(initial);
  const [removes, setRemoves] = useState<Record<HeaderId, boolean>>({
    content_type: false,
    cache_control: false,
    content_disposition: false,
    content_encoding: false,
  });
  const problem = (id: HeaderId, label: string): string | null => {
    const value = values[id];
    if (value.length === 0 || removes[id]) {
      return null;
    }
    if (ctx.batch || value !== initial[id]) {
      return validateHeaderValue(label, value) ?? (id === "content_type" ? validateMediaType(value) : null);
    }
    return null;
  };
  const changes: HeaderChanges = {
    content_type: headerChange(initial.content_type, values.content_type, removes.content_type, ctx.batch),
    cache_control: headerChange(initial.cache_control, values.cache_control, removes.cache_control, ctx.batch),
    content_disposition: headerChange(initial.content_disposition, values.content_disposition, removes.content_disposition, ctx.batch),
    content_encoding: headerChange(initial.content_encoding, values.content_encoding, removes.content_encoding, ctx.batch),
    ...NO_USER_META,
  };
  const nothing = STANDARD_HEADERS.every((header) => changes[header.id].action === "keep");
  const invalid = STANDARD_HEADERS.some((header) => problem(header.id, header.label) !== null) || sseKeyProblem(ctx) !== null;
  const apply = () =>
    void applier.run(() =>
      model.api.update_object_headers({
        account_id: ctx.accountId,
        bucket: ctx.bucket,
        keys: ctx.keys,
        changes,
        sse_c_key: sseKeyArg(ctx),
      }),
    );
  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {ctx.batch
          ? tr("Fill in the headers to set on every selected object, or tick Remove. Empty fields leave each object's header as it is.")
          : tr("Clear a field to remove the header. S3 then falls back to its default (binary/octet-stream for Content-Type).")}
      </Text>
      {STANDARD_HEADERS.map((header) => (
        <Stack key={header.id} gap={2}>
          <TextInput
            label={header.label}
            placeholder={ctx.batch ? tr("Leave empty to keep") : tr(header.placeholder)}
            value={values[header.id]}
            disabled={removes[header.id]}
            onChange={(event) => setValues({ ...values, [header.id]: event.currentTarget.value })}
            error={problem(header.id, header.label)}
            autoComplete="off"
            spellCheck={false}
          />
          {ctx.batch ? (
            <Checkbox
              size="xs"
              label={tr("Remove {label}", { label: tr(header.label) })}
              checked={removes[header.id]}
              onChange={(event) => setRemoves({ ...removes, [header.id]: event.currentTarget.checked })}
            />
          ) : null}
        </Stack>
      ))}
      <CurrentKeyField ctx={ctx} />
      <ApplyBar applier={applier} batch={ctx.batch} disabled={nothing || invalid} onApply={apply} />
    </Stack>
  );
}

/** User metadata (`x-amz-meta-*`). */
export function UserMetaTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const initial = ctx.details?.meta.user_meta ?? {};
  const [rows, setRows] = useState<Row[]>(() => (ctx.batch ? [] : recordToRows(initial)));
  const [removeText, setRemoveText] = useState("");
  const problem = validateMetaRows(rows);
  const patch = ctx.batch ? { set: rowsToRecord(rows, true), remove: splitNames(removeText) } : metaPatch(initial, rows);
  const nothing = Object.keys(patch.set).length === 0 && patch.remove.length === 0;
  const apply = () =>
    void applier.run(() =>
      model.api.update_object_headers({
        account_id: ctx.accountId,
        bucket: ctx.bucket,
        keys: ctx.keys,
        changes: {
          content_type: { action: "keep" },
          cache_control: { action: "keep" },
          content_disposition: { action: "keep" },
          content_encoding: { action: "keep" },
          user_meta_set: patch.set,
          user_meta_remove: patch.remove,
        },
        sse_c_key: sseKeyArg(ctx),
      }),
    );
  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {tr(
          "Custom metadata is stored with the object as x-amz-meta-* headers. Keys are not case sensitive; all keys and values together can take 2,048 bytes.",
        )}
      </Text>
      <KeyValueEditor rows={rows} onChange={setRows} keyLabel={tr("Metadata key")} valueLabel={tr("Metadata value")} addLabel={tr("Add metadata")} />
      {ctx.batch ? (
        <TextInput
          label={tr("Remove these keys")}
          description={tr("Separate keys with commas.")}
          value={removeText}
          onChange={(event) => setRemoveText(event.currentTarget.value)}
          autoComplete="off"
          spellCheck={false}
        />
      ) : null}
      {problem !== null ? (
        <Text size="sm" c="red" role="alert">
          {problem}
        </Text>
      ) : null}
      <CurrentKeyField ctx={ctx} />
      <ApplyBar applier={applier} batch={ctx.batch} disabled={nothing || problem !== null || sseKeyProblem(ctx) !== null} onApply={apply} />
    </Stack>
  );
}

/** Object tags. */
export function TagsTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const initial = ctx.details?.meta.tags ?? {};
  const [rows, setRows] = useState<Row[]>(() => (ctx.batch ? [] : recordToRows(initial)));
  const [removeText, setRemoveText] = useState("");
  const [replaceAll, setReplaceAll] = useState(false);
  const tagsError = ctx.details?.tags_error ?? null;
  if (tagsError !== null) {
    return (
      <Alert color="yellow" icon={<IconAlertCircle size={16} />} title={tr("The tags could not be read")} role="alert">
        {tagsError.message}
      </Alert>
    );
  }
  const problem = validateTagRows(rows);
  const wanted = rowsToRecord(rows);
  let edit: TagEdit;
  let nothing: boolean;
  if (!ctx.batch) {
    edit = { mode: "replace", tags: wanted };
    nothing = JSON.stringify(Object.entries(wanted).sort()) === JSON.stringify(Object.entries(initial).sort());
  } else if (replaceAll) {
    edit = { mode: "replace", tags: wanted };
    nothing = false;
  } else {
    const remove = splitNames(removeText);
    edit = { mode: "merge", set: wanted, remove };
    nothing = rows.length === 0 && remove.length === 0;
  }
  const apply = () =>
    void applier.run(() =>
      model.api.update_object_tags({ account_id: ctx.accountId, bucket: ctx.bucket, keys: ctx.keys, edit }),
    );
  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {ctx.batch
          ? tr("Tags below are added to every selected object (an existing tag with the same key gets the new value). Other tags stay.")
          : tr("An object can have up to 10 tags. Keys and values may contain letters, digits, spaces and + - = . _ : / @.")}
      </Text>
      <KeyValueEditor rows={rows} onChange={setRows} keyLabel={tr("Tag key")} valueLabel={tr("Tag value")} addLabel={tr("Add tag")} />
      {ctx.batch ? (
        <>
          <TextInput
            label={tr("Remove these tags")}
            description={tr("Tag keys, separated by commas.")}
            value={removeText}
            disabled={replaceAll}
            onChange={(event) => setRemoveText(event.currentTarget.value)}
            autoComplete="off"
            spellCheck={false}
          />
          <Switch
            label={tr("Replace all existing tags with the ones above")}
            checked={replaceAll}
            onChange={(event) => setReplaceAll(event.currentTarget.checked)}
          />
        </>
      ) : null}
      {problem !== null ? (
        <Text size="sm" c="red" role="alert">
          {problem}
        </Text>
      ) : null}
      <ApplyBar applier={applier} batch={ctx.batch} disabled={nothing || problem !== null} onApply={apply} />
    </Stack>
  );
}
