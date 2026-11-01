import { Button, Menu } from "@mantine/core";
import { IconChevronDown, IconCloudOff, IconFilter, IconShieldCheck, IconStethoscope, IconTools } from "@tabler/icons-react";
import type { ReactNode } from "react";
import { useModel, useSlice } from "../../app/context";
import { bucketsPaneContext } from "../menus/context";
import type { ActionId } from "../menus/ids";
import { actionState, runAction } from "../menus/registry";

const TOOLS: readonly { id: ActionId; label: string; icon: ReactNode }[] = [
  { id: "tools.diagnostics", label: "Diagnostics...", icon: <IconStethoscope size={14} /> },
  { id: "tools.security-scan", label: "Security scan...", icon: <IconShieldCheck size={14} /> },
  { id: "tools.multipart", label: "Uncompleted uploads...", icon: <IconCloudOff size={14} /> },
  { id: "list.filter", label: "Saved filters...", icon: <IconFilter size={14} /> },
];

/** Toolbar menu of the tools: the registry actions of diagnostics, uncompleted multipart uploads and saved filters. */
export function ToolsMenu() {
  const model = useModel();
  // The state of an entry depends on the account, so the menu renders again when it changes.
  useSlice(model.session.store, (state) => state.activeAccountId);
  return (
    <Menu position="bottom-start" withinPortal>
      <Menu.Target>
        <Button
          variant="subtle"
          size="xs"
          leftSection={<IconTools size={15} />}
          rightSection={<IconChevronDown size={12} />}
        >
          Tools
        </Button>
      </Menu.Target>
      <Menu.Dropdown>
        {TOOLS.map(({ id, label, icon }) => {
          const state = actionState(id, bucketsPaneContext(model));
          return (
            <Menu.Item
              key={id}
              leftSection={icon}
              disabled={!state.visible || state.disabled !== null}
              onClick={() => runAction(id, bucketsPaneContext(model))}
            >
              {label}
            </Menu.Item>
          );
        })}
      </Menu.Dropdown>
    </Menu>
  );
}
