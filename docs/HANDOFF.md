# Memeloop GEO 工程交接说明

更新时间：2026-09-19  
适用分支：`main`  
当前实现基线：`e0c5fe7 feat: add P00 AI workbench foundation`

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
- Rust JS Runtime 未接入时明确返回 `capability_missing`，界面不会伪造 AI 回复。

相关入口：

- 前端：`apps/web/src/pages/AgentWorkbenchPage.tsx`
- 前端 API：`apps/web/src/api/agent.ts`
- Agent 领域：`crates/domain/src/agent.rs`
- Agent HTTP/SSE：`crates/api/src/agent.rs`
- 内存/持久化边界：`crates/persistence/src/agent.rs`
- 应用装配：`crates/api/src/lib.rs`、`crates/app/src/main.rs`

### W00 兼容探针（引擎腿）

- 新增隔离 crate `crates/worker`（包名 `geo-worker`），基于 `deno_core 0.412.0` 内嵌 V8，**尚未接入** `geo-api`/`geo-app`。
- 7 项探针测试覆盖 ESM 跨模块加载、Promise 与顶层 await、host op、墙钟超时、外部取消、堆上限可恢复终止和 checkpoint 序列化；`cargo test -p geo-worker` 全通过。
- 隔离边界：JS 只能经 4 个窄 host op 触达 Rust；模块仅限 Rust 注入的内存 allow-list，无文件系统、网络或包 registry 解析；checkpoint 是 Rust 拥有的宿主状态序列化，不是 V8 堆快照。
- 回退路径不需要第二套运行时：`deno_core` 自带 `quickjs` feature，可在同一 API 上切换引擎。

相关入口：

- 探针运行时：`crates/worker/src/runtime.rs`
- 内存模块加载器：`crates/worker/src/loader.rs`
- 窄 host op：`crates/worker/src/ops.rs`
- 最小 bundle：`crates/worker/src/bundle.rs`
- 探针测试：`crates/worker/tests/probe.rs`

### W00 安全 Host Ops

- 封闭且带版本的 op 面（`geo.hostops.v1`）：只有 `model.complete.v1`、`knowledge.search.v1`、`manifest.read.v1`、`publish.submit.v1`、`measure.sample.v1` 五项；不在枚举里的名字没有 op，也就没有 Rust 实现体。JS 无法取得 SQL、任意网络、文件、进程或环境变量。
- 边界方向为 `geo-api → geo-worker`，worker 从不反向依赖 API。请求 DTO 全部 `#[serde(deny_unknown_fields)]` 且不携带 tenant/project 选择器，作用域只能来自 Rust 侧 bridge。预算、单次调用截止与取消统一在 `HostBridge::invoke` 施加。
- 应用当前装配 `unconfigured()`，四个尚未实现的能力如实返回 `capability_missing`；`RepositoryHostOps` 只实现 `knowledge_search`。**没有用空结果冒充成功。**

### W00 Run Executor

- `append_message` 提交**之后**由 handler 调用 `run_executor::dispatch`：`begin_run`（原子 `UPDATE … WHERE status='queued' RETURNING`）→ `run_turn` → `finish_run`（**单事务**写 run 状态、错误、assistant 消息、turn 终态与事件）。HTTP 响应只陈述**受理**，永不乐观地写成 `running`。
- **运行时 flavor 是本模块的硬约束，改任何一处都会静默出错**：`deno_core` 的 op driver 用 `deno_unsync::tokio::spawn` 派生首次轮询未完成的 op future，该函数断言 `runtime_flavor() == CurrentThread` 并据此把非 `Send` future 伪装为 `Send`。因此**隔离体必须在自己的 current-thread 运行时上驱动**（且在同一个 `spawn_blocking` 任务内构建与销毁，因为隔离体非 `Send`），而**能力调用必须投递回应用运行时**（tokio I/O 资源绑定创建它的运行时，连接池不能跨 turn 迁移）。在多线程运行时上驱动隔离体在 debug 下中止进程、在 release 下是未定义行为。理由写在 `HostBridge::new` 的文档注释里。
- 未配置装配下成功路径**无法演示**（`main.rs` 故意没有能打开部分配置运行时的开关）；成功路径由使用参考 bundle + 桩桥的 API 测试覆盖：`crates/api/tests/agent_runtime.rs`、`crates/worker/tests/host_ops.rs`。

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

## 4. 仍未实现，禁止误判为完成

