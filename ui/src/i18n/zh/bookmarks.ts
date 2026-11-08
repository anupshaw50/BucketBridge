/** 简体中文文案（bookmarks）。键是英文原文，值是中文译文。 */
export const zhBookmarks: Record<string, string> = {
  // ---- BookmarkDialog.tsx（添加、重命名书签）----
  "Enter a name for the bookmark.": "请输入书签名称。",
  "Use at most {n} characters.": "最多只能使用 {n} 个字符。",
  "Bookmark added.": "已添加书签。",
  "Bookmark renamed.": "已重命名书签。",
  "Rename bookmark": "重命名书签",
  "Add bookmark": "添加书签",
  Rename: "重命名",
  Name: "名称",
  "Defaults to the location": "默认为该位置",

  // ---- BookmarksSection.tsx（书签区）----
  Bookmarks: "书签",
  "Rename bookmark {label}": "重命名书签 {label}",
  "Delete bookmark {label}": "删除书签 {label}",

  // ---- actions.ts（删除书签）----
  Delete: "删除",
  "Delete bookmark": "删除书签",
  "Delete the bookmark \"{label}\"?\n\nOnly the shortcut is removed; the bucket and its objects are not changed.":
    "要删除书签「{label}」吗？\n\n只会删除这个快捷方式，存储桶及其对象不会受影响。",
  "Bookmark deleted.": "已删除书签。",
  "The bookmark could not be deleted": "无法删除该书签",
};
