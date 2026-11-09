import { Autocomplete, Button, Group, NumberInput, SegmentedControl, Select, Stack, Stepper, Text } from "@mantine/core";
import { IconLink } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { PresignMethod } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { CopyField } from "../../components/CopyField";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { formatDateTime } from "../../lib/format";
import { isFolderKey, s3Uri } from "../../lib/s3keys";
import { type ExpiryUnit, keyProblem, MAX_PRESIGN_SECONDS, UNIT_SECONDS } from "./expiry";
import { ShortenControl } from "./ShortenControl";

type Step = 0 | 1 | 2;

interface Created {
  url: string;
  method: PresignMethod;
  expiresAt: string;
}

/**
 * Sharing wizard of a bucket: choose the object, choose what the link allows and for how long, then
 * create the presigned link (and optionally shorten it). Back and Next move between the steps; a change
 * of an earlier step discards the link.
 */
export function SharingWizard({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const [step, setStep] = useState<Step>(0);
  const [key, setKey] = useState("");
  const [touched, setTouched] = useState(false);
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [method, setMethod] = useState<PresignMethod>("get");
  const [amount, setAmount] = useState<number>(1);
  const [unit, setUnit] = useState<ExpiryUnit>("hours");
  const [created, setCreated] = useState<Created | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);

  useEffect(() => {
    let live = true;
    model.api
      .list_objects({ account_id: accountId, bucket, prefix: "", flat: true, token: null })
      .then((page) => {
        if (live) {
          setSuggestions(page.entries.filter((entry) => !isFolderKey(entry.key)).map((entry) => entry.key));
        }
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [model, accountId, bucket]);

  const seconds = Number.isInteger(amount) ? amount * UNIT_SECONDS[unit] : Number.NaN;
  const keyError = keyProblem(key);
  const expiryError =
    !Number.isInteger(amount) || seconds < 1 || seconds > MAX_PRESIGN_SECONDS
      ? tr("Choose an expiry between 1 second and 7 days.")
      : null;

  const invalidate = () => {
    request.current += 1;
    setCreated(null);
    setBusy(false);
    setError(null);
  };
  const create = async () => {
    const id = ++request.current;
    setBusy(true);
    setError(null);
    try {
      const url = await model.api.presign({ account_id: accountId, bucket, key, method, expires_secs: seconds });
      if (id === request.current) {
        setCreated({ url, method, expiresAt: formatDateTime(new Date(Date.now() + seconds * 1000).toISOString()) });
        setStep(2);
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
  const next = () => {
    if (step === 0) {
      setTouched(true);
      if (keyError === null) {
        setStep(1);
      }
    } else if (step === 1) {
      if (expiryError === null) {
        void create();
      }
    } else {
      onClose();
    }
  };
  const back = () => {
    setError(null);
    setStep((current) => (current === 0 ? 0 : ((current - 1) as Step)));
  };

  return (
    <OkCancelDialog
      opened
      title={tr("Sharing wizard: {bucket}", { bucket })}
      okLabel={step === 0 ? tr("Next") : step === 1 ? tr("Create link") : tr("Done")}
      onOk={next}
      onCancel={onClose}
      cancelLabel={tr("Close")}
      busy={busy}
      error={error}
      size="lg"
      extraActions={
        step > 0 ? (
          <Button variant="default" onClick={back} disabled={busy}>
            {tr("Back")}
          </Button>
        ) : undefined
      }
    >
      <Stepper active={step} size="xs" allowNextStepsSelect={false}>
        <Stepper.Step label={tr("Object")} />
        <Stepper.Step label={tr("Access")} />
        <Stepper.Step label={tr("Link")} />
      </Stepper>
      {step === 0 ? (
        <Stack gap="xs">
          <Text size="sm">
            {tr("Create a temporary link that gives anyone who has it access to one object in")} <strong>{bucket}</strong>
            {tr(".")}
          </Text>
          <Autocomplete
            label={tr("Object key")}
            placeholder="folder/report.pdf"
            value={key}
            data={suggestions}
            limit={20}
            onChange={(value) => {
              setKey(value);
              invalidate();
            }}
            error={touched ? keyError : null}
            data-autofocus
          />
        </Stack>
      ) : null}
      {step === 1 ? (
        <Stack gap="xs">
          <Text size="sm" style={{ overflowWrap: "anywhere" }}>
            {tr("Link to")} <strong>{s3Uri(bucket, key)}</strong>
          </Text>
          <SegmentedControl<PresignMethod>
            value={method}
            onChange={(value) => {
              setMethod(value);
              invalidate();
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
              value={Number.isNaN(amount) ? "" : amount}
              onChange={(value) => {
                setAmount(typeof value === "number" ? value : Number.NaN);
                invalidate();
              }}
              error={expiryError}
            />
            <Select
              label={tr("Unit")}
              w={130}
              value={unit}
              allowDeselect={false}
              data={[
                { value: "seconds", label: tr("Seconds") },
                { value: "minutes", label: tr("Minutes") },
                { value: "hours", label: tr("Hours") },
                { value: "days", label: tr("Days") },
              ]}
              onChange={(value) => {
                if (value === "seconds" || value === "minutes" || value === "hours" || value === "days") {
                  setUnit(value);
                  invalidate();
                }
              }}
            />
          </Group>
          <Text size="xs" c="dimmed">
            {tr("The link works until it expires, or until the access of the account that signed it ends.")}
          </Text>
        </Stack>
      ) : null}
      {step === 2 && created !== null ? (
        <Stack gap={6}>
          <CopyField label={tr("Link")} value={created.url} multiline sensitive />
          <Group gap={4}>
            <IconLink size={12} aria-hidden />
            <Text size="xs" c="dimmed">
              {tr("Expires around {time}.", { time: created.expiresAt })}{" "}
              {created.method === "get"
                ? tr("Anyone with the link can download the object until then.")
                : tr("Upload with an HTTP PUT request to this link until then.")}
            </Text>
          </Group>
          <ShortenControl url={created.url} />
        </Stack>
      ) : null}
    </OkCancelDialog>
  );
}
