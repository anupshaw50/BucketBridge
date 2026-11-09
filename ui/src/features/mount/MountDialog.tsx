import { Alert, Button, Group, Select, Stack, Table, Text, TextInput } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { MountInfo, MountStatus } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";

/**
 * Mounts a bucket read-only as a folder (FUSE) or, on Windows, as a drive letter or folder (Dokan), and lists
 * the mounts that are active. A folder must exist and be empty; the mount ends with "Unmount" or when the app quits.
 */
export function MountDialog({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const [status, setStatus] = useState<MountStatus | null>(null);
  const [mountPoint, setMountPoint] = useState("");
  const [prefix, setPrefix] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    model.api
      .mount_status()
      .then((next) => {
        if (live) {
          setStatus(next);
        }
      })
      .catch((raw: unknown) => {
        if (live) {
          setError(toBbError(raw).message);
        }
      });
    const stop = model.api.listen("mount://changed", (event) => {
      setStatus((current) => (current === null ? current : { ...current, mounts: event.mounts }));
    });
    return () => {
      live = false;
      stop();
    };
  }, [model]);

  const choose = async () => {
    try {
      const picked = await model.platform.pickFolder(tr("Choose the mount folder"));
      if (picked !== null) {
        setMountPoint(picked);
      }
    } catch (raw) {
      model.status.error(raw, { notify: tr("The folder dialog could not be opened") });
    }
  };

  const mount = async () => {
    setBusy(true);
    setError(null);
    try {
      const info = await model.api.mount_bucket({
        account_id: accountId,
        bucket,
        prefix: prefix.trim() === "" ? null : prefix.trim(),
        mount_point: mountPoint.trim(),
      });
      model.status.info(tr("Mounted {bucket} on {point}.", { bucket: info.bucket, point: info.mount_point }));
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const unmount = async (info: MountInfo) => {
    setError(null);
    try {
      await model.api.unmount_bucket({ mount_point: info.mount_point });
      model.status.info(tr("Unmounted {point}.", { point: info.mount_point }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const windows = status?.platform === "windows";
  const driverName = windows ? "Dokan" : "FUSE";
  const ready = status !== null && status.supported && status.fuse_installed;
  return (
    <AppModal opened onClose={onClose} title={tr("Mount {bucket} as drive", { bucket })} size="lg">
      <Stack gap="sm">
        {status !== null && !status.supported && (
          <Alert color="yellow">{tr("This build of BucketBridge cannot mount buckets. See docs/mount.md.")}</Alert>
        )}
        {status !== null && status.supported && !status.fuse_installed && (
          <Alert color="yellow">
            {windows
              ? tr("Dokan is not installed. Install the Dokan 2 runtime (github.com/dokan-dev/dokany/releases) and try again.")
              : tr("FUSE is not installed. Install macFUSE or FUSE-T (macOS) or fuse3 (Linux) and try again.")}
          </Alert>
        )}
        <Text size="sm">
          {tr("The bucket appears as a read-only {form} ({driver}). Nothing in it can be changed from there.", {
            form: windows ? tr("drive or folder") : tr("folder"),
            driver: driverName,
          })}
        </Text>
        {windows && (
          <Select
            label={tr("Drive letter")}
            placeholder={tr("Pick a free drive letter")}
            data={status.drive_letters.map((letter) => `${letter}:`)}
            value={/^[A-Za-z]:$/.test(mountPoint.trim()) ? mountPoint.trim().toUpperCase() : null}
            onChange={(value) => setMountPoint(value ?? "")}
            clearable
          />
        )}
        <TextInput
          label={tr("Mount folder")}
          description={
            windows
              ? tr("A free drive letter such as X:, or an existing empty folder on an NTFS drive.")
              : tr("An existing, empty folder (absolute path).")
          }
          value={mountPoint}
          onChange={(event) => setMountPoint(event.currentTarget.value)}
          rightSectionWidth={90}
          rightSection={
            <Button size="compact-sm" variant="default" onClick={() => void choose()}>
              {tr("Browse...")}
            </Button>
          }
        />
        <TextInput
          label={tr("Prefix (optional)")}
          description={tr("Mount only this folder of the bucket.")}
          value={prefix}
          onChange={(event) => setPrefix(event.currentTarget.value)}
        />
        <Group>
          <Button onClick={() => void mount()} loading={busy} disabled={!ready || mountPoint.trim() === ""}>
            {tr("Mount")}
          </Button>
        </Group>
        {error !== null && (
          <Alert color="red" role="alert">
            {error}
          </Alert>
        )}
        {status !== null && status.mounts.length > 0 && (
          <Table aria-label={tr("Active mounts")}>
            <Table.Thead>
              <Table.Tr>
                <Table.Th>{tr("Bucket")}</Table.Th>
                <Table.Th>{tr("Folder")}</Table.Th>
                <Table.Th />
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {status.mounts.map((info) => (
                <Table.Tr key={info.mount_point}>
                  <Table.Td>{info.prefix === "" ? info.bucket : `${info.bucket}/${info.prefix}`}</Table.Td>
                  <Table.Td>{info.mount_point}</Table.Td>
                  <Table.Td>
                    <Button size="compact-sm" variant="default" onClick={() => void unmount(info)}>
                      {tr("Unmount")}
                    </Button>
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        )}
      </Stack>
    </AppModal>
  );
}
