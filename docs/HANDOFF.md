# Memeloop GEO 工程交接说明

更新时间：2026-10-02

适用分支：`integration/w00-pr-stack`（本地集成，尚未合并到 `main`）

当前实现基线：W00 运行时兼容、Provider HTTP/路由、本地 P00 模型装配、SSE 重验与 P07 项目级文档清单；精确验证进度见 `WORKLOG.md`。不配置模型时 AI 保持未开启，生产租户凭据适配尚未完成。

本文是脱离历史对话后的工程入口。接手者不需要读取 Codex、聊天记录或本地代理上下文；产品范围、当前状态、未完成任务和验证方式均以仓库内容为准。

## 1. 文档的权威顺序

1. [`product-plan-v1.md`](product-plan-v1.md)：产品、架构、页面、领域模型、API、容量和验收的完整实施基线。当前版本为 1.1。除非产品负责人再次明确变更需求，不要直接修改。
2. [`TODO.md`](TODO.md)：只保留未完成工作和当前验收目标。完成后从这里删除，并将结果写入工作日志。
3. [`WORKLOG.md`](WORKLOG.md)：已完成工作、验证证据、已知限制和重要技术判断。
4. 本文：仓库导航、运行方式、实现现状和接手步骤。
5. [`w03-knowledge-contract.md`](w03-knowledge-contract.md)：W03 企业知识导入纵切的细化契约；若与产品规格冲突，以产品规格为准。

不要把进度、临时判断或日常日志补进产品规格书；分别维护 `TODO.md` 与 `WORKLOG.md`。

## 2. 产品主线

系统的核心不是单个平台发布器，而是可恢复的两次 fan-out 和一次 reduce：

1. 企业资料经过解析、溯源和版本化，fan-out 为有限、可封存的知识文档清单。
2. 每份知识文档按目标平台、账号、连接器和网络策略 fan-out 为独立发布分支。
3. 发布证据、站点验证和独立 AI 渠道测量进入 reduce，形成不可变周报和下一轮增量。

P00 AI 工作台是默认入口。用户应能通过对话或附件调用所有产品能力；P01–P16 是需要查看或编辑具体细节时的专业界面。MemeLoop 负责 Agent/Loop 编排，Rust 是权限、预算、状态、清单、发布、测量、费用和外部副作用的权威边界。

## 3. 当前已经实现

### 工程与身份

- Rust/Axum 后端、TypeScript/React/Fluent 前端、pnpm/Cargo 工作区及 CI 基线。
- 同源 Cookie 会话、CSRF/Origin 校验、Host 到 Operator 解析、Membership 到 Tenant 的服务端授权。
- 本地内存开发模式和 PostgreSQL 权威模式；配置了 `DATABASE_URL` 后，连接或迁移失败会直接终止，不会静默回退到内存。
- 项目创建、配置修订、三类独立估算、原子启动、稳定幂等 Operation、Cycle、Workflow 和两份未封存 manifest 骨架。

### 企业知识库 W03-A

- 上传会话、SHA-256 字节核验、批量导入、来源/版本、确定性文本分段、不可变 KnowledgeRelease、证据定位和 evidence-only 问答。
- P01 已能物化文本、URL 引用和已上传对象；P03–P05 已接真实 API。
- 当前只确定性解析 `text/plain` 与 `text/markdown`。PDF、DOCX、XLSX、CSV、网页抓取、OCR、向量检索、LLM 回答和结构化事实提取尚未实现。

### P00 AI 工作台

- P00 是侧边栏首项，项目根、项目切换和首次启动完成后默认进入 `/chat`。
- 局部集成 `@memeloop/react-ui` 的 `AgentChatView`，外围应用壳继续使用 Fluent UI。
- 已定义并实现内存版 Conversation、Message、Turn、Run、AttachmentReference、RuntimeCapability 和递增 ConversationEvent。
- 已有会话创建/列表/详情、消息提交、Turn 取消和 SSE 重放 API；支持幂等提交、同键异请求冲突、附件-only 消息、跨项目隔离、`after`/`Last-Event-ID` 恢复。
- P00 已接多附件选择/拖拽/粘贴、逐项上传及失败重试；专用附件 API 核验字节与摘要，提交消息时核对作用域和已提交对象元数据。TXT/Markdown 可经模型显式导入工具形成知识版本，再检索并引用回答；上传本身不直接入库，原始大文件不塞入模型上下文。
- 真实 MemeLoop 已执行模型 → `knowledge_search` → Rust 检索 → 模型回答；JSON function-tool 协议可用，尚无流式工具分片、多回合历史或中途恢复。
- Rust JS Runtime 未接入时明确返回 `capability_missing`，界面不会伪造 AI 回复。

