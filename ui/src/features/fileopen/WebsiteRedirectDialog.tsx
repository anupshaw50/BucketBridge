import { Text, TextInput } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { redirectProblem } from "./helpers";

/** Views, sets and clears `x-amz-website-redirect-location` of one object. */
export function WebsiteRedirectDialog({
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
  const [current, setCurrent] = useState<string | null | undefined>(undefined);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    model.api
      .get_object_details({ account_id: accountId, bucket, key: objectKey, sse_c_key: null })
      .then((details) => {
        if (live) {
          const location = details.meta.website_redirect ?? null;
          setCurrent(location);
          setValue(location ?? "");
        }
      })
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket, objectKey]);

  const text = value.trim();
  const problem = redirectProblem(value);
  const unchanged = current !== undefined && text === (current ?? "");

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const change = text.length === 0 ? ({ action: "remove" } as const) : ({ action: "set", value: text } as const);
      const results = await model.api.update_object_headers({
        account_id: accountId,
        bucket,
        keys: [objectKey],
        changes: {
          content_type: { action: "keep" },
          cache_control: { action: "keep" },
          content_disposition: { action: "keep" },
          content_encoding: { action: "keep" },
          website_redirect: change,
          user_meta_set: {},
          user_meta_remove: [],
        },
        sse_c_key: null,
      });
      const failed = results.find((result) => result.error !== null);
      if (failed !== undefined) {
        setError(failed.error?.message ?? "The redirect could not be saved.");
      } else {
        model.status.info(text.length === 0 ? `Cleared the website redirect of ${objectKey}.` : `Website redirect of ${objectKey} set.`);
        onClose();
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
      title="Website redirect"
      okLabel="Save"
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={current === undefined || problem !== null || unchanged}
      error={error}
      extraActions={
        current ? (
          <button type="button" onClick={() => setValue("")} disabled={busy || value.length === 0}>
            Clear
          </button>
        ) : undefined
      }
    >
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        {objectKey}
      </Text>
      <TextInput
        label="Redirect location"
        description="Applies when the bucket serves this object as a static website. Leave empty to remove the redirect."
        placeholder="/other-page.html or https://example.com/"
        value={value}
        onChange={(event) => setValue(event.currentTarget.value)}
        error={problem}
        disabled={current === undefined && error === null}
        data-autofocus
      />
    </OkCancelDialog>
  );
}
