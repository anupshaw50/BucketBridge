import { Alert, Button, Checkbox, Group, Select, Stack, Text, Textarea, TextInput } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { CloudFrontDistribution, CreatedDistribution } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";

const PRICE_CLASSES = [
  { value: "PriceClass_All", label: tr("All edge locations") },
  { value: "PriceClass_200", label: tr("North America, Europe, Asia, Middle East and Africa") },
  { value: "PriceClass_100", label: tr("North America and Europe") },
];

/** At most 128 characters, the limit of the service. */
const MAX_COMMENT = 128;

/** Attaches a new distribution to a bucket: the origin is the regional endpoint of the bucket. */
export function AttachDialog({
  accountId,
  bucket,
  onClose,
  onCreated,
}: {
  accountId: string;
  bucket: string;
  onClose: () => void;
  onCreated: (created: CreatedDistribution) => void;
}) {
  const model = useModel();
  const known = model.browser.state.bucketList.buckets.find((entry) => entry.name === bucket)?.region ?? null;
  const [region, setRegion] = useState(known ?? "us-east-1");
  const [comment, setComment] = useState(`s3://${bucket}`);
  const [root, setRoot] = useState("index.html");
  const [priceClass, setPriceClass] = useState("PriceClass_All");
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreatedDistribution | null>(null);

  const problem =
    region.trim() === ""
      ? tr("Enter the region of the bucket.")
      : comment.length > MAX_COMMENT
        ? tr("The comment is limited to {n} characters.", { n: MAX_COMMENT })
        : root.startsWith("/")
          ? tr("The default root object has no leading '/'.")
          : null;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await model.api.create_cloudfront_distribution({
        account_id: accountId,
        new: { bucket, region: region.trim(), comment, enabled, default_root_object: root.trim(), price_class: priceClass },
      });
      setCreated(result);
      onCreated(result);
      model.status.info(tr("Distribution {id} created for {bucket}.", { id: result.distribution.id, bucket }));
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppModal opened onClose={onClose} title={tr("Attach distribution")} size="lg">
      <Stack gap="sm">
        {created === null ? (
          <>
            <Text size="sm" c="dimmed">
              {tr("Creates a distribution that serves s3://{bucket} through an origin access control. The bucket is not changed.", { bucket })}
            </Text>
            {error !== null && <Alert color="red">{error}</Alert>}
            <TextInput label={tr("Bucket region")} value={region} onChange={(event) => setRegion(event.currentTarget.value)} />
            <TextInput label={tr("Comment")} value={comment} onChange={(event) => setComment(event.currentTarget.value)} />
            <TextInput label={tr("Default root object")} value={root} onChange={(event) => setRoot(event.currentTarget.value)} />
            <Select
              label={tr("Price class")}
              data={PRICE_CLASSES}
              value={priceClass}
              onChange={(value) => setPriceClass(value ?? "PriceClass_All")}
              allowDeselect={false}
              comboboxProps={{ withinPortal: false }}
            />
            <Checkbox label={tr("Enabled")} checked={enabled} onChange={(event) => setEnabled(event.currentTarget.checked)} />
            {problem !== null && (
              <Text size="sm" c="red">
                {problem}
              </Text>
            )}
            <Group justify="flex-end">
              <Button variant="default" onClick={onClose}>
                {tr("Cancel")}
              </Button>
              <Button loading={busy} disabled={problem !== null} onClick={() => void submit()}>
                {tr("Create distribution")}
              </Button>
            </Group>
          </>
        ) : (
          <>
            <Alert color="green" title={tr("Distribution created")}>
              {tr("{id} ({domain}) is deploying.", { id: created.distribution.id, domain: created.distribution.domain_name })}
            </Alert>
            <Text size="sm">{tr("Add this statement to the bucket policy so the distribution can read the objects.")}</Text>
            <Textarea label={tr("Bucket policy")} readOnly rows={6} value={created.bucket_policy} />
            <Group justify="flex-end">
              <Button onClick={onClose}>{tr("Close")}</Button>
            </Group>
          </>
        )}
      </Stack>
    </AppModal>
  );
}

/** Edits the settings of a distribution: enabled, comment, price class, aliases and the default root object. */
export function EditDialog({
  accountId,
  id,
  onClose,
  onSaved,
}: {
  accountId: string;
  id: string;
  onClose: () => void;
  onSaved: (updated: CloudFrontDistribution) => void;
}) {
  const model = useModel();
  const [loaded, setLoaded] = useState<CloudFrontDistribution | null>(null);
  const [comment, setComment] = useState("");
  const [aliases, setAliases] = useState("");
  const [root, setRoot] = useState("");
  const [priceClass, setPriceClass] = useState("PriceClass_All");
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    model.api
      .get_cloudfront_distribution({ account_id: accountId, id })
      .then((found) => {
        if (!live) {
          return;
        }
        setLoaded(found);
        setComment(found.comment);
        setAliases(found.aliases.join("\n"));
        setRoot(found.default_root_object ?? "");
        setPriceClass(found.price_class);
        setEnabled(found.enabled);
      })
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, id]);

  const problem =
    comment.length > MAX_COMMENT ? tr("The comment is limited to {n} characters.", { n: MAX_COMMENT }) : root.startsWith("/") ? tr("The default root object has no leading '/'.") : null;

  const save = async () => {
    if (loaded === null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const updated = await model.api.update_cloudfront_distribution({
        account_id: accountId,
        id,
        edit: {
          comment,
          aliases: aliases
            .split(/\r?\n/)
            .map((line) => line.trim())
            .filter((line) => line !== ""),
          default_root_object: root.trim(),
          price_class: priceClass,
          enabled,
        },
        expected_etag: loaded.etag,
      });
      onSaved(updated);
      model.status.info(tr("Distribution {id} saved.", { id }));
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppModal opened onClose={onClose} title={tr("Edit distribution {id}", { id })} size="lg">
      <Stack gap="sm">
        {error !== null && <Alert color="red">{error}</Alert>}
        <TextInput label={tr("Comment")} disabled={loaded === null} value={comment} onChange={(event) => setComment(event.currentTarget.value)} />
        <Textarea
          label={tr("Aliases")}
          description={tr("One domain name per line.")}
          disabled={loaded === null}
          rows={3}
          value={aliases}
          onChange={(event) => setAliases(event.currentTarget.value)}
        />
        <TextInput label={tr("Default root object")} disabled={loaded === null} value={root} onChange={(event) => setRoot(event.currentTarget.value)} />
        <Select
          label={tr("Price class")}
          data={PRICE_CLASSES}
          value={priceClass}
          disabled={loaded === null}
          onChange={(value) => setPriceClass(value ?? "PriceClass_All")}
          allowDeselect={false}
          comboboxProps={{ withinPortal: false }}
        />
        <Checkbox label={tr("Enabled")} disabled={loaded === null} checked={enabled} onChange={(event) => setEnabled(event.currentTarget.checked)} />
        {problem !== null && (
          <Text size="sm" c="red">
            {problem}
          </Text>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Cancel")}
          </Button>
          <Button loading={busy} disabled={loaded === null || problem !== null} onClick={() => void save()}>
            {tr("Save")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
