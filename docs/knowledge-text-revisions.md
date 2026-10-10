# 企业知识文本修订实施契约

本文件细化冻结规格中的来源版本管理，不替代产品计划。资料编辑复用 Tiptap/ProseMirror；保存同一来源的子版本，不以重新导入创建另一来源。

## 接口与版本

`POST /api/v1/knowledge/sources/{source_id}/versions` 使用现有项目、身份与 CSRF 校验，要求 `If-Match` 来源 revision 和 `Idempotency-Key`。严格请求体只包含 `base_version_id`、`media_type`、`text`。支持 `text/plain`、`text/markdown`，非空白且不超过现有 256 KiB 内联文本限制；保存原始 UTF-8 文本，不偷偷裁剪。

成功返回 201 与不可变回执 `{source, source_version, knowledge_release}`。同键同请求在检查当前版本和解析状态之前重放原回执；同键异请求返回 `idempotency_conflict`。新写入要求来源处于 active、revision 匹配、base 是当前版本，否则返回 `source_revision_conflict`。跨项目或跨来源引用返回 404。

版本新增 `representation: original | authored_text`，旧数据默认 original。新子版本指向 base，版本号在来源锁内递增，哈希来自新文本；不继承旧二进制对象引用。保留来源名称、用途、定位信息以及历史原件、chunks、release 和发布引用。新 chunks 使用文本定位，不能伪装为旧 PDF 页或 Office 单元格证据。

`GET /knowledge/sources/{source_id}/versions/{version_id}/content` 返回 `{source_version_id, representation, media_type, text, text_basis}`。修订正文为 exact；原文本对象只有经过字节校验后才能标记 exact；从有序 chunks 重建的草稿必须标为 extracted。大文档不静默截断后保存。

## 持久化与并发

新增迁移，不改历史迁移。修订正文保存于作用域明确、按版本唯一的 authored-text 表，通过同来源复合外键绑定版本。扩展持久回执 action；幂等目标由作用域与请求键确定，不能只用来源 ID。

事务顺序：幂等回执锁 → 来源行锁 → 版本及解析状态检查 → 新版本/正文/chunks → 更新来源指针及 revision → 现有项目 release 锁 → 完整 release、回执和 outbox → 提交。调用现有 release 构造逻辑，不能只收录本次编辑来源，也不能反转既有来源锁与 release 锁顺序。

同来源存在 queued/running PDF/Office 解析时，新修订返回 `source_parse_in_progress`。收紧旧原件重试：已存在后继当前版本时不能重试旧版本覆盖它；保留无成功当前版本时的合法失败重试。检查、重试与完成共用来源锁。内存实现保持同一知识状态写锁，且与公开内容证据读 guard 兼容。

事件只记录标识与哈希，不含正文。故障必须回滚所有新版本、指针、release、回执和事件。

## UI 与 AI

来源详情分开显示原件证据和可编辑正文；历史版本只读。原件提取草稿保存后明确显示为编辑派生版本。复用既有编辑器和支持的序列化适配，不能静默丢弃不支持的格式。

网络错误或版本冲突保留草稿；提供查看最新版本，不自动强制覆盖。重试保持同一请求和键，用户再次修改才生成新键。

P00 读取正文和修订资料调用同一服务；服务器确定作用域，写入显式携带 revision/base。存在多个候选资料时先消除歧义。返回实际版本、release 和详情链接，不只生成一段修改建议。

## 验收

- 同来源父子链、中文/Markdown 精确读回；旧原件、证据和已发布引用不变。
- 幂等重放跨后续编辑和进程重启；同键异请求冲突。
- 同基线并发只有一个成功，失败者不留下部分状态；事务故障完全回滚。
- 解析重试与编辑的两种先后顺序均不会覆盖新正文；跨作用域引用拒绝。
- 内存与实际 PostgreSQL 行为一致，旧内容来源检查能识别过期版本。
- UI 保存、刷新、历史和冲突保留草稿；AI 与手动编辑读回相同持久版本。

来源用途修改与移除仍是独立待办，不由文本修订隐式改变。
