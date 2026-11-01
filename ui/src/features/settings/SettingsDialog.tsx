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
import type { ChecksumAlgorithm, LogLevel, ProxyKind, Settings, ThemeSetting } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
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
  { id: "general", label: "General" },
  { id: "interface", label: "Interface" },
  { id: "transfer", label: "Transfers" },
  { id: "advanced", label: "Advanced" },
  { id: "data_integrity", label: "Data integrity" },
  { id: "logging", label: "Logging" },
  { id: "proxy", label: "Proxy" },
  { id: "sharing", label: "Sharing" },
];

const CHECKSUMS: { value: ChecksumAlgorithm; label: string }[] = [
  { value: "crc32", label: "CRC32" },
  { value: "crc32c", label: "CRC32C" },
  { value: "crc64nvme", label: "CRC64NVME" },
  { value: "sha1", label: "SHA-1" },
  { value: "sha256", label: "SHA-256" },
  { value: "none", label: "None (only when the service requires one)" },
];

const LOG_LEVEL_OPTIONS: { value: LogLevel; label: string }[] = [
  { value: "error", label: "Errors only" },
  { value: "warning", label: "Warnings and errors" },
  { value: "information", label: "Everything (information, warnings, errors)" },
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
      model.status.info(value === null ? "Proxy password removed." : "Proxy password saved.");
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
        Password
      </Text>
      <Text size="sm" c="dimmed">
        {passwordSet ? "A password is stored in the credential store." : "No password is stored."} Passwords are never
        shown again; changes here apply immediately.
      </Text>
      {editing ? (
        <Group align="flex-end" gap="xs">
          <PasswordInput
            label="New proxy password"
            value={password}
            onChange={(event) => setPassword(event.currentTarget.value)}
            w={260}
            autoComplete="new-password"
            data-autofocus
          />
          <Button size="xs" loading={busy} disabled={password.length === 0} onClick={() => void store(password)}>
            Save password
          </Button>
          <Button size="xs" variant="default" onClick={() => setEditing(false)}>
            Cancel
          </Button>
        </Group>
      ) : (
        <Group gap="xs">
          <Button size="xs" variant="default" onClick={() => setEditing(true)}>
            {passwordSet ? "Change password..." : "Set password..."}
          </Button>
          {passwordSet ? (
            <Button size="xs" variant="subtle" color="red" loading={busy} onClick={() => void store(null)}>
              Remove password
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
      model.status.info("Settings saved.");
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
      const folder = await model.platform.pickFolder("Choose the default download folder");
      if (folder !== null) {
        change((next) => {
          next.general.default_download_dir = folder;
        }, ["general.default_download_dir"]);
      }
    } catch (failure) {
      model.status.error(failure, { notify: "The folder dialog could not be opened" });
    }
  };
  const invalidPages = new Set(Object.keys(errors).map((field) => pageOfField(field as SettingsField)));

  let content: ReactNode;
  switch (page) {
    case "general":
      content = (
        <Section title="General">
          <Switch
            label="Ask before deleting"
            description="Show a confirmation before objects and folders are deleted."
            checked={draft.general.confirm_delete}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.general.confirm_delete = checked;
              });
            }}
          />
          <Radio.Group
            label="Download folder"
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
              <Radio value="ask" label="Ask where to save every time" />
              <Radio value="fixed" label="Always download to this folder" />
            </Stack>
          </Radio.Group>
          {draft.general.default_download_dir !== null ? (
            <Group align="flex-start" gap="xs" wrap="nowrap">
              <TextInput
                aria-label="Default download folder"
                value={draft.general.default_download_dir}
                readOnly
                placeholder="No folder chosen"
                style={{ flex: 1 }}
                error={errors["general.default_download_dir"]}
              />
              <Button variant="default" leftSection={<IconFolderOpen size={14} />} onClick={() => void chooseDownloadDir()}>
                Browse...
              </Button>
            </Group>
          ) : null}
        </Section>
      );
      break;
    case "interface":
      content = (
        <Section title="Interface">
          <Stack gap={4}>
            <Text size="sm" fw={500}>
              Theme
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
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
              aria-label="Theme"
              w={300}
            />
          </Stack>
          <Switch
            label="Flat browsing"
            description="List every object below the open folder instead of one folder level."
            checked={draft.interface.flat_browsing}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.flat_browsing = checked;
              });
            }}
          />
          <Switch
            label="Show the transfer queue"
            checked={draft.interface.show_transfer_panel}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.show_transfer_panel = checked;
              });
            }}
          />
          <Switch
            label="Show the event log"
            checked={draft.interface.show_log_panel}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              change((next) => {
                next.interface.show_log_panel = checked;
              });
            }}
          />
        </Section>
      );
      break;
    case "transfer":
      content = (
        <Section title="Transfers">
          <RangedNumber
            field="transfer.max_concurrent_tasks"
            label="Tasks running at the same time"
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
            label="Parallel parts per task"
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
            label="Multipart threshold"
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
            label="Part size (grows automatically for very large files)"
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
            label="Limit bandwidth"
            description="One limit shared by all running transfers."
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
              label="Bandwidth limit"
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
            label="Retries per part or object"
            value={draft.transfer.retry_attempts}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.transfer.retry_attempts = value;
              }, ["transfer.retry_attempts"])
            }
          />
        </Section>
      );
      break;
    case "advanced":
      content = (
        <Section title="Advanced">
          <RangedNumber
            field="advanced.max_attempts"
            label="Request attempts (including the first)"
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
            label="Connection timeout"
            unit="seconds"
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
            label="Read timeout"
            unit="seconds"
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
            label="Objects per listing page"
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
        <Section title="Data integrity">
          <Select
            label="Upload checksum"
            description="Algorithm used to protect uploads end to end."
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
            label="Validate downloads"
            description="Compare checksums of downloaded data when the object has one."
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
        <Section title="Logging">
          <Select
            label="Record"
            description="Minimum level written to the event log."
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
            label="Write the log to a file"
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
            label="Maximum log file size"
            unit="MiB"
            value={draft.logging.max_file_mib}
            errors={errors}
            onChange={(value) =>
              change((next) => {
                next.logging.max_file_mib = value;
              }, ["logging.max_file_mib"])
            }
          />
          {logDir !== null ? (
            <Group gap="xs">
              <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere" }}>
                Log folder: {logDir}
              </Text>
              <Button
                size="compact-xs"
                variant="subtle"
                onClick={() => {
                  model.platform.revealPath(logDir).catch((failure: unknown) => model.status.error(failure));
                }}
              >
                Show
              </Button>
            </Group>
          ) : null}
        </Section>
      );
      break;
    case "proxy":
      content = (
        <Section title="Proxy">
          <Switch
            label="Connect through a proxy"
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
            aria-label="Proxy type"
            w={200}
          />
          <Group align="flex-start" gap="sm">
            <TextInput
              label="Host"
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
              label="Port"
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
            label="User name (optional)"
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
            label="Bypass the proxy for"
            description='One entry per line: a host name, "*.example.com", or a CIDR range such as 10.0.0.0/8.'
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
        <Section title="Sharing">
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
        <nav className="bb-settings-nav" aria-label="Settings pages">
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
          Some values are not valid. Pages with problems are marked.
        </Text>
      ) : null}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={close}>
          Cancel
        </Button>
        <Button variant="default" loading={busy} onClick={() => void apply(false)}>
          Apply
        </Button>
        <Button loading={busy} onClick={() => void apply(true)}>
          OK
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
    <AppModal opened onClose={close} title="Settings" size="xl" closeOnClickOutside={false}>
      {loaded ? (
        <SettingsForm saved={saved} onClose={onClose} initialPage={initialPage} />
      ) : (
        <Stack gap="sm">
          <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert">
            The settings could not be loaded{loadError ? `: ${loadError}` : "."}
          </Alert>
          <Group justify="flex-end">
            <Button variant="default" onClick={close}>
              Close
            </Button>
            <Button loading={reloading} onClick={() => void reload()}>
              Try again
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
