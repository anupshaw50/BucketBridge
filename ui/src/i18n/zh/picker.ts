/** 简体中文文案（picker）。键是英文原文，值是中文译文。 */
export const zhPicker: Record<string, string> = {
  // ---- problem.ts（目标位置校验）----
  "Choose a bucket first.": "请先选择一个存储桶。",
  "Nothing is selected.": "未选择任何项目。",
  "be moved": "移动",
  "be copied": "复制",
  "A folder cannot be moved into itself or one of its subfolders.": "不能把文件夹移动到其自身或其子文件夹中。",
  "A folder cannot be copied into itself or one of its subfolders.": "不能把文件夹复制到其自身或其子文件夹中。",

  // ---- DestinationPicker.tsx ----
  "As set in Settings": "与设置中相同",
  Overwrite: "覆盖",
  Skip: "跳过",
  "Rename (name (1).ext)": "重命名（name (1).ext）",
  "Copy 1 item to": "复制 1 个项目到",
  "Move 1 item to": "移动 1 个项目到",
  "Copy {n} items to": "复制 {n} 个项目到",
  "Move {n} items to": "移动 {n} 个项目到",
  "Copy here": "复制到此处",
  "Move here": "移动到此处",
  "Destination account": "目标账户",
  "Account: {label}": "账户：{label}",
  "Between accounts the service copies the objects when it can; otherwise they go through this computer.":
    "跨账户时，服务在可行的情况下会直接复制对象；否则将通过这台电脑中转。",
  " Each source is deleted only after its copy was checked.": "每个源只会在其副本校验完成后才被删除。",
  "Destination path": "目标路径",
  "New folder name": "新文件夹名称",
  "Folder name": "文件夹名称",
  Destinations: "目标列表",
  "No folders here.": "这里没有文件夹。",
  "Destination:": "目标位置：",
  "none chosen": "未选择",
  "If an item already exists there": "如果目标处已有同名项目",
  "The copy could not be started.": "无法开始复制。",
  "The move could not be started.": "无法开始移动。",
};
