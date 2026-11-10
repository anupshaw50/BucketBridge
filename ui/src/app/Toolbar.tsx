import { ActionIcon, Badge, Button, Divider, Group, Menu, Text, Tooltip } from "@mantine/core";
import {
  IconBucket,
  IconCheck,
  IconChevronDown,
  IconClipboard,
  IconCopy,
  IconCut,
  IconDownload,
  IconEdit,
  IconExternalLink,
  IconEye,
  IconFolderPlus,
  IconHistory,
  IconInfoCircle,
  IconLayoutBottombar,
  IconLink,
  IconListDetails,
  IconListTree,
  IconLock,
  IconLockOpen,
  IconRefresh,
  IconSelectAll,
  IconSettings,
  IconTrash,
  IconTransfer,
  IconUpload,
  IconUsers,
  IconHelpCircle,
  IconStar,
  IconTableExport,
} from "@tabler/icons-react";
import type { ReactNode } from "react";
import { bucketsPaneContext, filesPaneContext } from "../features/menus/context";
import { ContextMenu } from "../features/menus/ContextMenu";
import type { ActionId } from "../features/menus/ids";
import { bindingFor } from "../features/menus/keymap";
import { actionState, runAction } from "../features/menus/registry";
import { accountMenu } from "../features/menus/surfaces";
import { useContextMenu } from "../features/menus/useContextMenu";
import { downloadSelected, toggleFlatBrowsing, togglePanel } from "../features/objects/actions";
import { FilterMenu } from "../features/tools/FilterControls";
import { ToolsMenu } from "../features/tools/ToolsMenu";
import { tr } from "../i18n";
import { groupAccounts } from "../lib/accountGroups";
import { formatBinding } from "../lib/keys";
import { useModel, useSlice } from "./context";

/** Shortcut text of an action in the object list, as the context menus show it. */
function shortcutOf(id: ActionId): string | undefined {
  const binding = bindingFor(id, "files");
  return binding === null ? undefined : formatBinding(binding);
}

interface ToolButtonProps {
  label: string;
  shortcut?: string;
  icon: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  pressed?: boolean;
  /** Tooltip text explaining why the button is disabled. */
  reason?: string | null;
}

function ToolButton({ label, shortcut, icon, onClick, disabled = false, pressed, reason }: ToolButtonProps) {
  const tip = shortcut ? `${label} (${shortcut})` : label;
  return (
    <Tooltip label={disabled && reason ? `${tip}: ${reason}` : tip} openDelay={300}>
      <ActionIcon
        variant={pressed ? "light" : "subtle"}
        color={pressed ? undefined : "gray"}
        size="md"
        aria-label={label}
        aria-pressed={pressed}
        disabled={disabled}
        onClick={onClick}
      >
        {icon}
      </ActionIcon>
    </Tooltip>
  );
}

