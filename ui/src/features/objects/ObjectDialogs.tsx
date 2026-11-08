import { Group, NumberInput, SegmentedControl, Select, Stack, Text, TextInput } from "@mantine/core";
import { IconLink } from "@tabler/icons-react";
import { useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { ObjectEntry, PresignMethod } from "../../api/types";
import { useModel } from "../../app/context";
import { CopyField } from "../../components/CopyField";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { formatCount, formatDateTime } from "../../lib/format";
import { type ExpiryUnit, MAX_PRESIGN_SECONDS, UNIT_SECONDS } from "../sharing/expiry";
import { ShortenControl } from "../sharing/ShortenControl";
import { baseName, childKey, displaySegment, isFolderKey, parentPrefix, s3Uri, validateNewName } from "../../lib/s3keys";

function loadedKeys(entries: readonly ObjectEntry[]): ReadonlySet<string> {
  return new Set(entries.map((entry) => entry.key));
}

/** Creates a folder marker under the open prefix. */
export function NewFolderDialog({
  accountId,
  bucket,
  prefix,
  onClose,
}: {
  accountId: string;
  bucket: string;
  prefix: string;
  onClose: () => void;
}) {
  const model = useModel();
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const existing = loadedKeys(model.browser.state.listing.entries);
  const nameError = touched || name.length > 0 ? validateNewName(name, { prefix, folder: true, existingKeys: existing }) : null;
  const submit = async () => {
    setTouched(true);
    if (validateNewName(name, { prefix, folder: true, existingKeys: existing }) !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const key = childKey(prefix, name, true);
      await model.api.create_folder({ account_id: accountId, bucket, key });
      model.status.info(tr("Folder {key} created.", { key }));
      onClose();
      const listing = model.browser.state.listing;
      if (listing.bucket === bucket && listing.prefix === prefix) {
        await model.browser.refresh();
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={tr("New folder")}
      okLabel={tr("Create folder")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
    >
      <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere" }}>
        {tr("In {uri}", { uri: s3Uri(bucket, prefix) })}
      </Text>
      <TextInput
        label={tr("Folder name")}
        value={name}
        onChange={(event) => setName(event.currentTarget.value)}
        error={nameError}
        autoComplete="off"
        spellCheck={false}
        data-autofocus
      />
    </OkCancelDialog>
  );
}

/** Renames an object or folder (the backend queues move tasks). */
export function RenameDialog({
  accountId,
  bucket,
  objectKey,
  onClose,
}: {
  accountId: string;
  bucket: string;
  objectKey: string;
  onClose: () => void;
}) {
  const model = useModel();
  const folder = isFolderKey(objectKey);
  const parent = parentPrefix(objectKey);
  const original = baseName(objectKey);
  const [name, setName] = useState(original);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const existing = loadedKeys(model.browser.state.listing.entries);
  const unchanged = name === original;
  const nameError = unchanged ? null : validateNewName(name, { prefix: parent, folder, existingKeys: existing });
  const submit = async () => {
    if (unchanged || nameError !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const tasks = await model.api.rename({
        account_id: accountId,
        bucket,
        from_key: objectKey,
        to_key: childKey(parent, name, folder),
      });
      model.transfers.addReturned(tasks);
      model.status.info(
        tasks.length === 1
          ? tr("Renaming {from} to {to}: 1 task queued.", { from: displaySegment(original), to: name })
          : tr("Renaming {from} to {to}: {n} tasks queued.", { from: displaySegment(original), to: name, n: formatCount(tasks.length) }),
      );
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={folder ? tr("Rename folder") : tr("Rename object")}
      okLabel={tr("Rename")}
      okDisabled={unchanged || nameError !== null}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
    >
      {folder ? (
        <Text size="sm" c="dimmed">
          {tr("Every object in the folder is moved to the new name.")}
        </Text>
      ) : null}
      <TextInput
        label={tr("New name")}
        value={name}
        onChange={(event) => setName(event.currentTarget.value)}
        error={nameError}
        autoComplete="off"
        spellCheck={false}
        data-autofocus
        onFocus={(event) => event.currentTarget.select()}
      />
    </OkCancelDialog>
  );
}

/** Presigned link for one object: method, expiry (1 second to 7 days), copy. */
export function ShareLinkDialog({
  accountId,
  bucket,
  objectKey,
  onClose,
}: {
  accountId: string;
  bucket: string;
  objectKey: string;
  onClose: () => void;
}) {
  const model = useModel();
  const [method, setMethod] = useState<PresignMethod>("get");
  const [amount, setAmount] = useState<number>(1);
  const [unit, setUnit] = useState<ExpiryUnit>("hours");
  // The link keeps the method and expiry it was created with, so its caption can never disagree with it.
  const [link, setLink] = useState<{ url: string; method: PresignMethod; expiresAt: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);
  const seconds = Number.isInteger(amount) ? amount * UNIT_SECONDS[unit] : Number.NaN;
  const expiryError =
    !Number.isInteger(amount) || seconds < 1 || seconds > MAX_PRESIGN_SECONDS
      ? tr("Choose an expiry between 1 second and 7 days.")
      : null;
  const create = async () => {
    if (expiryError !== null) {
      return;
    }
    const id = ++request.current;
    const requested = { method, seconds };
    setBusy(true);
    setError(null);
    setLink(null);
    try {
      const url = await model.api.presign({
        account_id: accountId,
        bucket,
        key: objectKey,
        method: requested.method,
        expires_secs: requested.seconds,
      });
      if (id === request.current) {
        setLink({
          url,
          method: requested.method,
          expiresAt: formatDateTime(new Date(Date.now() + requested.seconds * 1000).toISOString()),
        });
      }
    } catch (raw) {
      if (id === request.current) {
        setError(toBbError(raw).message);
      }
    } finally {
      if (id === request.current) {
        setBusy(false);
      }
    }
  };
  /** Any change of method or expiry invalidates the shown link and any pending request. */
  const reset = () => {
    request.current += 1;
    setLink(null);
    setBusy(false);
  };
  return (
    <OkCancelDialog
      opened
      title={tr("Share link")}
      okLabel={tr("Create link")}
      onOk={() => void create()}
      onCancel={onClose}
      cancelLabel={tr("Close")}
      okDisabled={expiryError !== null}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        {tr("A presigned link gives anyone who has it temporary access to")}{" "}
        <strong>{s3Uri(bucket, objectKey)}</strong>
        {tr(".")}
      </Text>
      <SegmentedControl<PresignMethod>
        value={method}
        disabled={busy}
        onChange={(value) => {
          setMethod(value);
          reset();
        }}
        data={[
          { value: "get", label: tr("Download (GET)") },
          { value: "put", label: tr("Upload (PUT)") },
        ]}
        aria-label={tr("Link type")}
      />
      <Group align="flex-start" gap="sm">
        <NumberInput
          label={tr("Valid for")}
          w={120}
          min={1}
          allowDecimal={false}
          disabled={busy}
          value={Number.isNaN(amount) ? "" : amount}
          onChange={(value) => {
            setAmount(typeof value === "number" ? value : Number.NaN);
            reset();
          }}
          error={expiryError}
        />
        <Select
          label={tr("Unit")}
          w={130}
          value={unit}
          allowDeselect={false}
          disabled={busy}
          data={[
            { value: "seconds", label: tr("Seconds") },
            { value: "minutes", label: tr("Minutes") },
            { value: "hours", label: tr("Hours") },
            { value: "days", label: tr("Days") },
          ]}
          onChange={(value) => {
            if (value === "seconds" || value === "minutes" || value === "hours" || value === "days") {
              setUnit(value);
              reset();
            }
          }}
        />
      </Group>
      {link !== null ? (
        <Stack gap={6}>
          <CopyField label={tr("Link")} value={link.url} multiline sensitive />
          <Group gap={4}>
            <IconLink size={12} aria-hidden />
            <Text size="xs" c="dimmed">
              {tr("Expires around {time}.", { time: link.expiresAt })}{" "}
              {link.method === "get"
                ? tr("Anyone with the link can download the object until then.")
                : tr("Upload with an HTTP PUT request to this link until then.")}
            </Text>
          </Group>
          <ShortenControl url={link.url} />
        </Stack>
      ) : null}
    </OkCancelDialog>
  );
}
