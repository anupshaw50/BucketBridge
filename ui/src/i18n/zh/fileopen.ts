/** 简体中文文案（fileopen）。键是英文原文，值是中文译文。 */
export const zhFileopen: Record<string, string> = {
  // ---- OpenWithDialog.tsx（打开方式）----
  "Open with": "打开方式",
  "Recent applications": "最近使用的应用程序",
  "Choose application…": "选择应用程序…",
  "Choose an application": "选择应用程序",
  "Opening {key} with {app}.": "正在用 {app} 打开 {key}。",

  // ---- WebsiteRedirectDialog.tsx（网站重定向）----
  "Website redirect": "网站重定向",
  "Redirect location": "重定向目标",
  "Applies when the bucket serves this object as a static website. Leave empty to remove the redirect.":
    "当存储桶以静态网站方式提供此对象时生效。留空表示移除重定向。",
  Clear: "清除",
  "The redirect could not be saved.": "无法保存重定向。",
  "Cleared the website redirect of {key}.": "已清除 {key} 的网站重定向。",
  "Website redirect of {key} set.": "已设置 {key} 的网站重定向。",

  // ---- helpers.ts（重定向校验）----
  "Use a path that starts with / or an http:// or https:// URL.": "请使用以 / 开头的路径，或 http:// 或 https:// 链接。",
};
