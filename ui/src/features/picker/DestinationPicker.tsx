import { Anchor, Button, Group, Select, Stack, Text, TextInput, UnstyledButton } from "@mantine/core";
import { IconBucket, IconFolder, IconFolderPlus } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { useSlice } from "../../app/context";
import { toBbError } from "../../api/errors";
import type { Bucket, IfExists } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { isFolderEntry } from "../../lib/objects";
import { breadcrumbs, childKey, displaySegment, relativeName, s3Uri, validateNewName } from "../../lib/s3keys";
import { transferKeys } from "../objects/actions";
import { destinationProblem, type TransferMode } from "./problem";

const MAX_PAGES = 50;

interface Listing {
  /** Which place the rows belong to; rows of another place are stale. */
  at: string;
  buckets: Bucket[];
  folders: string[];
  error: string | null;
}

/** "settings" keeps the overwrite policy of the settings. */
type PolicyChoice = IfExists | "settings";

const POLICY_OPTIONS: { value: PolicyChoice; label: string }[] = [
  { value: "settings", label: tr("As set in Settings") },
  { value: "overwrite", label: tr("Overwrite") },
  { value: "ask", label: tr("Ask me") },
  { value: "skip", label: tr("Skip") },
  { value: "overwrite_if_newer", label: tr("Overwrite if newer") },
  { value: "rename", label: tr("Rename (name (1).ext)") },
];

function isPolicyChoice(value: string | null): value is PolicyChoice {
  return POLICY_OPTIONS.some((item) => item.value === value);
}

/**
 * Reusable destination picker: browse the buckets of an account and their
 * folders, create a folder, and choose where a copy or move goes. The
 * destination account starts as the source's and can be another one: the
 * objects are then copied by the service when it can, and streamed through
 * this computer otherwise.
 */
