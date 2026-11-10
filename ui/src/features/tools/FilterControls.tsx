import { ActionIcon, Badge, CloseButton, Menu, Tooltip } from "@mantine/core";
import { IconCheck, IconFilter, IconFilterOff } from "@tabler/icons-react";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { describeFilter } from "../../lib/filters";
import { filesPaneContext } from "../menus/context";
import { runAction } from "../menus/registry";

/** Toolbar menu: pick a saved filter for the object list, clear it, or manage the saved filters. */
export function FilterMenu({ disabled }: { disabled: boolean }) {
  const model = useModel();
  const saved = useSlice(model.filters.store, (state) => state.saved);
  const loadError = useSlice(model.filters.store, (state) => state.error);
  const active = useSlice(model.browser.store, (state) => state.filter);
  return (
    <Menu position="bottom-start" withinPortal onOpen={() => void model.filters.reload()}>
      <Menu.Target>
        <Tooltip label={tr("Saved filters")} openDelay={300}>
          <ActionIcon
            variant={active === null ? "subtle" : "light"}
            color={active === null ? "gray" : undefined}
            size="md"
            aria-label={tr("Apply a saved filter")}
            disabled={disabled}
          >
            <IconFilter size={17} />
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>{tr("Saved filters")}</Menu.Label>
        {loadError !== null ? <Menu.Item disabled>{tr("The filters could not be loaded")}</Menu.Item> : null}
        {loadError === null && saved.length === 0 ? <Menu.Item disabled>{tr("No saved filters yet")}</Menu.Item> : null}
        {saved.map((filter) => {
          const applied = active?.name.toLowerCase() === filter.name.toLowerCase();
          return (
            <Menu.Item
              key={filter.name}
              leftSection={applied ? <IconCheck size={14} /> : <span style={{ width: 14 }} />}
              onClick={() => model.filters.apply(filter.name)}
              aria-label={applied ? tr("{name} (applied)", { name: filter.name }) : filter.name}
              title={describeFilter(filter.filter)}
            >
              {filter.name}
            </Menu.Item>
          );
        })}
        <Menu.Divider />
        <Menu.Item
          leftSection={<IconFilterOff size={14} />}
          disabled={active === null}
          onClick={() => model.filters.clearActive()}
        >
          {tr("Clear saved filter")}
        </Menu.Item>
        <Menu.Item onClick={() => runAction("list.filter", filesPaneContext(model, []))}>{tr("Manage filters...")}</Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}

/** Shows the saved filter in effect next to the item count, with a button that removes it. */
export function ActiveFilterChip() {
  const model = useModel();
  const filter = useSlice(model.browser.store, (state) => state.filter);
  if (filter === null) {
    return null;
  }
  return (
    <Tooltip label={describeFilter(filter.spec)} openDelay={300} multiline maw={360}>
      <Badge
        variant="light"
        size="lg"
        tt="none"
        maw={280}
        rightSection={<CloseButton size="xs" aria-label={tr("Clear saved filter")} onClick={() => model.filters.clearActive()} />}
      >
        {tr("Filter: {name}", { name: filter.name })}
      </Badge>
    </Tooltip>
  );
}
