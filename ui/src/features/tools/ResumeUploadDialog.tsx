import { Alert, Button, Checkbox, Group, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { UploadResumeCheck, UploadRow } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";

/** The Chinese-ready sentence for a refused file; the host's own message follows it. */
function refusal(check: UploadResumeCheck): string {
  switch (check.problem?.code) {
    case "file_too_small":
      return tr("This file is smaller than the part of the upload that is already on the server, so it is not the file the upload was made from.");
    case "part_sizes":
      return tr("The parts on the server do not fit this file, so it is not the file the upload was made from.");
    case "part_mismatch":
      return tr("Part {part} on the server is not the same as this file, so it is not the file the upload was made from.", {
        part: check.problem.part ?? 0,
      });
    default:
      return "";
  }
}

/**
 * Finishes one unfinished multipart upload from the local file it was made from: the file is compared with the
 * parts already on the server, and only the missing parts are uploaded (as a task of the transfer queue).
 */
export function ResumeUploadDialog({
  accountId,
  upload,
  onClose,
  onResumed,
}: {
  accountId: string;
  upload: UploadRow;
  onClose: () => void;
  onResumed: () => void;
}) {
  const model = useModel();
  const [path, setPath] = useState<string | null>(null);
  const [skip, setSkip] = useState(false);
  const [check, setCheck] = useState<UploadResumeCheck | null>(null);
  const [checking, setChecking] = useState(false);
  const [resuming, setResuming] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const args = (file: string, skipVerification: boolean) => ({
    account_id: accountId,
    bucket: upload.bucket,
    key: upload.key,
    upload_id: upload.upload_id,
    path: file,
    skip_part_verification: skipVerification,
  });
  const verify = async (file: string, skipVerification: boolean) => {
    setChecking(true);
    setError(null);
    setCheck(null);
    try {
      setCheck(await model.api.check_multipart_resume(args(file, skipVerification)));
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setChecking(false);
    }
  };
  const choose = async () => {
    try {
      const picked = await model.platform.pickFiles(tr("Choose the file this upload was made from"));
      const file = picked?.[0];
      if (file !== undefined) {
        setPath(file);
        await verify(file, skip);
      }
    } catch (raw) {
      model.status.error(raw, { notify: tr("The file dialog could not be opened") });
    }
  };
  const resume = async () => {
    if (path === null) {
      return;
    }
    setResuming(true);
    setError(null);
    try {
      await model.api.resume_multipart_upload(args(path, skip));
      model.status.info(tr("The upload of {key} continues in the transfers list.", { key: upload.key }));
      onResumed();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setResuming(false);
    }
  };

  const refused = check?.problem ?? null;
  return (
    <AppModal opened onClose={onClose} title={tr("Resume upload")} size="lg" closeOnClickOutside={false}>
      <Stack gap="sm">
        <Text size="sm">
          {tr("Choose the file this upload was made from. It is compared with the parts already on the server, and only the missing parts are uploaded.")}
        </Text>
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {tr("Upload to s3://{bucket}/{key}", { bucket: upload.bucket, key: upload.key })}
        </Text>
        <Group gap="xs" wrap="nowrap">
          <Button variant="default" loading={checking} onClick={() => void choose()}>
            {path === null ? tr("Choose file...") : tr("Choose another file...")}
          </Button>
          <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere", flex: 1 }}>
            {path ?? tr("No file chosen")}
          </Text>
        </Group>
        {error !== null ? (
          <Alert color="red" role="alert" py="xs">
            {error}
          </Alert>
        ) : null}
        {refused !== null && check !== null ? (
          <Alert color="red" role="alert" py="xs" title={tr("This file cannot finish the upload")}>
            <Stack gap={4}>
              <Text size="sm">{refusal(check)}</Text>
              <Text size="xs" c="dimmed">
                {refused.message}
              </Text>
            </Stack>
          </Alert>
        ) : null}
        {check !== null && refused === null ? (
          <Alert color="green" py="xs" title={tr("This file matches the upload")}>
            <Stack gap={2} aria-live="polite">
              <Text size="sm">
                {tr("{count} of {total} parts are already uploaded ({size}).", {
                  count: formatCount(check.parts.length),
                  total: formatCount(check.part_count),
                  size: formatBytes(check.bytes_uploaded),
                })}
              </Text>
              <Text size="sm">
                {tr("{count} parts remain to upload ({size}).", {
                  count: formatCount(check.missing_parts),
                  size: formatBytes(check.bytes_remaining),
                })}
              </Text>
              <Text size="xs" c="dimmed">
                {check.parts.some((part) => part.verified === "skipped")
                  ? tr("The content of the uploaded parts was not compared; their sizes and positions fit.")
                  : tr("The uploaded parts were compared with the file.")}
              </Text>
            </Stack>
          </Alert>
        ) : null}
        {path !== null && (refused?.code === "part_mismatch" || skip) ? (
          <Checkbox
            label={tr("Do not compare the content of the uploaded parts with the file")}
            description={tr("Use this when the upload is encrypted with a KMS key or a customer key and has no checksums: the parts then cannot be compared. Only the sizes and positions are checked.")}
            checked={skip}
            onChange={(event) => {
              const next = event.currentTarget.checked;
              setSkip(next);
              void verify(path, next);
            }}
          />
        ) : null}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Cancel")}
          </Button>
          <Button disabled={check === null || refused !== null} loading={resuming} onClick={() => void resume()}>
            {tr("Resume upload")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
