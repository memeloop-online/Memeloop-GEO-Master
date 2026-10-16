# 测量会话完成证据恢复

本接口只读取原会话历史，不重新发送问题，不解释答案，也不删除会话。原始来源及其回执保持不变；答案与引用仍由共享 AI 解析流程处理。

## 已实现边界

- 原始来源必须已证明会话归属，且保留精确首轮请求／响应标识和终止事件。缺少这些信息不能通过选择最近会话或匹配标题补造。
- 独立恢复存档绑定原来源 ID、来源摘要、回执摘要、恢复执行与新会话、实际重新识别的账号以及经过投影的历史。
- `get_by_source` 按作用域选择最新存档并重新验证来源及存档；损坏的最新记录直接报错，不回退到旧证据。
- 清理领取与事务内删除授权复用同一个完成库存解析器：优先使用原始完整证据，否则使用经过验证的恢复存档。账号占用、原执行、会话独占、先前删除尝试和当前远端库存校验仍分别执行。
- 已归属的首轮会话若明确以 `backend_err`／`clear_response` 终止，平台随后返回完整空历史，可恢复仅供清理使用的空库存。原始来源必须是完整的 ready／错误 hint／close 序列，重新读取必须匹配同一会话、全量替换、零消息和显式空当前消息；不接受普通空历史或单消息历史。此证据没有 assistant completion，不改写测量状态、生成答案或跳过删除前的实时库存核对。

## 内部执行器接口

`POST /v1/sessions/{session_id}/recover-conversation-completion`

沿用执行器认证；不属于面向客户或模型的任意浏览器接口。

请求字段：

- `execution_id`
- `expected_identity`: `provider`、`platform_account_id`
- `external_conversation_id`
- `request_message_id`、`response_message_id`

成功返回 `status: recovered`，以及对应执行／会话标识、独立识别的 `recovery_identity`、`observed_at`、`history_json` 和 `history_sha256`。该状态只表示读取成功，不能单独证明完成或授权删除。

失败返回 `retained`、`needs_login` 或 `unknown` 与可选闭合诊断；不返回原始异常或凭据。Rust 桥接校验作用域、标识、身份、字节上限及摘要，领域验证仍负责会话完成证明。

历史读取的 `retained/history/completion_unverified` 可附加闭合 `reason`：
`document_unavailable`、`read_deadline_exceeded`、`transport_unavailable`、
`response_envelope_unverified`、`history_envelope_unverified`、
`history_binding_mismatch`、`inventory_unverified`、`completion_unverified`。
原因只解释既有拒绝分支，不改变返回状态、重试、完成证明或清理权限；不保存原始异常、正文、账号、地址或实际业务码。缺少原因仍兼容旧执行器。必须先部署支持该字段的 API，再部署新执行器；旧 API 会拒绝额外字段。

同一执行标识的参数不可改变；重放不产生新提问。执行器在读取前后识别账号，在独立截止内保持浏览器占用，超时后的迟到操作不能继续读取历史。适配器通过供应商注册表选择。

## 持久调度

迁移 `0056` 增加独立任务和追加式尝试记录；`0055` 仍只负责恢复证据存档。应用每 30 秒扫描，每页最多 100 项，固定扫描时点和游标；暂停项目仍可回收已发生测量的会话，不会创建新测量。

1. 按作用域及原始来源幂等入队，领取时重新读取权威来源、派生精确会话／消息绑定。缺失不可变前提终结为 `Retained`，不通过标题或最近会话猜测。
2. 数据库时钟控制 180 秒领取租约，过期回执不得改写新租约；仅可重新读取同一会话，不允许重发问题。租约过期不释放浏览器或账号占用。
3. 恢复原账号及网络版本，建立独立 `CompletionRecovery` 浏览器操作；重新识别账号后执行只读历史 RPC。
4. 持久保存并重新验证独立恢复证据后才记录 `Recovered`。浏览器确认释放后才能释放账号；关闭未知继续保留持久占用。
5. `Retry` 按 30 秒至一小时退避；`NeedsLogin` 为 15 分钟至一小时。明确的 `History/CompletionUnverified` 表示历史尚未完成，允许只读重试；无诊断或不支持的 `Retained` 不自动升级为可重试。

代码入口为领域的 `crates/domain/src/measurement/observation_completion_recovery_jobs.rs`、持久层的 `crates/persistence/src/measurement/completion_recovery_jobs.rs`、API 的 `crates/api/src/measurement/observation_completion_recovery.rs` 和应用的 `crates/app/src/observation_completion_recovery_dispatch.rs`。

基础恢复实现已通过定向测试、独立审查及 CI PostgreSQL 实库测试，迁移 0056 和调度 API 已部署；后续真实任务已进入重试，闭合原因诊断仍待交付，重启恢复和真实同会话验收尚未完成。不得把代码接线、表存在、历史空队列或服务就绪标为自动恢复验收完成。最新交付和部署状态以 `HANDOFF.md` 为准。
