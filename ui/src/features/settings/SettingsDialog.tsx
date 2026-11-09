import {
  Alert,
  Button,
  Group,
  NavLink,
  NumberInput,
  PasswordInput,
  Radio,
  SegmentedControl,
  Select,
  Stack,
  Switch,
  Text,
  Textarea,
  TextInput,
  Title,
} from "@mantine/core";
import { IconAlertCircle, IconFolderOpen } from "@tabler/icons-react";
import { type ReactNode, useState } from "react";
import { errorText } from "../../api/errors";
import type { ChecksumAlgorithm, IfExists, LogLevel, ProxyKind, Settings, ThemeSetting } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { getLocale, setLocale, tr, type Locale } from "../../i18n";
import {
  cloneSettings,
  hasErrors,
  pageOfField,
  type RangedField,
  SETTINGS_RANGES,
  type SettingsErrors,
  type SettingsField,
  type SettingsPage,
  validateSettings,
} from "../../lib/settings";
import { parseBypassList } from "../../lib/validation";
import { ShortenerSettings } from "../sharing/ShortenerSettings";

const PAGES: { id: SettingsPage; label: string }[] = [
  { id: "general", label: tr("General") },
  { id: "interface", label: tr("Interface") },
  { id: "transfer", label: tr("Transfers") },
  { id: "advanced", label: tr("Advanced") },
  { id: "data_integrity", label: tr("Data integrity") },
  { id: "logging", label: tr("Logging") },
  { id: "proxy", label: tr("Proxy") },
  { id: "sharing", label: tr("Sharing") },
];

const CHECKSUMS: { value: ChecksumAlgorithm; label: string }[] = [
  { value: "crc32", label: "CRC32" },
  { value: "crc32c", label: "CRC32C" },
  { value: "crc64nvme", label: "CRC64NVME" },
  { value: "sha1", label: "SHA-1" },
  { value: "sha256", label: "SHA-256" },
  { value: "none", label: tr("None (only when the service requires one)") },
];

const IF_EXISTS_OPTIONS: { value: IfExists; label: string }[] = [
  { value: "overwrite", label: tr("Overwrite") },
  { value: "ask", label: tr("Ask me") },
  { value: "skip", label: tr("Skip") },
  { value: "overwrite_if_newer", label: tr("Overwrite if newer") },
  { value: "rename", label: tr("Rename (name (1).ext)") },
];

function isIfExists(value: string | null): value is IfExists {
  return IF_EXISTS_OPTIONS.some((item) => item.value === value);
}

const LOG_LEVEL_OPTIONS: { value: LogLevel; label: string }[] = [
  { value: "error", label: tr("Errors only") },
  { value: "warning", label: tr("Warnings and errors") },
  { value: "information", label: tr("Everything (information, warnings, errors)") },
];

function isChecksum(value: string | null): value is ChecksumAlgorithm {
  return CHECKSUMS.some((item) => item.value === value);
}

function isLogLevel(value: string | null): value is LogLevel {
  return LOG_LEVEL_OPTIONS.some((item) => item.value === value);
}

interface RangedNumberProps {
  field: RangedField;
  label: string;
  unit?: string;
  value: number | null;
  errors: SettingsErrors;
  onChange: (value: number) => void;
}

function RangedNumber({ field, label, unit, value, errors, onChange }: RangedNumberProps) {
  const { min, max } = SETTINGS_RANGES[field];
  return (
    <NumberInput
      label={label}
      description={`${min.toLocaleString("en-US")} to ${max.toLocaleString("en-US")}${unit ? ` ${unit}` : ""}`}
      value={value === null || Number.isNaN(value) ? "" : value}
      onChange={(next) => onChange(typeof next === "number" ? next : Number.NaN)}
      allowDecimal={false}
      allowNegative={false}
      thousandSeparator=","
      error={errors[field]}
      w={260}
    />
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <Stack gap="sm">
      <Title order={4}>{title}</Title>
      {children}
    </Stack>
  );
}

