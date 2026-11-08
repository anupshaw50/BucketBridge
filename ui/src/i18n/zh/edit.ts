/** 简体中文文案（edit：就地编辑与编辑冲突）。键是英文原文，值是中文译文。 */
export const zhEdit: Record<string, string> = {
  // ---- edit/EditingDialog.tsx（编辑面板）----
  Editing: "正在编辑",
  "Each save of the local copy is uploaded to the object. Stop editing to upload a pending save and remove the copy.":
    "本地副本的每次保存都会上传到对象。选择「停止编辑」会先上传待保存的内容并删除副本。",
  "No objects are being edited.": "没有正在编辑中的对象。",
  "Resolve…": "处理…",
  "Stop": "停止",
  "Stop editing {key}": "停止编辑 {key}",

  // ---- edit/EditConflictDialog.tsx（编辑冲突）----
  "Edit conflict": "编辑冲突",
  "Overwrite the object": "覆盖该对象",
  "{name} was deleted. Upload your version and create it again?": "{name} 已被删除。要上传你的版本并重新创建它吗？",
  "Replace the current {name} with your version? The change made on the server is lost.":
    "要用你的版本替换当前的 {name} 吗？在服务器上所做的修改将丢失。",
  "Discard your changes": "放弃你的修改",
  "Drop your saved changes to {name} and stop editing? The local copy is deleted.":
    "要放弃对 {name} 已保存的修改并停止编辑吗？本地副本将被删除。",
  Discard: "放弃",
  "Saved your version as {key}.": "你的版本已保存为 {key}。",
  "Uploaded your version of {key}.": "已上传 {key} 中你的版本。",
  "Discarded your changes to {key}.": "已放弃你对 {key} 的修改。",
  "{name} was deleted on the server after you opened it.": "在你打开之后，{name} 已在服务器上被删除。",
  "{name} was changed on the server after you opened it.": "在你打开之后，{name} 已在服务器上被修改。",
  "Your last save was not uploaded.": "你最近一次保存的内容没有上传。",
  "Save as a copy": "另存为副本",
  "Uploads your version as {key} and stops editing. The object stays as it is.":
    "将你的版本上传为 {key} 并停止编辑。原对象保持不变。",
  "Creates the object again from your version.": "用你的版本重新创建该对象。",
  "Replaces the object on the server with your version.": "用你的版本替换服务器上的该对象。",
  "Discard my changes": "放弃我的修改",
  "Decide later": "稍后决定",
};
