import { ActionIcon, Group, Text, Tooltip } from "@mantine/core";
import { IconBookmark, IconPencil, IconTrash } from "@tabler/icons-react";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { removeBookmark, showRenameBookmark } from "./actions";

/** The saved locations above the bucket tree: click to open, pencil to rename, bin to delete. Hidden while there are none. */
export function BookmarksSection() {
  const model = useModel();
  const items = useSlice(model.bookmarks.store, (state) => state.items);
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  if (items.length === 0) {
    return null;
  }
  return (
    <section className="bb-bookmarks" aria-label={tr("Bookmarks")}>
      <Text size="xs" fw={600} c="dimmed" px="xs" py={4}>
        {tr("Bookmarks")}
      </Text>
      <ul className="bb-bookmark-list">
        {items.map((bookmark) => {
          const account = accounts.find((item) => item.id === bookmark.account_id);
          return (
            <li key={bookmark.id} className="bb-bookmark-row">
              <button
                type="button"
                className="bb-bookmark-open"
                title={`${bookmark.bucket}/${bookmark.prefix}${account ? ` (${account.label})` : ""}`}
                onClick={() => {
                  void model.bookmarks.open(bookmark).catch((error: unknown) => model.status.error(error));
                }}
              >
                <IconBookmark size={14} aria-hidden />
                <span className="bb-ellipsis">{bookmark.label}</span>
              </button>
              <Group gap={2} wrap="nowrap" className="bb-bookmark-tools">
                <Tooltip label={tr("Rename bookmark")}>
                  <ActionIcon
                    variant="subtle"
                    color="gray"
                    size="xs"
                    aria-label={tr("Rename bookmark {label}", { label: bookmark.label })}
                    onClick={() => showRenameBookmark(model, bookmark)}
                  >
                    <IconPencil size={12} />
                  </ActionIcon>
                </Tooltip>
                <Tooltip label={tr("Delete bookmark")}>
                  <ActionIcon
                    variant="subtle"
                    color="gray"
                    size="xs"
                    aria-label={tr("Delete bookmark {label}", { label: bookmark.label })}
                    onClick={() => void removeBookmark(model, bookmark)}
                  >
                    <IconTrash size={12} />
                  </ActionIcon>
                </Tooltip>
              </Group>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