相关入口：

- 前端：`apps/web/src/pages/AgentWorkbenchPage.tsx`
- 前端 API：`apps/web/src/api/agent.ts`
- Agent 领域：`crates/domain/src/agent.rs`
- Agent HTTP/SSE：`crates/api/src/agent.rs`
- 内存/持久化边界：`crates/persistence/src/agent.rs`
- 应用装配：`crates/api/src/lib.rs`、`crates/app/src/main.rs`

### W00 兼容探针（引擎腿）

- 新增隔离 crate `crates/worker`（包名 `geo-worker`），基于 `deno_core 0.412.0` 内嵌 V8；`geo-api` 已接入运行时边界，应用在显式本地模型配置下装配真实 bundle，否则为 `unconfigured()`。
- 7 项原始探针覆盖 ESM 跨模块加载、Promise 与顶层 await、host op、墙钟超时、外部取消、堆上限可恢复终止和 checkpoint 序列化。原 PR 验证记录见工作日志；当前集成修改必须单独复验，不能沿用旧测试结果。
- 隔离边界：JS 只能经 4 个窄 host op 触达 Rust；模块仅限 Rust 注入的内存 allow-list，无文件系统、网络或包 registry 解析；checkpoint 是 Rust 拥有的宿主状态序列化，不是 V8 堆快照。
- 回退路径不需要第二套运行时：`deno_core` 自带 `quickjs` feature，可在同一 API 上切换引擎。

相关入口：

- 探针运行时：`crates/worker/src/runtime.rs`
- 内存模块加载器：`crates/worker/src/loader.rs`
- 窄 host op：`crates/worker/src/ops.rs`
- 最小 bundle：`crates/worker/src/bundle.rs`
- 探针测试：`crates/worker/tests/probe.rs`

### W00 安全 Host Ops

- 封闭且带版本的 op 面（`geo.hostops.v3`）：`model.complete.v1`、`knowledge.search.v1`、`knowledge.import_attachments.v1`、`manifest.read.v2`、`publish.submit.v2`、`measure.sample.v2`、`report.get.v1`、`report.reduce.v1` 八项。附件导入只接受 Rust 已绑定到当前回合的对象；JS 无法取得 SQL、任意网络、文件、进程或环境变量。
- 边界方向为 `geo-api → geo-worker`，worker 从不反向依赖 API。请求 DTO 全部 `#[serde(deny_unknown_fields)]` 且不携带 tenant/project 选择器，作用域只能来自 Rust 侧 bridge。预算、单次调用截止与取消统一在 `HostBridge::invoke` 施加。
- `RepositoryHostOps` 已实现 `knowledge_search`、文档 `manifest_read` 和可注入的 `model_complete`；本地开发模型装配见第 5 节。清单读取保留规划状态、阻断原因及覆盖分母，不把规划项 ID 冒充正文版本；分发清单、发布和测量仍缺失。

### W00 Run Executor

