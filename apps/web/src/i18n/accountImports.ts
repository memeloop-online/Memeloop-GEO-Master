import i18n from "./index";

const resources = {
  "zh-CN": {
    title: "批量导入账号",
    help: "导入账号后，逐个完成登录。",
    format:
      "UTF-8 CSV，最多 100 行、1 MiB。必填列：username,password；可选列：platform,display_alias,group_id。未填写的平台和资源组使用下方选择。",
    platform: "默认平台",
    group: "默认资源组",
    ungrouped: "不分组",
    file: "选择 CSV 文件",
    paste: "在此粘贴 CSV",
    pasteHelp:
      "此输入区会显示粘贴的凭据。读取后立即清空，只保留账号数量。文件或粘贴清单会替换尚未提交的账号。",
    prepare: "读取并清空清单",
    username: "用户名",
    password: "密码",
    alias: "显示别名（可选）",
    add: "添加一行",
    count: "已准备 {{count}} 个账号",
    submit: "导入账号",
    clear: "清空待导入账号",
    busy: "正在处理",
    fieldLimits:
      "请缩短超长字段：用户名最多 512 字节，密码 4096 字节，别名 160 字节，平台 64 字节（UTF-8）。",
    invalid:
      "无法读取清单。请检查表头、CSV 格式、必填字段和平台；最多 100 行、1 MiB。",
    failed: "暂时无法完成，请重试。",
    pendingHelp: "结果尚未确认。请重试导入或查看导入结果。",
    recoveryHelp: "已保存 {{saved}} / {{count}} 行，重新选择原清单以继续。",
    recoveryCount: "请重新选择原清单，需包含原顺序的 {{count}} 行。",
    recoveryChanged:
      "清单与已保存的行不一致。请清空本页输入，重新选择原清单并保持原顺序。",
    clearConfirm:
      "仅清除此页面的输入，已保存的账号不受影响。继续导入时仍需重新选择原清单。确定清空？",
    legacyHelp:
      "这里只显示已保存的行，无法确认原清单是否已全部导入。请先核对账号，避免重复导入。",
    recoveryStorage: "无法保存恢复信息。请允许此网站使用会话存储后重试。",
    reference: "导入编号",
    referenceHelp: "刷新后会恢复最近的导入结果，也可以输入其他导入编号。",
    load: "查看导入结果",
    results: "导入结果",
    row: "第 {{number}} 行",
    needs_login: "待登录",
    rowFailed: "导入失败，请重试此行或检查原清单。",
    retry: "重试此行",
    login: "打开登录窗口",
    errors: {
      unsupported_platform:
        "此平台暂不支持导入。请为此行选择可用平台后重新导入。",
      invalid_credentials: "用户名或密码不符合要求。请修正此行后重新导入。",
      group_not_found:
        "找不到此资源组。请检查资源组后重试，或修正此行后重新导入。",
    },
  },
  en: {
    title: "Import accounts",
    help: "Import accounts, then sign in to each one.",
    format:
      "UTF-8 CSV, up to 100 rows and 1 MiB. Required columns: username,password. Optional: platform,display_alias,group_id. Missing platform and group values use the selections below.",
    platform: "Default platform",
    group: "Default resource group",
    ungrouped: "No group",
    file: "Choose CSV file",
    paste: "Paste CSV here",
    pasteHelp:
      "Pasted credentials are visible in this input. Reading the list clears it and shows only the account count. A file or pasted list replaces accounts not yet submitted.",
    prepare: "Read and clear list",
    username: "Username",
    password: "Password",
    alias: "Display alias (optional)",
    add: "Add row",
    count: "{{count}} accounts prepared",
    submit: "Import prepared accounts",
    clear: "Clear prepared accounts",
    busy: "Working",
    fieldLimits:
      "Shorten oversized fields: username 512 bytes, password 4096 bytes, alias 160 bytes, platform 64 bytes maximum (UTF-8).",
    invalid:
      "Unable to read this list. Check the headers, CSV format, required fields and platforms. The limit is 100 rows and 1 MiB.",
    failed: "Unable to complete this action. Please try again.",
    pendingHelp:
      "The result is not confirmed. Retry the import or retrieve its results.",
    recoveryHelp:
      "{{saved}} of {{count}} rows saved. Select the original list again to continue.",
    recoveryCount:
      "Select the original list with all {{count}} rows in their original order.",
    recoveryChanged:
      "This list differs from saved rows. Clear the inputs on this page and select the original list in its original order.",
    clearConfirm:
      "Clear only the inputs on this page? Saved accounts remain. To continue this import, select the original list again.",
    legacyHelp:
      "Only saved rows are shown. The original list may be incomplete. Check existing accounts before importing again to avoid duplicates.",
    recoveryStorage:
      "Recovery information could not be saved. Allow session storage for this site and try again.",
    reference: "Import reference",
    referenceHelp:
      "Your latest import results return after refreshing. You can also enter another import reference.",
    load: "Retrieve import results",
    results: "Import results",
    row: "Row {{number}}",
    needs_login: "Needs login",
    rowFailed: "Import failed. Retry this row or check the original list.",
    retry: "Retry row",
    login: "Open login window",
    errors: {
      unsupported_platform:
        "This platform cannot be imported. Choose an available platform and import this row again.",
      invalid_credentials:
        "The username or password does not meet the requirements. Correct this row and import it again.",
      group_not_found:
        "The resource group was not found. Check the group and retry, or correct this row and import it again.",
    },
  },
};
for (const [language, translation] of Object.entries(resources)) {
  i18n.addResourceBundle(language, "accountImports", translation, true, true);
}
