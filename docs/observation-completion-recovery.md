# 测量会话完成证据恢复

本接口只读取原会话历史，不重新发送问题，不解释答案，也不删除会话。原始来源及其回执保持不变；答案与引用仍由共享 AI 解析流程处理。

状态：通用回执改造正在工作区实施，审查、整合验证及部署尚未完成。本文描述当前候选契约，不代表线上已支持；准确交付状态见 `HANDOFF.md`，验证记录见 `WORKLOG.md`。

## 职责与证据边界

- MJS 的供应商策略负责解码已存来源、派生供应商请求／响应标识、读取同一会话历史并验证协议状态、完整库存和完成条件。不能通过选择最近会话或匹配标题补造原始来源缺失的证明。
- Rust 从当前作用域的权威存储读取原来源，验证来源及回执完整性、测量会话归属和账号绑定。候选来源限首份 `ordinal: 0` 来源，且尚无完成回执；必须具有测量用途及创建响应关联。Rust 不再解析供应商历史或终止事件。
- 独立恢复存档绑定原来源 ID、来源摘要、原回执摘要、恢复执行与新的浏览器会话、实际重新识别的账号、规范化 `completion` 以及历史审计。新浏览器仍读取原外部会话，不创建替代会话。
- `completion` 包含 `outcome` 和 `message_inventory`。`complete` 库存须包含用户与助手消息；`cleared_failure` 只接受空库存，表示供应商策略已证明失败回答被清除，不表示助手成功完成。库存以消息 ID 和角色二元组表示，按 ID 严格递增且无重复；Rust 校验通用结构并计算库存摘要。
- `history_json` 是有字节上限、脱敏且带 `history_sha256` 的不透明 JSON 审计。Rust 校验完整性和通用秘密字段限制，不从审计内容重新推导供应商完成结论。
- `get_by_source` 按作用域选择最新存档并重新验证来源及存档；损坏的最新记录直接报错，不回退到旧证据。
- 清理领取与事务内删除授权使用同一通用库存解析：优先采用原始完成回执，否则采用经过验证的恢复存档。账号占用、原执行、会话独占、先前删除尝试和删除前实时库存仍须独立核验。恢复不改写测量状态、生成答案或单独授予删除权限。

## 内部执行器接口

`POST /v1/sessions/{session_id}/recover-conversation-completion`

沿用执行器认证；不属于面向客户或模型的任意浏览器接口。

请求字段：

- `execution_id`
- `expected_identity`: `provider`、`platform_account_id`
- `external_conversation_id`
- `source_capture_id`
- `source_json`、`source_sha256`
- `source_receipt_sha256`：原始 capture 回执的 `digest_sha256`

请求直接携带已保存的原始来源及其绑定摘要，供应商消息标识由 MJS 策略从该来源派生。Rust 不提交供应商专用消息标识。

成功返回 `status: recovered`，回显 `execution_id`、`external_conversation_id`、`source_capture_id`、`source_sha256`、`source_receipt_sha256`，并携带独立识别的 `recovery_identity`、`observed_at`、规范化 `completion`、`history_json` 和 `history_sha256`。Rust 校验回显绑定、身份及通用证据后持久保存；此响应本身不授权删除。

失败返回 `retained`、`needs_login` 或 `unknown`，同样回显原来源绑定，并可携带闭合诊断。诊断仅保存固定枚举，不返回原始异常、正文、账号、地址或实际业务码。

- `retained/scope/unsupported_platform`：当前账号对应的供应商没有恢复策略，或执行会话与请求供应商不匹配，永久保留。
- `retained/source/source_ineligible`：供应商策略判定原始来源不具备恢复前提，永久保留；重新读取历史不能补造缺失的原始证明。
- `retained/history/completion_unverified`：同一会话的历史暂未满足完整性或完成条件，由持久调度退避后只读重试。
- `needs_login`：进入登录恢复退避；`unknown`：按暂时执行失败重试。其余 `retained` 不自动提升为可重试。

历史读取的 `retained/history/completion_unverified` 可附加闭合 `reason`：
`document_unavailable`、`read_deadline_exceeded`、`transport_unavailable`、
`response_envelope_unverified`、`history_envelope_unverified`、
`history_binding_mismatch`、`inventory_unverified`、`completion_unverified`。
这些原因用于区分同一历史重试分支，不授予完成或清理权限。

同一执行标识的参数不可改变；重放不产生新提问。执行器在读取前后识别账号，在独立截止内保持浏览器占用，超时后的迟到操作不能继续读取历史。适配器通过供应商注册表选择。

## 持久调度

迁移 `0056` 增加独立任务和追加式尝试记录；`0055` 仍只负责恢复证据存档。应用每 30 秒扫描，每页最多 100 项，固定扫描时点和游标；暂停项目仍可回收已发生测量的会话，不会创建新测量。

1. 按作用域及原始来源幂等入队，领取时重新读取权威来源并建立通用来源／会话绑定。缺失不可变前提终结为 `Retained`，不通过标题或最近会话猜测。
2. 数据库时钟控制 180 秒领取租约，过期回执不得改写新租约；仅可重新读取同一会话，不允许重发问题。租约过期不释放浏览器或账号占用。
3. 恢复原账号及网络版本，建立独立 `CompletionRecovery` 浏览器操作；重新识别账号后执行只读历史 RPC。
4. 持久保存并重新验证独立恢复证据后才记录 `Recovered`。浏览器确认释放后才能释放账号；关闭未知继续保留持久占用。
5. `Retry` 按 30 秒至一小时退避；`NeedsLogin` 为 15 分钟至一小时。仅明确的 `retained/history/completion_unverified` 转为历史重试；`unsupported_platform`、`source/source_ineligible` 及其余 `retained` 终结为永久 `Retained`。

代码入口为 MJS 的 `packages/browser-runner/src/provider-completion-recovery.mjs` 和 `packages/browser-runner/src/runner.mjs`、领域的 `crates/domain/src/measurement/observation_completion_recovery.rs` 与 `crates/domain/src/measurement/observation_completion_recovery_jobs.rs`、持久层的 `crates/persistence/src/measurement/completion_recovery_jobs.rs`、API 的 `crates/api/src/measurement/observation_completion_recovery.rs` 与 `crates/api/src/accounts/browser_bridge.rs`、应用的 `crates/app/src/observation_completion_recovery_dispatch.rs`。

本次通用回执需完成跨 MJS／Rust 的契约审查、来源绑定与错误分流回归、持久存档及重启恢复验证，再进行部署和真实同会话只读恢复验收。现有表、旧批次测试或服务就绪不证明本次改造已交付。