- `append_message` 提交**之后**由 handler 调用 `run_executor::dispatch`：`begin_run`（原子 `UPDATE … WHERE status='queued' RETURNING`）→ `run_turn` → `finish_run`（**单事务**写 run 状态、错误、assistant 消息、turn 终态与事件）。HTTP 响应只陈述**受理**，永不乐观地写成 `running`。
- **运行时 flavor 是本模块的硬约束，改任何一处都会静默出错**：`deno_core` 的 op driver 用 `deno_unsync::tokio::spawn` 派生首次轮询未完成的 op future，该函数断言 `runtime_flavor() == CurrentThread` 并据此把非 `Send` future 伪装为 `Send`。因此**隔离体必须在自己的 current-thread 运行时上驱动**（且在同一个 `spawn_blocking` 任务内构建与销毁，因为隔离体非 `Send`），而**能力调用必须投递回应用运行时**（tokio I/O 资源绑定创建它的运行时，连接池不能跨 turn 迁移）。在多线程运行时上驱动隔离体在 debug 下中止进程、在 release 下是未定义行为。理由写在 `HostBridge::new` 的文档注释里。
- 未配置装配下提交消息会产生能力缺失；配置后真实 bundle 单回合已由应用装配测试覆盖，测试使用注入 transport，不代表已调用真实外部模型。参考 bundle + 桩桥测试仍见 `crates/api/tests/agent_runtime.rs`、`crates/worker/tests/host_ops.rs`。

### W00 Agent PostgreSQL 持久化

- `migrations/0007_agent_state.sql`（9 张表）与 `PgAgentRepository`；由 fail-closed 桩实现为完整实现，**未添加内存回退**，内存模式（不设 `DATABASE_URL`）不受影响。
- 会话行 `SELECT … FOR UPDATE` 是幂等重放、单活跃 turn、消息序号与事件游标的串行点；`cancel_turn` 锁 run 行使取消与完成成为一次串行判定。
- PostgreSQL 条件测试已用一次性容器实库执行通过（14/14），覆盖租户/项目隔离、幂等与同键异请求冲突、Run 状态机、run 原子声明与单次完成、checkpoint 往返与输入变更冲突、ToolCallLedger 幂等追加、并发事件序号单调、重启重放、取消竞争、仅附件消息。另有一项 250ms 超时指向 `127.0.0.1:1` 的常驻 fail-closed 测试，无需凭据。

相关入口：

- op 面与 trait 边界：`crates/worker/src/host.rs`
- op 实现与生产运行时：`crates/worker/src/host_ops.rs`、`crates/worker/src/host_runtime.rs`
- API 侧能力实现与装配：`crates/api/src/agent_runtime.rs`、`crates/app/src/main.rs`
- Agent 持久化：`migrations/0007_agent_state.sql`、`crates/persistence/src/agent.rs`
- 实库测试：`crates/persistence/tests/postgres.rs`

### W09 报告 fan-in 首批实现

- `crates/domain/src/report.rs`：冻结分母、逐文档/发布/独立测量状态汇聚、证据归属/时间校验、缺口结论与不可变更正；缺测不算未提及，未知发布不算失败。
- `crates/persistence/src/report.rs` 与迁移 `0009_report_snapshots.sql`：租户作用域下保存快照，重复生成复用结果；更正创建关联的新版本。
- `crates/api/src/reports.rs`：列表、详情、证据与周期 reduce 服务。应用的 PostgreSQL 模式定时扫描已持久化的到期周期，以游标分页避免旧失败项挡住后续周期；这不等于已经实现下一周周期创建。
- P14 已接列表、详情、证据、项目时区及 CSV；P00 注册 `report_get`/`report_reduce`，省略 ID 时在 Rust 中解析当前项目周期或最新快照。
- 目前应用输入只接已有文档规划及周期清单；发布/测量来源尚未实现，显示不可用或未封存。纯汇聚器已支持并测试这些输入类型，不代表生产已采到真实渠道数据。文档状态无截止时间证明时明确保留时间依据缺口，不回填伪时间。
- 正式周报遵循冻结截止；随时生成的临时预览、PDF、下一周期动作及真实跨平台效果报告仍未完成。准确测试和 CI 提交对应关系见 `WORKLOG.md`。

## 4. 仍未实现，禁止误判为完成

