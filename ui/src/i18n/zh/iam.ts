/** 简体中文文案（iam）。键是英文原文，值是中文译文。 */
export const zhIam: Record<string, string> = {
  // ---- SecretPanel.tsx（新访问密钥的一次性展示）----
  "Copy the secret access key now": "请立即复制秘密访问密钥",
  "This is the only time the secret access key is shown. It is not stored anywhere and cannot be recovered; if you lose it, create a new key.":
    "秘密访问密钥只会显示这一次。它不会保存在任何地方，也无法找回；如果丢失，请创建新密钥。",
  "Copy secret": "复制秘密访问密钥",
  "I have saved it": "我已保存",

  // ---- UserPanel.tsx（用户面板：删除用户）----
  "Delete user": "删除用户",
  "Delete user...": "删除用户…",
  "Delete {user} with its access keys, policies, login profile and group memberships? This cannot be undone.":
    "要删除 {user} 及其访问密钥、策略、登录配置文件和组成员身份吗？此操作无法撤销。",
  "User {user} deleted.": "已删除用户 {user}。",
  failed: "失败",
  "The user was not deleted.": "未能删除该用户。",

  // ---- UserPanel.tsx（托管策略与内联策略）----
  "Managed policies": "托管策略",
  Detach: "分离",
  "Attach a policy": "附加策略",
  "Choose a managed policy": "选择一个托管策略",
  Attach: "附加",
  "Inline policies": "内联策略",
  "Delete inline policy": "删除内联策略",
  "Delete the inline policy {name} of {user}?": "要删除 {user} 的内联策略 {name} 吗？",
  "Delete policy": "删除策略",
  "Policy name": "策略名称",
  "Policy document (JSON)": "策略文档（JSON）",
  "Save inline policy": "保存内联策略",

  // ---- UserPanel.tsx（访问密钥）----
  "Create access key": "创建访问密钥",
  "Delete...": "删除…",
  "Delete access key": "删除访问密钥",
  "Delete the access key {key}? Anything that uses it stops working.":
    "要删除访问密钥 {key} 吗？所有使用它的程序都将停止工作。",
  "Delete key": "删除密钥",
  "No access keys.": "没有访问密钥。",
  "Activate": "启用",
  "Deactivate": "停用",

  // ---- UserPanel.tsx（控制台登录配置文件）----
  "Console login profile": "控制台登录配置文件",
  "Password set": "已设置密码",
  "Password set (reset required at next sign-in)": "已设置密码（下次登录时需重置）",
  "Delete login profile": "删除登录配置文件",
  "Remove the console password of {user}?": "要移除 {user} 的控制台密码吗？",
  "Delete profile": "删除配置文件",
  "Console password": "控制台密码",
  "Require a new password at the next sign-in": "下次登录时要求设置新密码",
  "Create login profile": "创建登录配置文件",

  // ---- IamDialog.tsx（用户列表与主对话框）----
  "IAM manager": "IAM 管理器",
  "Add or choose an account first.": "请先添加或选择一个账户。",
  "IAM is not available for this endpoint": "此端点不支持 IAM",
  "New user": "新用户",
  "Create": "创建",
  "No users.": "没有用户。",
  "Choose a user.": "请选择一个用户。",
  "Refresh list": "刷新列表",
};