export function DestinationPicker({
  mode,
  accountId,
  sourceBucket,
  keys,
  onClose,
}: {
  mode: TransferMode;
  accountId: string;
  sourceBucket: string;
  keys: string[];
  onClose: () => void;
}) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const [dstAccountId, setDstAccountId] = useState(accountId);
  const [policy, setPolicy] = useState<PolicyChoice>("settings");
  const account = accounts.find((item) => item.id === dstAccountId);
  const sameAccount = dstAccountId === accountId;
  const [bucket, setBucket] = useState<string | null>(sourceBucket);
  const [prefix, setPrefix] = useState("");
  const [listing, setListing] = useState<Listing | null>(null);
  const reload = 0;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const at = `${dstAccountId}\n${bucket ?? ""}\n${prefix}\n${reload}`;

  useEffect(() => {
    let current = true;
    const load = async () => {
      try {
        if (bucket === null) {
          const buckets = await model.api.list_buckets({ account_id: dstAccountId });
          if (current) {
            setListing({ at, buckets, folders: [], error: null });
          }
          return;
        }
        const folders: string[] = [];
        let token: string | null = null;
        for (let page = 0; page < MAX_PAGES; page += 1) {
          const result = await model.api.list_objects({
            account_id: dstAccountId,
            bucket,
            prefix,
            flat: false,
            token,
          });
          folders.push(...result.entries.filter(isFolderEntry).map((entry) => entry.key));
          token = result.next_token;
          if (token === null) {
            break;
          }
        }
        if (current) {
          setListing({ at, buckets: [], folders, error: null });
        }
      } catch (raw) {
        if (current) {
          setListing({ at, buckets: [], folders: [], error: toBbError(raw).message });
        }
      }
    };
    void load();
    return () => {
      current = false;
    };
  }, [model, dstAccountId, bucket, prefix, at]);

  const ready = listing !== null && listing.at === at;
  const problem = destinationProblem(mode, sourceBucket, keys, bucket, prefix, sameAccount);
  const go = (nextBucket: string | null, nextPrefix: string) => {
    setBucket(nextBucket);
    setPrefix(nextPrefix);
    setCreating(false);
    setName("");
    setNameError(null);
    setError(null);
  };

  const create = async () => {
    if (bucket === null) {
      return;
    }
    const invalid = validateNewName(name, {
      prefix,
      folder: true,
      existingKeys: new Set(listing?.folders ?? []),
    });
    if (invalid !== null) {
      setNameError(invalid);
      return;
    }
    try {
      const key = childKey(prefix, name, true);
      await model.api.create_folder({ account_id: dstAccountId, bucket, key });
      model.status.info(tr("Folder {key} created.", { key }));
      go(bucket, key);
    } catch (raw) {
      setNameError(toBbError(raw).message);
    }
  };

  const submit = async () => {
    if (problem !== null || bucket === null) {
      return;
    }
    setBusy(true);
    const ok = await transferKeys(model, mode, {
      account_id: accountId,
      src_bucket: sourceBucket,
      keys,
      dst_bucket: bucket,
      dst_prefix: prefix,
      dst_account_id: sameAccount ? null : dstAccountId,
      if_exists: policy === "settings" ? null : policy,
    });
    setBusy(false);
    if (ok) {
      onClose();
    } else {
      setError(mode === "copy" ? tr("The copy could not be started.") : tr("The move could not be started."));
    }
  };

  const crumbs = breadcrumbs(prefix);
  return (
    <OkCancelDialog
      opened
      title={
        mode === "copy"
          ? keys.length === 1
            ? tr("Copy 1 item to")
            : tr("Copy {n} items to", { n: keys.length })
          : keys.length === 1
            ? tr("Move 1 item to")
            : tr("Move {n} items to", { n: keys.length })
      }
      okLabel={mode === "copy" ? tr("Copy here") : tr("Move here")}
      okDisabled={problem !== null}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
      extraActions={
        bucket === null ? null : (
          <Button variant="subtle" leftSection={<IconFolderPlus size={16} />} onClick={() => setCreating(true)}>
            {tr("New folder")}
          </Button>
        )
      }
    >
      {accounts.length > 1 ? (
        <Select
          label={tr("Destination account")}
          data={accounts.map((item) => ({ value: item.id, label: item.label }))}
          value={dstAccountId}
          allowDeselect={false}
          onChange={(value) => {
            if (value !== null) {
              setDstAccountId(value);
              setBucket(null);
              setPrefix("");
              setCreating(false);
              setError(null);
            }
          }}
          comboboxProps={{ withinPortal: false }}
        />
      ) : (
        <Text size="sm" c="dimmed">
          {tr("Account: {label}", { label: account?.label ?? dstAccountId })}
        </Text>
      )}
      {sameAccount ? null : (
        <Text size="xs" c="dimmed">
          {tr("Between accounts the service copies the objects when it can; otherwise they go through this computer.")}
          {mode === "move" ? tr(" Each source is deleted only after its copy was checked.") : ""}
        </Text>
      )}
      <Group gap={4} wrap="wrap" aria-label={tr("Destination path")}>
        <Anchor component="button" type="button" size="sm" onClick={() => go(null, "")}>
          {tr("Buckets")}
        </Anchor>
        {bucket === null ? null : (
          <>
            <Text size="sm">/</Text>
            <Anchor component="button" type="button" size="sm" onClick={() => go(bucket, "")}>
              {bucket}
            </Anchor>
          </>
        )}
        {crumbs.map((crumb) => (
          <Group key={crumb.prefix} gap={4}>
            <Text size="sm">/</Text>
            <Anchor component="button" type="button" size="sm" onClick={() => go(bucket, crumb.prefix)}>
              {crumb.name}
            </Anchor>
          </Group>
        ))}
      </Group>
      {creating ? (
        <Group align="flex-start" gap="xs" wrap="nowrap">
          <TextInput
            aria-label={tr("New folder name")}
            placeholder={tr("Folder name")}
            value={name}
            onChange={(event) => {
              setName(event.currentTarget.value);
              setNameError(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                event.stopPropagation();
                void create();
              }
            }}
            error={nameError}
            autoComplete="off"
            spellCheck={false}
            style={{ flex: 1 }}
            data-autofocus
          />
          <Button variant="default" onClick={() => void create()}>
            {tr("Create")}
          </Button>
        </Group>
      ) : null}
      <Stack gap={2} role="list" aria-label={tr("Destinations")} mih={160} mah={280} style={{ overflowY: "auto" }}>
        {!ready ? (
          <Text size="sm" c="dimmed">
            {tr("Loading...")}
          </Text>
        ) : listing.error !== null ? (
          <Text size="sm" c="red" role="alert">
            {listing.error}
          </Text>
        ) : bucket === null ? (
          listing.buckets.map((item) => (
            <UnstyledButton key={item.name} role="listitem" onClick={() => go(item.name, "")} py={4}>
              <Group gap="xs">
                <IconBucket size={16} />
                <Text size="sm">{item.name}</Text>
              </Group>
            </UnstyledButton>
          ))
        ) : listing.folders.length === 0 ? (
          <Text size="sm" c="dimmed">
            {tr("No folders here.")}
          </Text>
        ) : (
          listing.folders.map((key) => (
            <UnstyledButton key={key} role="listitem" onClick={() => go(bucket, key)} py={4}>
              <Group gap="xs">
                <IconFolder size={16} />
                <Text size="sm">{displaySegment(relativeName(key, prefix))}</Text>
              </Group>
            </UnstyledButton>
          ))
        )}
      </Stack>
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        {tr("Destination:")} {bucket === null ? tr("none chosen") : s3Uri(bucket, prefix)}
      </Text>
      <Select
        label={tr("If an item already exists there")}
        data={POLICY_OPTIONS}
        value={policy}
        allowDeselect={false}
        onChange={(value) => {
          if (isPolicyChoice(value)) {
            setPolicy(value);
          }
        }}
        comboboxProps={{ withinPortal: false }}
      />
      {problem !== null && bucket !== null ? (
        <Text size="sm" c="red" role="alert">
          {problem}
        </Text>
      ) : null}
    </OkCancelDialog>
  );
}
