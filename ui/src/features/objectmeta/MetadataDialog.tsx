import { Alert, Button, Group, Loader, PasswordInput, Stack, Table, Tabs, Text } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { type BackendError, toBbError } from "../../api/errors";
import type { ObjectDetails, ObjectEntry } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { CopyField } from "../../components/CopyField";
import { plural } from "../../lib/format";
import { isFolderEntry } from "../../lib/objects";
import { baseName, displaySegment, s3Uri } from "../../lib/s3keys";
import { validateCustomerKey } from "../../lib/objectMeta";
import type { ObjectPropertiesTab } from "../../state/dialogs";
import { HeadersTab, TagsTab, UserMetaTab } from "./EditTabs";
import { GeneralTab } from "./GeneralTab";
import type { TabContext } from "./apply";
import { EncryptionTab, PermissionsTab, StorageClassTab } from "./SecurityTabs";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; details: ObjectDetails }
  | { status: "error"; error: BackendError };

/** The message that asks for the key of an SSE-C object. */
function needsKey(error: BackendError): boolean {
  return error.message.includes("SSE-C");
}

function FolderProperties({ bucket, entries, onClose }: { bucket: string; entries: readonly ObjectEntry[]; onClose: () => void }) {
  const only = entries.length === 1 ? entries[0] : undefined;
  return (
    <AppModal opened onClose={onClose} title="Properties" size="lg">
      <Stack gap="sm">
        {only !== undefined ? (
          <>
            <Table withTableBorder verticalSpacing={4} fz="sm">
              <Table.Tbody>
                <Table.Tr>
                  <Table.Th scope="row" w={150} fw={500}>
                    Name
                  </Table.Th>
                  <Table.Td>{displaySegment(baseName(only.key))}</Table.Td>
                </Table.Tr>
                <Table.Tr>
                  <Table.Th scope="row" fw={500}>
                    Type
                  </Table.Th>
                  <Table.Td>Folder</Table.Td>
                </Table.Tr>
                <Table.Tr>
                  <Table.Th scope="row" fw={500}>
                    Bucket
                  </Table.Th>
                  <Table.Td>{bucket}</Table.Td>
                </Table.Tr>
                <Table.Tr>
                  <Table.Th scope="row" fw={500}>
                    Key
                  </Table.Th>
                  <Table.Td style={{ overflowWrap: "anywhere" }}>
                    <Text size="sm" ff="monospace">
                      {only.key}
                    </Text>
                  </Table.Td>
                </Table.Tr>
              </Table.Tbody>
            </Table>
            <CopyField label="S3 URI" value={s3Uri(bucket, only.key)} />
          </>
        ) : null}
        <Text size="sm" c="dimmed">
          A folder has no metadata of its own: tags, storage class, encryption, headers and ACLs belong to the objects
          in it. Select objects to edit them.
        </Text>
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}

/**
 * The properties of the selected objects: read-only facts and editors for
 * HTTP headers, user metadata, tags, storage class, encryption and ACLs. With
 * one object the editors show its values; with several they edit a patch that
 * is applied to each, and show the result per object.
 */
export function MetadataDialog({
  accountId,
  bucket,
  entries,
  initialTab = "general",
  onClose,
}: {
  accountId: string;
  bucket: string;
  entries: ObjectEntry[];
  /** The tab that is shown when the dialog opens. */
  initialTab?: ObjectPropertiesTab;
  onClose: () => void;
}) {
  const model = useModel();
  const objects = useMemo(() => entries.filter((entry) => !isFolderEntry(entry)), [entries]);
  const folders = entries.length - objects.length;
  const single = objects.length === 1 && folders === 0;
  const keys = useMemo(() => objects.map((entry) => entry.key), [objects]);
  const firstKey = keys[0] ?? "";
  const [tab, setTab] = useState<string | null>(initialTab);
  const [sseKey, setSseKey] = useState("");
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const request = useRef(0);

  // The first read of a single object; reads after an edit or with a key go through `read`.
  useEffect(() => {
    if (!single) {
      return undefined;
    }
    let live = true;
    model.api
      .get_object_details({ account_id: accountId, bucket, key: firstKey, sse_c_key: null })
      .then(
        (details) => live && setState({ status: "ready", details }),
        (raw: unknown) => live && setState({ status: "error", error: toBbError(raw) }),
      );
    return () => {
      live = false;
    };
  }, [model, single, accountId, bucket, firstKey]);

  const read = useCallback(
    (key: string | null, keepOnError: boolean) => {
      const id = ++request.current;
      if (!keepOnError) {
        setState({ status: "loading" });
      }
      model.api
        .get_object_details({ account_id: accountId, bucket, key: firstKey, sse_c_key: key })
        .then(
          (details) => {
            if (id === request.current) {
              setState({ status: "ready", details });
            }
          },
          (raw: unknown) => {
            if (id === request.current && !keepOnError) {
              setState({ status: "error", error: toBbError(raw) });
            }
          },
        );
    },
    [model, accountId, bucket, firstKey],
  );

  const typedKey = sseKey.trim().length > 0 && validateCustomerKey(sseKey) === null ? sseKey.trim() : null;
  const changed = useCallback(
    (nextKey?: string) => {
      if (nextKey !== undefined) {
        setSseKey(nextKey);
      }
      if (single) {
        const key = (nextKey ?? sseKey).trim();
        read(key.length > 0 && validateCustomerKey(key) === null ? key : null, true);
      }
      void model.browser.refresh();
    },
    [single, read, sseKey, model],
  );

  if (objects.length === 0) {
    return <FolderProperties bucket={bucket} entries={entries} onClose={onClose} />;
  }

  const details = state.status === "ready" ? state.details : null;
  const ctx: TabContext = { accountId, bucket, keys, details: single ? details : null, batch: !single, sseKey, setSseKey, changed };
  const title = single ? "Properties" : `Properties of ${plural(objects.length, "object")}`;
  const showTabs = !single || state.status === "ready";
  return (
    <AppModal opened onClose={onClose} title={title} size="xl">
      <Stack gap="sm">
        {folders > 0 ? (
          <Text size="sm" c="dimmed">
            {plural(folders, "folder")} in the selection {folders === 1 ? "is" : "are"} left out: folders have no metadata of their own.
          </Text>
        ) : null}
        {single && state.status === "loading" ? (
          <Group gap="xs" role="status">
            <Loader size="sm" />
            <Text size="sm">Loading properties</Text>
          </Group>
        ) : null}
        {single && state.status === "error" ? (
          <Stack gap="xs">
            <Alert color="red" icon={<IconAlertCircle size={16} />} title="The properties could not be read" role="alert">
              <Text size="sm" style={{ overflowWrap: "anywhere" }}>
                {state.error.message}
              </Text>
            </Alert>
            {needsKey(state.error) ? (
              <Group align="flex-end" gap="xs">
                <PasswordInput
                  label="Customer key (SSE-C)"
                  description="If the object is encrypted with a customer-provided key, enter it to read the object. It is never stored."
                  value={sseKey}
                  onChange={(event) => setSseKey(event.currentTarget.value)}
                  error={sseKey.length > 0 ? validateCustomerKey(sseKey) : null}
                  autoComplete="off"
                  style={{ flex: 1 }}
                />
                <Button disabled={typedKey === null} onClick={() => read(typedKey, false)}>
                  Read with key
                </Button>
              </Group>
            ) : (
              <Group>
                <Button variant="default" onClick={() => read(null, false)}>
                  Try again
                </Button>
              </Group>
            )}
          </Stack>
        ) : null}
        {showTabs ? (
          <Tabs value={tab} onChange={setTab} keepMounted={false}>
            <Tabs.List>
              <Tabs.Tab value="general">General</Tabs.Tab>
              <Tabs.Tab value="headers">HTTP headers</Tabs.Tab>
              <Tabs.Tab value="metadata">User metadata</Tabs.Tab>
              <Tabs.Tab value="tags">Tags</Tabs.Tab>
              <Tabs.Tab value="class">Storage class</Tabs.Tab>
              <Tabs.Tab value="encryption">Encryption</Tabs.Tab>
              <Tabs.Tab value="permissions">Permissions</Tabs.Tab>
            </Tabs.List>
            <Tabs.Panel value="general" pt="sm">
              <GeneralTab ctx={ctx} entries={objects} />
            </Tabs.Panel>
            <Tabs.Panel value="headers" pt="sm">
              <HeadersTab ctx={ctx} />
            </Tabs.Panel>
            <Tabs.Panel value="metadata" pt="sm">
              <UserMetaTab ctx={ctx} />
            </Tabs.Panel>
            <Tabs.Panel value="tags" pt="sm">
              <TagsTab ctx={ctx} />
            </Tabs.Panel>
            <Tabs.Panel value="class" pt="sm">
              <StorageClassTab ctx={ctx} />
            </Tabs.Panel>
            <Tabs.Panel value="encryption" pt="sm">
              <EncryptionTab ctx={ctx} />
            </Tabs.Panel>
            <Tabs.Panel value="permissions" pt="sm">
              <PermissionsTab ctx={ctx} />
            </Tabs.Panel>
          </Tabs>
        ) : null}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
