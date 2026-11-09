/** 简体中文文案（sharing）。键是英文原文，值是中文译文。 */
export const zhSharing: Record<string, string> = {
  // ---- expiry.ts（第一步的键校验）----
  "Enter the key of the object to share.": "请输入要分享对象的键。",
  "Choose an object, not a folder.": "请选择对象，而不是文件夹。",

  // ---- SharingWizard.tsx ----
  "Sharing wizard: {bucket}": "共享向导：{bucket}",
  "Create link": "创建链接",
  Object: "对象",
  Access: "访问",
  Link: "链接",
  "Create a temporary link that gives anyone who has it access to one object in":
    "创建一个临时链接，任何持有它的人都可以访问",
  "Object key": "对象键",
  "Link to": "链接到",
  "Download (GET)": "下载（GET）",
  "Upload (PUT)": "上传（PUT）",
  "Link type": "链接类型",
  Unit: "单位",
  Seconds: "秒",
  Minutes: "分钟",
  Hours: "小时",
  Days: "天",
  "The link works until it expires, or until the access of the account that signed it ends.":
    "链接在过期前有效，或直到签署它的账户的访问结束为止。",
  "Upload with an HTTP PUT request to this link until then.": "在此之前，可向该链接发送 HTTP PUT 请求进行上传。",

  // ---- ShortenControl.tsx ----
  "URL shortening is off. Share links are never sent to a third party unless you choose a service.":
    "URL 缩短未启用。除非你选择了服务，分享链接绝不会发送给第三方。",
  "Choose a service...": "选择服务…",
  Shorten: "缩短",
  "Shorten sends the whole link, including its signature, to {name}, outside this machine.":
    "缩短会把整个链接（包括其签名）发送给 {name}，离开这台电脑。",
  "Short link": "短链接",
  "The short link keeps redirecting after the presigned link expires, but the target stops working.":
    "预签名链接过期后，短链接仍会继续重定向，但目标会停止工作。",

  // ---- ShortenerSettings.tsx（设置中的 Sharing 页）----
  "None (default)": "无（默认）",
  "A URL shortener is an outside service. When you press Shorten on a share link, the whole link, including the signature of a presigned link, is sent to it. Nothing is sent otherwise, and nothing is sent while this is set to None.":
    "URL 短链服务是外部服务。在分享链接上按「缩短」时，整个链接（包括预签名链接的签名）都会发送给它。其他情况下不会发送任何内容，设置为「无（默认）」时也不会发送。",
  "URL shortener": "URL 短链服务",
  "API token": "API 令牌",
  "A token is stored in the credential store.": "凭据存储中已存有一个令牌。",
  "No token is stored.": "没有存储令牌。",
  "Bitly needs a token.": "Bitly 需要 API 令牌。",
  "TinyURL works without a token.": "TinyURL 无需 API 令牌即可使用。",
  "Tokens are never shown again; changes here apply immediately.": "令牌不会再显示；此处的更改立即生效。",
  "New token": "新令牌",
  "Save token": "保存令牌",
  "Change token...": "更换令牌…",
  "Set token...": "设置令牌…",
  "Remove token": "移除令牌",
  "Shortener token removed.": "短链服务令牌已移除。",
  "Shortener token saved.": "短链服务令牌已保存。",
};