- 真实 MemeLoop bundle 在嵌入式引擎中的加载（引擎契约本身已由 `crates/worker` 验证）。`memeloop/loop-api` 的传递闭包不含任何 `node:` 内建导入，外部依赖只有 `zod`/`acorn`/`json5`/`semver` 四个纯 JS 包；剩余工作是把它们经既有内存加载器注入，并从 `createAgentToolLoopRunner` 这个可移植入口进入。详见 `WORKLOG.md` 中 2026-09-21 的更正记录。
- **重启对账**：run executor 已在进程内驱动真实 turn，但进程内**没有任何优雅关闭**，因此退出时在飞的 run 会永久停在 `running`。需要启动期扫描。注意该做法在单进程下成立、**多副本下错误**，落地时必须把这个假设显式写进代码。
- **回合进行中的实时取消**：`cancel_turn` 语义正确（`finish_run` 不会覆盖 `Cancelled`），但取消不触达隔离体，turn 仍跑到 deadline 才结束。`HostBridge::with_cancellation` 已备好接口。
- **隔离体堆上限**：`EmbeddedAgentRuntime::start` 传入 `None`，`install_heap_limit_guard` 目前是**死代码**，失控循环只受 turn deadline 约束。
- **checkpoint 与 tool-call ledger** 已有持久化实现，但运行路径尚未写入。
- 模型 Provider、Token Center 真实调用、流式模型事件和模型费用记账；`model_complete` 目前如实返回 `capability_missing`。
- GEO 工具桥接实现；`manifest_read`、`publish_submit`、`measure_sample` 目前如实返回 `capability_missing`。
- P00 文件选择到对象存储引用的完整上传适配。
- 知识文档清单的生成与封存、文档 × 平台发布矩阵、真实连接器/账号池/出口池执行。
- 独立 AI 渠道测量、证据 reduce、不可变周报和自动进入下一轮。
- PostgreSQL 全仓库事务级 tenant scope、FORCE RLS 和非 bypass 角色验收。

PostgreSQL 模式下 `PgAgentRepository` 当前故意 fail closed；不要用内存回退掩盖迁移或持久化缺失。复制 `.env.example` 也不会自动把变量载入 Rust 进程，PowerShell 中需要显式设置环境变量。

## 5. 本地启动

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

启动会自动执行 `migrations/`。数据库迁移不创建演示用户、Operator Host 或 Membership；在补齐正式引导/种子流程前，PostgreSQL 模式主要用于迁移和 repository 集成测试，而且 P00 Agent API 会因持久化尚未实现而返回依赖不可用。

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

1. **兼容探针（引擎腿已完成，bundle 腿已解锁）**：`crates/worker` 已在 `deno_core 0.412.0` 上通过 ESM、Promise、host op、超时、取消、内存上限与 checkpoint 七项测试，尚未接入 `geo-api`/`geo-app`。真实 bundle 腿的实际阻碍不是 Node 内建——`memeloop/loop-api` 不含任何 `node:` 导入，只需注入 `zod`/`acorn`/`json5`/`semver` 四个纯 JS 依赖，并从 `createAgentToolLoopRunner` 进入。回退到 QuickJS 不需要维护第二套运行时——`deno_core` 自带 `quickjs` feature。
2. **run executor**：安全 Host Ops 与 Agent 持久化均已完成，但两者之间还缺驱动者——有桥、有库、没有进程跑真实 turn。入口是 `EmbeddedAgentRuntime::start(scope, budgets)`；隔离体非 `Send`，需在 run 自己的线程上构建 `HostRuntime`。
3. **落地真实 bundle 加载**：打包配方已实证（见 `WORKLOG.md` 2026-09-21），但 1.5 MB 自包含产物的存放方式与第三方许可声明策略需先定夺，再决定是构建时生成还是分发各依赖 ESM。
4. **模型 Provider 与 GEO 工具桥接**：把上述四项 `capability_missing` 逐一变成真实实现。
5. **首个完整纵切**：上传附件并形成对象引用，给出带来源回答，生成两个文档分支，中断后从 checkpoint 恢复，再 reduce 为结果摘要。

每个提交都必须：

- 同时包含相应领域/API/持久化或 UI 测试。
- 不把未实现能力显示为成功。
- 保持 tenant/project scope、幂等、可恢复和证据链。
- 完成后精简 `TODO.md`，把事实、测试数量、浏览器验证与限制写入 `WORKLOG.md`。

## 8. 已知风险与设计边界

- 上游 MemeLoop 官方 server worker 是 Node 实现；Rust 托管兼容性已由 `crates/worker` 的探针证实引擎契约可用，但真实 bundle 尚未在内嵌引擎中实际跑通，所以 bundle 腿仍是必须完成的第一步。
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