function AccountSwitcher() {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const folders = useSlice(model.session.store, (state) => state.folders);
  const activeId = useSlice(model.session.store, (state) => state.activeAccountId);
  const defaultId = useSlice(model.session.store, (state) => state.defaultAccountId);
  const active = accounts.find((account) => account.id === activeId);
  const groups = groupAccounts(accounts, folders);
  const menu = useContextMenu();
  return (
    <>
      <Menu position="bottom-start" withinPortal>
        <Menu.Target>
          <Button
            variant="default"
            size="xs"
            leftSection={<IconUsers size={14} />}
            rightSection={<IconChevronDown size={12} />}
            aria-label={active ? tr("Account: {label}", { label: active.label }) : tr("Choose an account")}
            maw={220}
            onContextMenu={(event) => {
              if (active) {
                const built = accountMenu(model, active);
                menu.open(event, built.items, built.label);
              }
            }}
          >
            <Text size="xs" className="bb-ellipsis" span>
              {active ? active.label : tr("No account")}
            </Text>
          </Button>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Label>{tr("Accounts")}</Menu.Label>
          {accounts.length === 0 ? <Menu.Item disabled>{tr("No accounts yet")}</Menu.Item> : null}
          {groups.flatMap((group) => [
            group.folder !== null && group.accounts.length > 0 ? (
              <Menu.Label key={`folder:${group.folder}`}>{group.folder}</Menu.Label>
            ) : group.folder === null && folders.length > 0 ? (
              <Menu.Label key="folder:none">{tr("Not in a folder")}</Menu.Label>
            ) : null,
            ...group.accounts.map((account) => (
            <Menu.Item
              key={account.id}
              leftSection={account.id === activeId ? <IconCheck size={14} /> : <span style={{ width: 14 }} />}
              rightSection={
                account.id === defaultId ? (
                  <Badge size="xs" variant="light" leftSection={<IconStar size={10} />}>
                    {tr("Default")}
                  </Badge>
                ) : null
              }
              onClick={() => model.session.setActive(account.id)}
            >
              {account.label}
            </Menu.Item>
            )),
          ])}
          <Menu.Divider />
          <Menu.Item onClick={() => model.dialogs.open({ kind: "add-account" })}>{tr("Add account...")}</Menu.Item>
          <Menu.Item onClick={() => model.dialogs.open({ kind: "import-aws" })}>{tr("Import from AWS config...")}</Menu.Item>
          <Menu.Item onClick={() => model.dialogs.open({ kind: "accounts" })}>{tr("Manage accounts...")}</Menu.Item>
        </Menu.Dropdown>
      </Menu>
      <ContextMenu menu={menu.state} onClose={menu.close} />
    </>
  );
}