- 真实 MemeLoop bundle 单回合、附件导入及知识检索工具循环已通过嵌入式 V8 测试；剩余为其他业务工具、跨回合历史、持久恢复及分支编排。构建入口 `pnpm agent:bundle`，产物不提交。
- **重启对账**：已接 `GEO_SINGLE_PROCESS_EXECUTOR=true` 启动扫描，只适用于整个数据库严格单执行进程，默认关闭。滚动部署、多副本不得启用；queued 恢复、租约和优雅关闭仍未实现。PostgreSQL 对账测试已由 CI 独立 schema 验收。
- **回合进行中的实时取消**：`cancel_turn` 语义正确（`finish_run` 不会覆盖 `Cancelled`），但取消不触达隔离体，turn 仍跑到 deadline 才结束。`HostBridge::with_cancellation` 已备好接口。
- **隔离体基础保护**：64 MiB V8 堆、near-heap 终止、独立墙钟和 Rust 输出预算均已回归通过。不设固定隔离体并发准入门槛；高吞吐调度和进程资源观测仍需真实容量验收。
- **checkpoint 与 tool-call ledger**：executor 已写完成结果存档，中途恢复及 Rust 调用侧 intent/attempt/outcome 尚未接通。
- Token Center HTTP 适配已通过本地契约测试，但正式租户映射/应用装配、流式模型事件和模型费用记账尚未接通；本地单模型 HTTP 调用已装配，需环境注入凭据。
- GEO 分发、发布和测量工具，以及文档读取工具在对话 registry 的注册尚待补齐。
- P00 非 TXT/Markdown 解析、媒体/表格与跨回合附件使用；当前存储为内存或 PostgreSQL blob，非正式对象存储服务。回合输入可由持久 Message 重建，但这不等于完整自动恢复执行。
- 项目级知识文档清单已支持规划、封存和只读 GET，P07 显示真实覆盖项及来源依赖。刷新封存清单不重新规划，知识当前版本改变也不覆盖历史清单。产品级细化、正文生成、文档 × 平台矩阵、真实连接器/账号池/出口池仍待实现。
- 独立 AI 渠道采样与真实发布证据接入、丰富效果归纳及自动进入下一轮；已有报告快照/汇聚器和到期周期扫描，不代表整个周闭环完成。
- PostgreSQL 全仓库事务级 tenant scope、FORCE RLS 和非 bypass 角色验收。

PostgreSQL 模式下 `PgAgentRepository` 已实现持久化，数据库故障仍 fail closed，不能回退内存。完成结果存档不代表中途恢复或工具副作用账本。复制 `.env.example` 不会自动把变量载入 Rust 进程，PowerShell 中需要显式设置环境变量。

## 5. 本地启动

### 可选：启用本地 P00 模型单回合

仅允许未配置 `DATABASE_URL` 且监听 loopback 的本地内存模式。
正式租户 Token Center 接入另见 `token-center-integration.md`，不能使用此模式代替。

先运行 `pnpm agent:bundle`，再同时提供全部五项服务端环境变量：
`GEO_AI_BASE_URL`、`GEO_AI_API_KEY`、`GEO_AI_MODEL`、
`GEO_AGENT_BUNDLE_PATH`、`GEO_AGENT_BUNDLE_SHA256`。
密钥通过运行环境或密钥管理器注入，不写入命令日志、Git 或 VITE 变量。
产物路径为 `packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs`；
摘要可用 `Get-FileHash -Algorithm SHA256` 计算。重新构建后需更新摘要。

全部缺省时保持未配置；只提供部分配置、摘要错误、文件超过 8 MiB 或尝试用于数据库模式均拒绝启动。
模型路由固定为配置模型；该阶段仅证明真实 MemeLoop 单回合模型路径，
已支持 TXT/Markdown 附件导入及知识检索工具循环，但不代表多回合恢复、发布和测量工具已接通。不设置固定两回合并发门禁，优先跑通应用功能。

无需真实凭据的装配验证：
`cargo test -p geo-app generated_bundle_runs_one_turn_through_assembled_provider -- --ignored`。
它使用真实生成 bundle 与测试 transport，不会调用外部 AI。

### 最快的内存开发模式

内存模式适合 UI/API 开发；重启进程会丢失数据。不要设置 `DATABASE_URL`。

```powershell
pnpm install
$env:GEO_DEV_PASSWORD = "local-dev-password"
Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
cargo run -p geo-app
```

另开终端：

```powershell
pnpm --dir apps/web dev --host 127.0.0.1
```

浏览器打开 `http://127.0.0.1:5173`，用户名为 `demo@localhost`，密码是当前终端设置的 `GEO_DEV_PASSWORD`。