function ProxyPassword({ passwordSet, onSaved }: { passwordSet: boolean; onSaved: (ref: string | null) => void }) {
  const model = useModel();
  const [editing, setEditing] = useState(false);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const store = async (value: string | null) => {
    setBusy(true);
    setError(null);
    try {
      await model.api.set_proxy_password({ password: value });
      const ref = await model.settings.refreshProxyPasswordRef();
      onSaved(ref);
      model.status.info(tr(value === null ? "Proxy password removed." : "Proxy password saved."));
      setEditing(false);
      setPassword("");
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Stack gap={6}>
      <Text size="sm" fw={500}>
        {tr("Password")}
      </Text>
      <Text size="sm" c="dimmed">
        {tr(passwordSet ? "A password is stored in the credential store." : "No password is stored.")}{" "}
        {tr("Passwords are never shown again; changes here apply immediately.")}
      </Text>
      {editing ? (
        <Group align="flex-end" gap="xs">
          <PasswordInput
            label={tr("New proxy password")}
            value={password}
            onChange={(event) => setPassword(event.currentTarget.value)}
            w={260}
            autoComplete="new-password"
            data-autofocus
          />
          <Button size="xs" loading={busy} disabled={password.length === 0} onClick={() => void store(password)}>
            {tr("Save password")}
          </Button>
          <Button size="xs" variant="default" onClick={() => setEditing(false)}>
            {tr("Cancel")}
          </Button>
        </Group>
      ) : (
        <Group gap="xs">
          <Button size="xs" variant="default" onClick={() => setEditing(true)}>
            {tr(passwordSet ? "Change password..." : "Set password...")}
          </Button>
          {passwordSet ? (
            <Button size="xs" variant="subtle" color="red" loading={busy} onClick={() => void store(null)}>
              {tr("Remove password")}
            </Button>
          ) : null}
        </Group>
      )}
      {error !== null ? (
        <Text size="xs" c="red" role="alert">
          {error}
        </Text>
      ) : null}
    </Stack>
  );
}

function SettingsForm({ saved, onClose, initialPage }: { saved: Settings; onClose: () => void; initialPage?: SettingsPage }) {
  const model = useModel();
  const logDir = useSlice(model.app, (state) => state.info?.log_dir ?? null);
  const [draft, setDraft] = useState(() => cloneSettings(saved));
  const [bypassText, setBypassText] = useState(saved.proxy.bypass.join("\n"));
  const [page, setPage] = useState<SettingsPage>(initialPage ?? "general");
  const [errors, setErrors] = useState<SettingsErrors>({});
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const change = (mutate: (next: Settings) => void, fields: SettingsField[] = []) => {
    setDraft((current) => {
      const next = cloneSettings(current);
      mutate(next);
      return next;
    });
    if (fields.length > 0) {
      setErrors((current) => {
        const next = { ...current };
        for (const field of fields) {
          delete next[field];
        }
        return next;
      });
    }
  };
  const close = () => {
    model.settings.setPreviewTheme(null);
    onClose();
  };
  const apply = async (closeAfter: boolean) => {
    const candidate = cloneSettings(draft);
    candidate.proxy.bypass = parseBypassList(bypassText);
    const found = validateSettings(candidate);
    setErrors(found);
    setSaveError(null);
    if (hasErrors(found)) {
      const first = Object.keys(found)[0] as SettingsField | undefined;
      if (first !== undefined) {
        setPage(pageOfField(first));
      }
      return;
    }
    setBusy(true);
    try {
      await model.settings.save(candidate);
      setDraft(candidate);
      model.status.info(tr("Settings saved."));
      if (closeAfter) {
        close();
      }
    } catch (failure) {
      setSaveError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  const chooseDownloadDir = async () => {
    try {
      const folder = await model.platform.pickFolder(tr("Choose the default download folder"));
      if (folder !== null) {
        change((next) => {
          next.general.default_download_dir = folder;
        }, ["general.default_download_dir"]);
      }
    } catch (failure) {
      model.status.error(failure, { notify: tr("The folder dialog could not be opened") });
    }
  };
  const invalidPages = new Set(Object.keys(errors).map((field) => pageOfField(field as SettingsField)));

  let content: ReactNode;
  switch (page) {
    case "general":
      content = (
        <Section title={tr("General")}>
          <Switch
            label={tr("Ask before deleting")}
            description={tr("Show a confirmation before objects and folders are deleted.")}
            checked={draft.general.confirm_delete}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.general.confirm_delete = checked;
              });
            }}
          />
          <Switch
            label={tr("Keep the computer awake during transfers")}
            description={tr("Stops the computer from sleeping while transfers or a sync run work. It is released as soon as they end.")}
            checked={draft.general.keep_awake}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.general.keep_awake = checked;
              });
            }}
          />
          <Radio.Group
            label={tr("When quitting while transfers are running")}
            value={draft.general.quit_with_tasks}
            onChange={(value) =>
              change((next) => {
                next.general.quit_with_tasks = value === "wait" || value === "quit" ? value : "ask";
              })
            }
          >
            <Stack gap={6} mt={6}>
              <Radio value="ask" label={tr("Ask me what to do")} />
              <Radio value="wait" label={tr("Let running tasks finish, then quit")} />
              <Radio value="quit" label={tr("Quit at once (transfers can be resumed later)")} />
            </Stack>
          </Radio.Group>
          <Radio.Group
            label={tr("Download folder")}
            value={draft.general.default_download_dir === null ? "ask" : "fixed"}
            onChange={(value) =>
              change(
                (next) => {
                  next.general.default_download_dir = value === "ask" ? null : (saved.general.default_download_dir ?? "");
                },
                ["general.default_download_dir"],
              )
            }
          >
            <Stack gap={6} mt={6}>
              <Radio value="ask" label={tr("Ask where to save every time")} />
              <Radio value="fixed" label={tr("Always download to this folder")} />
            </Stack>
          </Radio.Group>
          {draft.general.default_download_dir !== null ? (
            <Group align="flex-start" gap="xs" wrap="nowrap">
              <TextInput
                aria-label={tr("Default download folder")}
                value={draft.general.default_download_dir}
                readOnly
                placeholder={tr("No folder chosen")}
                style={{ flex: 1 }}
                error={errors["general.default_download_dir"]}
              />
              <Button variant="default" leftSection={<IconFolderOpen size={14} />} onClick={() => void chooseDownloadDir()}>
                {tr("Browse...")}
              </Button>
            </Group>
          ) : null}
        </Section>
      );
      break;
    case "interface":
      content = (
        <Section title={tr("Interface")}>
          <Stack gap={4}>
            <Text size="sm" fw={500}>
              {tr("Language")}
            </Text>
            <SegmentedControl<Locale>
              value={getLocale()}
              onChange={(value) => {
                setLocale(value);
                // Labels that a module builds once (tables of choices, column
                // titles) are evaluated at import time, so the page reloads to
                // show every one of them in the new language.
                window.location.reload();
              }}
              data={[
                { value: "zh-CN", label: "简体中文" },
                { value: "en", label: "English" },
              ]}
              aria-label={tr("Language")}
              w={300}
            />
            <Text size="xs" c="dimmed">
              {tr("The whole interface switches at once.")}
            </Text>
          </Stack>
          <Stack gap={4}>
            <Text size="sm" fw={500}>
              {tr("Theme")}
            </Text>
            <SegmentedControl<ThemeSetting>
              value={draft.interface.theme}
              onChange={(value) => {
                change((next) => {
                  next.interface.theme = value;
                });
                model.settings.setPreviewTheme(value);
              }}
              data={[
                { value: "system", label: tr("System") },
                { value: "light", label: tr("Light") },
                { value: "dark", label: tr("Dark") },
              ]}
              aria-label={tr("Theme")}
              w={300}
            />
          </Stack>
          <Switch
            label={tr("Flat browsing")}
            description={tr("List every object below the open folder instead of one folder level.")}
            checked={draft.interface.flat_browsing}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.flat_browsing = checked;
              });
            }}
          />
          <Switch
            label={tr("Show the transfer queue")}
            checked={draft.interface.show_transfer_panel}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.show_transfer_panel = checked;
              });
            }}
          />
          <Switch
            label={tr("Show the event log")}
            checked={draft.interface.show_log_panel}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.show_log_panel = checked;
              });
            }}
          />
          <RangedNumber
            field="interface.bucket_page_size"
            label={tr("Buckets shown at once")}
            value={draft.interface.bucket_page_size}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.interface.bucket_page_size = value;
              }, ["interface.bucket_page_size"])
            }
          />
        </Section>
      );
      break;
    case "transfer":
      content = (
        <Section title={tr("Transfers")}>
          <RangedNumber
            field="transfer.max_concurrent_tasks"
            label={tr("Tasks running at the same time")}
            value={draft.transfer.max_concurrent_tasks}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.max_concurrent_tasks = value;
              }, ["transfer.max_concurrent_tasks"])
            }
          />
          <RangedNumber
            field="transfer.max_parts_per_task"
            label={tr("Parallel parts per task")}
            value={draft.transfer.max_parts_per_task}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.max_parts_per_task = value;
              }, ["transfer.max_parts_per_task"])
            }
          />
          <RangedNumber
            field="transfer.multipart_threshold_mib"
            label={tr("Multipart threshold")}
            unit="MiB"
            value={draft.transfer.multipart_threshold_mib}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.multipart_threshold_mib = value;
              }, ["transfer.multipart_threshold_mib"])
            }
          />
          <RangedNumber
            field="transfer.part_size_mib"
            label={tr("Part size (grows automatically for very large files)")}
            unit="MiB"
            value={draft.transfer.part_size_mib}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.part_size_mib = value;
              }, ["transfer.part_size_mib"])
            }
          />
          <Switch
            label={tr("Limit bandwidth")}
            description={tr("One limit shared by all running transfers.")}
            checked={draft.transfer.bandwidth_limit_kibps !== null}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.transfer.bandwidth_limit_kibps = checked ? 1024 : null;
              }, ["transfer.bandwidth_limit_kibps"]);
            }}
          />
          {draft.transfer.bandwidth_limit_kibps !== null ? (
            <RangedNumber
              field="transfer.bandwidth_limit_kibps"
              label={tr("Bandwidth limit")}
              unit="KiB/s"
              value={draft.transfer.bandwidth_limit_kibps}
              errors={errors}
              onChange={(value) =>
                change((next) => {
                  next.transfer.bandwidth_limit_kibps = value;
                }, ["transfer.bandwidth_limit_kibps"])
              }
            />
          ) : null}
          <RangedNumber
            field="transfer.retry_attempts"
            label={tr("Retries per part or object")}
            value={draft.transfer.retry_attempts}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.retry_attempts = value;
              }, ["transfer.retry_attempts"])
            }
          />
          <Select
            label={tr("When the destination exists")}
            description={tr("What uploads, downloads, copies and moves do with an item that is already there. Each transfer can choose another policy.")}
            data={IF_EXISTS_OPTIONS}
            value={draft.transfer.if_exists}
            allowDeselect={false}
            w={360}
            onChange={(value) => {
              if (isIfExists(value)) {
                change((next) => {
                  next.transfer.if_exists = value;
                });
              }
            }}
            comboboxProps={{ withinPortal: false }}
          />
        </Section>
      );
      break;
    case "advanced":
      content = (
        <Section title={tr("Advanced")}>
          <RangedNumber
            field="advanced.max_attempts"
            label={tr("Request attempts (including the first)")}
            value={draft.advanced.max_attempts}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.advanced.max_attempts = value;
              }, ["advanced.max_attempts"])
            }
          />
          <RangedNumber
            field="advanced.connect_timeout_secs"
            label={tr("Connection timeout")}
            unit={tr("seconds")}
            value={draft.advanced.connect_timeout_secs}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.advanced.connect_timeout_secs = value;
              }, ["advanced.connect_timeout_secs"])
            }
          />
          <RangedNumber
            field="advanced.read_timeout_secs"
            label={tr("Read timeout")}
            unit={tr("seconds")}
            value={draft.advanced.read_timeout_secs}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.advanced.read_timeout_secs = value;
              }, ["advanced.read_timeout_secs"])
            }
          />
          <RangedNumber
            field="advanced.list_page_size"
            label={tr("Objects per listing page")}
            value={draft.advanced.list_page_size}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.advanced.list_page_size = value;
              }, ["advanced.list_page_size"])
            }
          />
        </Section>
      );
      break;
    case "data_integrity":
      content = (
        <Section title={tr("Data integrity")}>
          <Select
            label={tr("Upload checksum")}
            description={tr("Algorithm used to protect uploads end to end.")}
            data={CHECKSUMS}
            value={draft.data_integrity.checksum_algorithm}
            allowDeselect={false}
            w={360}
            onChange={(value) => {
              if (isChecksum(value)) {
                change((next) => {
                  next.data_integrity.checksum_algorithm = value;
                });
              }
            }}
            comboboxProps={{ withinPortal: false }}
          />
          <Switch
            label={tr("Keep modification times")}
            description={tr("Downloaded files get the time of the object, and uploads store the file's time with the object.")}
            checked={draft.data_integrity.preserve_mtime}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.data_integrity.preserve_mtime = checked;
              });
            }}
          />
          <Switch
            label={tr("Validate downloads")}
            description={tr("Compare checksums of downloaded data when the object has one.")}
            checked={draft.data_integrity.validate_downloads}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.data_integrity.validate_downloads = checked;
              });
            }}
          />
        </Section>
      );
      break;
    case "logging":
      content = (
        <Section title={tr("Logging")}>
          <Select
            label={tr("Record")}
            description={tr("Minimum level written to the event log.")}
            data={LOG_LEVEL_OPTIONS}
            value={draft.logging.level}
            allowDeselect={false}
            w={360}
            onChange={(value) => {
              if (isLogLevel(value)) {
                change((next) => {
                  next.logging.level = value;
                });
              }
            }}
            comboboxProps={{ withinPortal: false }}
          />
          <Switch
            label={tr("Write the log to a file")}
            checked={draft.logging.file_logging}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.logging.file_logging = checked;
              });
            }}
          />
          <RangedNumber
            field="logging.max_file_mib"
            label={tr("Maximum log file size")}
            unit="MiB"
            value={draft.logging.max_file_mib}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.logging.max_file_mib = value;
              }, ["logging.max_file_mib"])
            }
          />
          <RangedNumber
            field="logging.keep_days"
            label={tr("Keep old log files for")}
            unit={tr("days")}
            value={draft.logging.keep_days}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.logging.keep_days = value;
              }, ["logging.keep_days"])
            }
          />
          <RangedNumber
            field="logging.max_total_mib"
            label={tr("Maximum size of all log files")}
            unit="MiB"
            value={draft.logging.max_total_mib}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.logging.max_total_mib = value;
              }, ["logging.max_total_mib"])
            }
          />
          {logDir !== null ? (
            <Group gap="xs">
              <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere" }}>
                {tr("Log folder: {path}", { path: logDir })}
              </Text>
              <Button
                size="compact-xs"
                variant="subtle"
                onClick={() => {
                  model.platform.revealPath(logDir).catch((failure: unknown) => model.status.error(failure));
                }}
              >
                {tr("Show")}
              </Button>
            </Group>
          ) : null}
        </Section>
      );
      break;
    case "proxy":
      content = (
        <Section title={tr("Proxy")}>
          <Switch
            label={tr("Connect through a proxy")}
            checked={draft.proxy.enabled}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.proxy.enabled = checked;
              }, ["proxy.host"]);
            }}
          />
          <SegmentedControl<ProxyKind>
            value={draft.proxy.kind}
            onChange={(value) =>
              change((next) => {
                next.proxy.kind = value;
              })
            }
            data={[
              { value: "http", label: "HTTP" },
              { value: "socks5", label: "SOCKS5" },
            ]}
            aria-label={tr("Proxy type")}
            w={200}
          />
          <Group align="flex-start" gap="sm">
            <TextInput
              label={tr("Host")}
              value={draft.proxy.host}
              onChange={(event) => {
                const value = event.currentTarget.value.trim();
                change((next) => {
                  next.proxy.host = value;
                }, ["proxy.host"]);
              }}
              error={errors["proxy.host"]}
              placeholder="proxy.example.com"
              w={260}
              spellCheck={false}
            />
            <NumberInput
              label={tr("Port")}
              value={Number.isNaN(draft.proxy.port) ? "" : draft.proxy.port}
              onChange={(value) =>
                change((next) => {
                  next.proxy.port = typeof value === "number" ? value : Number.NaN;
                }, ["proxy.port"])
              }
              allowDecimal={false}
              allowNegative={false}
              error={errors["proxy.port"]}
              w={110}
            />
          </Group>
          <TextInput
            label={tr("User name (optional)")}
            value={draft.proxy.username ?? ""}
            onChange={(event) => {
              const value = event.currentTarget.value;
              change((next) => {
                next.proxy.username = value.trim().length === 0 ? null : value;
              }, ["proxy.username"]);
            }}
            error={errors["proxy.username"]}
            w={260}
            autoComplete="off"
          />
          <ProxyPassword
            passwordSet={draft.proxy.password_ref !== null}
            onSaved={(ref) =>
              change((next) => {
                next.proxy.password_ref = ref;
              })
            }
          />
          <Textarea
            label={tr("Bypass the proxy for")}
            description={tr("One entry per line: a host name, \"*.example.com\", or a CIDR range such as 10.0.0.0/8.")}
            value={bypassText}
            onChange={(event) => {
              setBypassText(event.currentTarget.value);
              setErrors((current) => {
                const next = { ...current };
                delete next["proxy.bypass"];
                return next;
              });
            }}
            error={errors["proxy.bypass"]}
            rows={4}
            resize="vertical"
            spellCheck={false}
          />
        </Section>
      );
      break;
    case "sharing":
      content = (
        <Section title={tr("Sharing")}>
          <ShortenerSettings
            provider={draft.shortener.provider}
            tokenSet={draft.shortener.token_ref !== null}
            onProvider={(provider) =>
              change((next) => {
                next.shortener.provider = provider;
              })
            }
            onTokenSaved={(ref) =>
              change((next) => {
                next.shortener.token_ref = ref;
              })
            }
          />
        </Section>
      );
      break;
  }

  return (
    <Stack gap="sm">
      <div className="bb-settings">
        <nav className="bb-settings-nav" aria-label={tr("Settings pages")}>
          {PAGES.map((item) => (
            <NavLink
              key={item.id}
              component="button"
              type="button"
              label={item.label}
              active={page === item.id}
              onClick={() => setPage(item.id)}
              color={invalidPages.has(item.id) ? "red" : undefined}
              rightSection={invalidPages.has(item.id) ? <IconAlertCircle size={14} color="var(--mantine-color-red-6)" /> : null}
              aria-current={page === item.id ? "page" : undefined}
            />
          ))}
        </nav>
        <div className="bb-settings-page">{content}</div>
      </div>
      {saveError !== null ? (
        <Alert color="red" py="xs" role="alert" icon={<IconAlertCircle size={16} />}>
          {saveError}
        </Alert>
      ) : null}
      {hasErrors(errors) ? (
        <Text size="xs" c="red" role="alert">
          {tr("Some values are not valid. Pages with problems are marked.")}
        </Text>
      ) : null}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={close}>
          {tr("Cancel")}
        </Button>
        <Button variant="default" loading={busy} onClick={() => void apply(false)}>
          {tr("Apply")}
        </Button>
        <Button loading={busy} onClick={() => void apply(true)}>
          {tr("OK")}
        </Button>
      </Group>
    </Stack>
  );
}

