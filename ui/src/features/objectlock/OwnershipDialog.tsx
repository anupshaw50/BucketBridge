import { Radio, Stack, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { ObjectOwnership } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";

const OPTIONS: { value: ObjectOwnership; label: string; hint: string }[] = [
  {
    value: "BucketOwnerEnforced",
    label: "Bucket owner enforced",
    hint: "ACLs are disabled; the bucket owner owns every object.",
  },
  {
    value: "BucketOwnerPreferred",
    label: "Bucket owner preferred",
    hint: "The bucket owner owns objects uploaded with the bucket-owner-full-control ACL.",
  },
  { value: "ObjectWriter", label: "Object writer", hint: "The account that uploads an object owns it." },
];

/** Object Ownership of a bucket. */
export function OwnershipDialog({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const [value, setValue] = useState<ObjectOwnership | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    model.api
      .get_object_ownership({ account_id: accountId, bucket })
      .then((current) => {
        if (live) {
          setValue(current);
          setLoaded(true);
        }
      })
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket]);

  const submit = async () => {
    if (value === null) {
      return;
    }
    if (value === "BucketOwnerEnforced") {
      const confirmed = await model.dialogs.confirm({
        title: tr("Disable ACLs"),
        message: tr("With BucketOwnerEnforced, ACLs of {bucket} stop working and requests that set them fail.", { bucket }),
        confirmLabel: tr("Disable ACLs"),
        danger: true,
        typedConfirmation: bucket,
      });
      if (!confirmed) {
        return;
      }
    }
    setBusy(true);
    setError(null);
    try {
      await model.api.put_object_ownership({ account_id: accountId, bucket, ownership: value });
      model.status.info(tr("Object ownership of {bucket} saved.", { bucket }));
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
      title={tr("Object ownership: {bucket}", { bucket })}
      okLabel={tr("Save")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={!loaded || value === null}
      error={error}
    >
      <Radio.Group value={value ?? ""} onChange={(next) => setValue(next as ObjectOwnership)}>
        <Stack gap="sm">
          {OPTIONS.map((option) => (
            <Radio key={option.value} value={option.value} label={tr(option.label)} description={tr(option.hint)} />
          ))}
        </Stack>
      </Radio.Group>
      {loaded && value === null && <Text size="sm" c="dimmed">{tr("No setting is configured; S3 behaves as Object writer.")}</Text>}
    </OkCancelDialog>
  );
}