/** The bucket menu of the toolbar: the registry actions of the bucket that is open in the object list. */
function BucketActions() {
  const model = useModel();
  const activeAccountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const openBucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const list = useSlice(model.browser.store, (state) => state.bucketList);
  const bucket = list.buckets.find((item) => item.name === openBucket) ?? null;
  const context = () => bucketsPaneContext(model, bucket === null ? [] : [bucket]);
  const run = (id: ActionId) => {
    runAction(id, context());
  };
  const state = (id: ActionId) => actionState(id, context());
  const policy = state("bucket.policy");
  const removeExternal = state("bucket.remove-external");
  return (
    <Menu position="bottom-start" withinPortal>
      <Menu.Target>
        <Tooltip label={tr("Buckets")} openDelay={300}>
          <ActionIcon variant="subtle" color="gray" size="md" aria-label={tr("Bucket actions")} disabled={activeAccountId === null}>
            <IconBucket size={17} />
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Item onClick={() => run("bucket.new")}>{tr("New bucket...")}</Menu.Item>
        {/* The dialog opens on its first tab, the one of the policy entry of the Properties submenu. */}
        <Menu.Item disabled={!policy.visible || policy.disabled !== null} onClick={() => run("bucket.policy")}>
          {bucket === null ? tr("Bucket properties...") : tr("Properties of {name}...", { name: bucket.name })}
        </Menu.Item>
        <Menu.Item onClick={() => run("bucket.add-external")}>{tr("Add external bucket...")}</Menu.Item>
        {removeExternal.visible && bucket !== null ? (
          <Menu.Item disabled={removeExternal.disabled !== null} onClick={() => run("bucket.remove-external")}>
            {tr("Remove external bucket {name}...", { name: bucket.name })}
          </Menu.Item>
        ) : null}
        <Menu.Divider />
        <Menu.Item color="red" disabled={bucket === null || activeAccountId === null} onClick={() => run("bucket.delete")}>
          {bucket === null ? tr("Delete bucket...") : tr("Delete bucket {name}...", { name: bucket.name })}
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}

/** Main toolbar. Every control maps to a command of the IPC contract. */
export function Toolbar() {
  const model = useModel();
  const accountId = useSlice(model.browser.store, (state) => state.listing.accountId);
  const bucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const prefix = useSlice(model.browser.store, (state) => state.listing.prefix);
  const status = useSlice(model.browser.store, (state) => state.listing.status);
  const selection = useSlice(model.browser.store, (state) => state.selection);
  const visibleCount = useSlice(model.browser.store, (state) => state.visibleKeys.length);
  const clipboard = useSlice(model.clipboard, (state) => state.contents);
  const settings = useSlice(model.settings.store, (state) => state.settings.interface);
  const settingsLoaded = useSlice(model.settings.store, (state) => state.loaded);
  const notLoaded = settingsLoaded ? null : tr("the settings are not loaded yet");
  const security = useSlice(model.session.store, (state) => state.security);
  const activeAccountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const ready = status === "ready" && bucket !== null;
  // The toolbar, the context menus and the keyboard run the same registry actions on the same kind of context.
  // The values below are read so that the toolbar renders again when the selection (also when another single
  // row is selected), the clipboard, the open folder or the number of listed items changes.
  void [accountId, prefix, selection, visibleCount, clipboard];
  const pane = filesPaneContext(model);
  const run = (id: ActionId) => {
    runAction(id, filesPaneContext(model));
  };
  /** Props of a toolbar button that is the twin of a registry action. */
  const twin = (id: ActionId, options: { needsList?: boolean } = {}) => {
    const state = actionState(id, pane);
    const needsList = options.needsList ?? true;
    return {
      shortcut: shortcutOf(id),
      disabled: (needsList && !ready) || !state.visible || state.disabled !== null,
      reason: state.visible ? state.disabled : null,
      onClick: () => run(id),
    };
  };
  const locked = security !== null && security.master_password_set && !security.unlocked;

  const lockOrUnlock = async () => {
    try {
      if (locked) {
        model.prompts.showUnlock();
      } else {
        await model.raw.lock();
        await model.session.refreshSecurity();
        model.status.info(tr("Secrets locked. Enter the master password to use the accounts again."));
      }
    } catch (error) {
      model.status.error(error, { notify: tr("Locking failed") });
    }
  };

  return (
    <header className="bb-toolbar" role="toolbar" aria-label={tr("Main toolbar")}>
      <AccountSwitcher />
      <Divider orientation="vertical" />
      <Menu position="bottom-start" withinPortal>
        <Menu.Target>
          <Button
            variant="filled"
            size="xs"
            leftSection={<IconUpload size={15} />}
            rightSection={<IconChevronDown size={12} />}
            disabled={!ready}
          >
            {tr("Upload")}
          </Button>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Item onClick={() => run("upload.files")}>{tr("Upload files...")}</Menu.Item>
          <Menu.Item onClick={() => run("upload.folder")}>{tr("Upload folder...")}</Menu.Item>
        </Menu.Dropdown>
      </Menu>
      <Menu position="bottom-start" withinPortal>
        <Menu.Target>
          <Button
            variant="default"
            size="xs"
            leftSection={<IconDownload size={15} />}
            rightSection={<IconChevronDown size={12} />}
            disabled={!ready || !actionState("item.download", pane).visible}
          >
            {tr("Download")}
          </Button>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Item onClick={() => void downloadSelected(model, false)}>{tr("Download")}</Menu.Item>
          <Menu.Item onClick={() => run("item.download")}>{tr("Download to...")}</Menu.Item>
        </Menu.Dropdown>
      </Menu>
      <ToolButton label={tr("New folder")} icon={<IconFolderPlus size={17} />} {...twin("folder.new")} />
      <div className="bb-tgroup">
      <ToolButton label={tr("Copy")} icon={<IconCopy size={17} />} {...twin("clipboard.copy")} />
      <ToolButton label={tr("Cut")} icon={<IconCut size={17} />} {...twin("clipboard.cut")} />
      <ToolButton label={tr("Paste")} icon={<IconClipboard size={17} />} {...twin("clipboard.paste")} />
      <ToolButton label={tr("Rename")} icon={<IconEdit size={17} />} {...twin("item.rename")} />
      <ToolButton label={tr("Delete")} icon={<IconTrash size={17} />} {...twin("item.delete")} />
      </div>
      <div className="bb-tgroup">
      <ToolButton label={tr("Open")} icon={<IconExternalLink size={17} />} {...twin("open")} />
      <ToolButton label={tr("Properties")} icon={<IconInfoCircle size={17} />} {...twin("item.properties")} />
      <ToolButton label={tr("Share link")} icon={<IconLink size={17} />} {...twin("item.share")} />
      <ToolButton label={tr("Export listing")} icon={<IconTableExport size={17} />} {...twin("file.export")} />
      <ToolButton label={tr("Preview")} icon={<IconEye size={17} />} {...twin("file.preview")} />
      <ToolButton label={tr("Versions")} icon={<IconHistory size={17} />} {...twin("item.versions")} />
      </div>
      <div className="bb-tgroup">
      <Menu position="bottom-start" withinPortal>
        <Menu.Target>
          <Tooltip label={tr("Selection")} openDelay={300}>
            <ActionIcon variant="subtle" color="gray" size="md" aria-label={tr("Selection")} disabled={!ready}>
              <IconSelectAll size={17} />
            </ActionIcon>
          </Tooltip>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Item onClick={() => run("list.select-all")}>
            {tr("Select all ({shortcut})", { shortcut: shortcutOf("list.select-all") ?? "" })}
          </Menu.Item>
          <Menu.Item onClick={() => run("list.invert")}>
            {tr("Invert selection ({shortcut})", { shortcut: shortcutOf("list.invert") ?? "" })}
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
      <ToolButton
        label={tr("Flat browsing")}
        icon={<IconListTree size={17} />}
        pressed={settings.flat_browsing}
        disabled={!settingsLoaded}
        reason={notLoaded}
        onClick={() => void toggleFlatBrowsing(model)}
      />
      <FilterMenu disabled={!ready} />
      <ToolButton label={tr("Refresh")} icon={<IconRefresh size={17} />} {...twin("list.refresh", { needsList: false })} />
      </div>
      <BucketActions />
      <Button
        variant="subtle"
        color="gray"
        size="xs"
        leftSection={<IconTransfer size={15} />}
        disabled={activeAccountId === null}
        onClick={() => run("sync.open")}
      >
        {tr("Sync")}
      </Button>
      <ToolsMenu />
      <Group gap={6} ml="auto" wrap="nowrap">
        <div className="bb-tgroup">
        <ToolButton
          label={tr("Transfer queue")}
          icon={<IconLayoutBottombar size={17} />}
          pressed={settings.show_transfer_panel}
          disabled={!settingsLoaded}
          reason={notLoaded}
          onClick={() => void togglePanel(model, "transfers")}
        />
        <ToolButton
          label={tr("Event log")}
          icon={<IconListDetails size={17} />}
          pressed={settings.show_log_panel}
          disabled={!settingsLoaded}
          reason={notLoaded}
          onClick={() => void togglePanel(model, "log")}
        />
        </div>
        {security?.master_password_set ? (
          <ToolButton
            label={locked ? tr("Unlock secrets") : tr("Lock secrets")}
            icon={locked ? <IconLockOpen size={17} /> : <IconLock size={17} />}
            onClick={() => void lockOrUnlock()}
          />
        ) : null}
        <ToolButton label={tr("Accounts")} icon={<IconUsers size={17} />} onClick={() => model.dialogs.open({ kind: "accounts" })} />
        <ToolButton label={tr("Settings")} icon={<IconSettings size={17} />} onClick={() => model.dialogs.open({ kind: "settings" })} />
        <ToolButton label={tr("About")} icon={<IconHelpCircle size={17} />} onClick={() => model.dialogs.open({ kind: "about" })} />
      </Group>
    </header>
  );
}