### PostgreSQL 模式

```powershell
Copy-Item .env.example .env
docker compose up -d postgres
$env:DATABASE_URL = "postgres://memeloop:change-me-local-only@localhost:5432/memeloop"
cargo run -p geo-app
```

启动会自动执行 `migrations/`。数据库迁移不创建演示用户、Operator Host 或 Membership；在补齐正式引导/种子流程前，PostgreSQL 模式主要用于迁移和 repository 集成测试。P00 的持久会话已实现，但应用运行时仍未配置，提交消息会记录明确的 `capability_missing`，不会生成 AI 回答。

完整基础设施定义见 `compose.yaml`，示例变量见 `.env.example`。任何模型、Token Center、客户、账号或代理凭据都只能由本地环境或秘密管理器注入，禁止写入仓库、前端变量、测试快照和日志。

## 6. 验证命令

提交前至少运行：

```powershell
pnpm format:check
pnpm typecheck
pnpm test
pnpm build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

PostgreSQL 条件测试需要一个可丢弃的空数据库：

```powershell
$env:GEO_TEST_DATABASE_URL = "postgres://..."
cargo test -p geo-persistence --test postgres -- --ignored
```

测试会执行迁移并写入数据，不要指向共享、生产或含重要数据的数据库。

## 7. 下一项工作的明确入口

当前最高优先级是 `TODO.md` 中的 W00，不要先扩展次要页面。建议按以下可独立提交的顺序推进：

1. **执行安全验证**：复验 current-thread 下模块求值及 `main` 死循环的独立线程墙钟终止、堆上限与取消；测试进程必须有外部截止保护。
2. **run executor 恢复**：驱动链已存在，剩余工作是租约/重启对账、实时取消及持久 checkpoint/tool-call ledger。不得通过扫描并结束所有 running run 的方式干扰其他副本。
3. **扩展真实 bundle 工具循环**：模型工具协议、附件显式导入及知识检索已通，下一步接入文档规划和生成；产物不入库，分发携带第三方许可。
4. **模型 Provider 与 GEO 工具桥接**：把上述四项 `capability_missing` 逐一变成真实实现。
5. **首个完整纵切**：上传附件并形成对象引用，给出带来源回答，生成两个文档分支，中断后从 checkpoint 恢复，再 reduce 为结果摘要。

当前附件导入的具体接缝、文件责任和验收见
[`p00-attachment-import-slice.md`](p00-attachment-import-slice.md)。

每个提交都必须：

- 同时包含相应领域/API/持久化或 UI 测试。
- 不把未实现能力显示为成功。
- 保持 tenant/project scope、幂等、可恢复和证据链。
- 完成后精简 `TODO.md`，把事实、测试数量、浏览器验证与限制写入 `WORKLOG.md`。

## 8. 已知风险与设计边界

- 上游 MemeLoop 官方 server worker 是 Node 实现；真实 bundle 已在 Rust/V8 单回合运行，但不代表所有 Node 功能、完整工具循环及多回合恢复均已兼容。
- `@memeloop/react-ui` 使用 MUI/assistant-ui；只能在 P00 局部 ThemeProvider 中使用，不能污染 Fluent 全局主题。
- 当前前端生产构建存在大 chunk 警告，尚不阻塞功能，但后续应按路由拆分 P00 依赖。
- `migrations/0004_tenant_rls.sql` 仍是安全 no-op，不能对外宣称 FORCE RLS 已完成。
- NATS、Redis、MinIO 已有本地基础设施定义，但当前纵切的大部分状态仍直接由 API/repository 处理，不能仅凭 Compose 服务存在就宣称已接入。

## 9. 接手者十分钟检查

```powershell
git status --short
git log -5 --oneline
Get-Content docs/TODO.md
pnpm install --frozen-lockfile
pnpm typecheck
cargo test --workspace --all-targets
```

预期：工作区为空；能看到 P00、W03-A、W02 的最近提交；前端类型检查和不依赖外部 PostgreSQL 的测试通过。若结果不同，先记录环境与失败证据，再修改代码，不要根据旧聊天记录覆盖仓库现状。