/** Settings pages with OK / Cancel / Apply. Theme changes preview immediately. */
export function SettingsDialog({ onClose, initialPage }: { onClose: () => void; initialPage?: SettingsPage }) {
  const model = useModel();
  const saved = useSlice(model.settings.store, (state) => state.settings);
  const loaded = useSlice(model.settings.store, (state) => state.loaded);
  const loadError = useSlice(model.settings.store, (state) => state.loadError);
  const [reloading, setReloading] = useState(false);
  const close = () => {
    model.settings.setPreviewTheme(null);
    onClose();
  };
  const reload = async () => {
    setReloading(true);
    try {
      await model.settings.load();
    } catch (failure) {
      model.status.error(failure);
    } finally {
      setReloading(false);
    }
  };
  return (
    <AppModal opened onClose={close} title={tr("Settings")} size="xl" closeOnClickOutside={false}>
      {loaded ? (
        <SettingsForm saved={saved} onClose={onClose} initialPage={initialPage} />
      ) : (
        <Stack gap="sm">
          <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert">
            {tr("The settings could not be loaded{detail}", { detail: loadError ? `: ${loadError}` : "." })}
          </Alert>
          <Group justify="flex-end">
            <Button variant="default" onClick={close}>
              {tr("Close")}
            </Button>
            <Button loading={reloading} onClick={() => void reload()}>
              {tr("Try again")}
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
