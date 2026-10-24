# Memeloop GEO 工程交接

更新时间：2026-10-24。本文提供当前状态、运行入口及必要边界；无需聊天历史即可接手。

## 文档职责

- [product-plan-v1.md](product-plan-v1.md)：产品与验收基线，仅在产品负责人明确变更需求时修改。
- [TODO.md](TODO.md)：未完成工作、当前优先级和验收目标。
- [WORKLOG.md](WORKLOG.md)：完成结果、精确提交、检查与真实运行证据，以及失败定位过程。
- 本文：只维护一份当前检查点、代码导航和运行说明；历史检查点不继续累积。

## 当前检查点

当前优先交付测量产品体验：关注对象、问题集组织、推荐表现与证据分层，以及实际浏览器测量。暂停发布开发与真实发布测试。恢复发布测试前核对已有待执行请求，避免重试造成意外发送。

演示环境已按授权弃用旧测试数据并初始化新的空 PostgreSQL，恢复既有 bootstrap 身份；这不是旧业务数据恢复。原本地卷保留且不再使用。PR #140 的 API／Web 已配套部署，实际镜像、两份 bundle 摘要与 Ready 已核验，浏览器执行器保持原版。真实 UI 登录、空项目、输入框和供应商接入入口已验证；供应商重新登录及新测量仍待完成。复制存储健康与高可用尚未验收，不能由服务就绪推定业务或故障切换成功。

重置后首页对话已实际创建一套六题候选问题集，专业页及刷新读回一致。独立关注对象通过真实 UI 六次保存（修订 1→7），覆盖新增、编辑、删除、清空与恢复；刷新与 API 读回一致，其他项目设置不变。无关注对象时仍可输入任意话题，未发送测量。随后一次真实工作台对话成功读取已有目标并按明确要求为第二项目标增加别名（修订 7→8），其他设置不变；刷新工作台与详情编辑器读回相同两项目标及别名。该样本验证 AI／UI 共用状态，多轮、多报告与供应商稳定性仍待扩展验收。尚无重新接入的供应商账号或新测量。后续旧业务样本均为重置前证据，不代表新库已有相同数据；新验收须核对新项目、报告与账号，不复用旧标识或重发旧任务。

PR #140 使用独立 `measurement_targets` 保存名称、类型与显式别名，专业页及 AI 项目工具共用持久状态，不再从企业资料推断关注对象。按类型及名称／别名精确匹配，每个对象每份回答最多计一次，冲突排名单列；当前配置用于展示已存报告，冻结快照不变。PR #141 继续补同条件首位率及内容页测试预加载，尚未合并／部署，CI 与最终审查待完成。

测量体验首个增量已通过 PR #120 合并并部署 Web：可选关注产品、实际问题标题、问题筛选、最新优先与证据分层。已完成一次新产品推荐测量、关注对象保存及刷新读回，平台会话已清理。多供应商稳定性、模型菜单发现耗时及信源到优化建议仍待完成。

| 范围                       | 当前状态                                                                                                          |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| 主分支                     | `d58e13fc`（PR #140），PR 双平台 CI `40051387460` 通过；独立关注对象已合并并部署 |
| 最新验证                   | PR #140 镜像构建 `40053474562`、实际部署与目标 UI 读回通过；主分支 CI `40053474356` 一项首载测试失败，PR #141 修正待 CI |
| API                        | 已部署 PR #140；精确镜像、两份实际 bundle 摘要与 Ready 已核验；host v26／54 项，报告读取必须传 kind，可用 question_target_id 限定同题样本 |
| Web                        | PR #140 已部署并核验 Ready；目标 UI 增删改清空／恢复通过，真实 AI 加别名与两入口刷新一致；多报告及供应商验收待完成 |
| 浏览器执行器               | 已部署 `843e3233`（PR #116），实际镜像与健康状态已核验                                                            |
| 短内容发布                 | 发布／查回／撤回配套路径已部署；尚未真实发布或删除，业务闭环待验收                                                |
| 显式内容来源选择           | 两份文档均核对两份精确冻结来源版本；跨页及无模型复用继续验收                                                      |
| 模型传输截止               | PR #81 已部署；已存推荐回答真实解析成功，最终对话超时仍待处理                                                     |
| 内容检查／修正工作流       | 既有双文档完成；新增短内容 closed 1／1，自动修正一轮后零阻断，编辑用途标签被误删的语义质量待验收                  |
| AI 正文修订                | PR #118 已随 PR #126 API 与 Agent bundle 配套部署；实际表格修订待验收，当前不推进内容／发布工作 |
| 合作方内容导出             | PR #86 已部署；设置开关／刷新／撤销真实验证通过，实际内容下载及合作方联调待验收                                   |
| 统一测量提示／内容调度范围 | PR #89／#90 已部署；鉴权提示准备契约通过，已核对限定项目无待执行内容积压                                          |

PR #77 的错误分类已随三组件更新部署。源码、CI、镜像、实际部署及业务验收分别核对，不由主分支提交推定运行版本；部署逐项核对实际镜像、健康状态及两份 bundle 摘要。后台内容工作流在核对既有限定作用域及零待执行积压后启用；按需生成不再要求运营周期。

已完成的推荐测量验收跑通问题集 → 测量 → 自动解析 → 推荐排名／信源 → 固定报告。第四次测量只提交一次、只产生一次模型请求；受理到原始回执约 0.5 秒，约 81 秒后完成后台解析。结果为 7 个产品、7 个明确排名和 9 条引用；刷新及精确信源链接通过。原始 `extraction_pending` 回执保留，完成状态来自持久解析，平台会话经自动核对已删除，浏览器及预约均释放。

上述成功样本不代表首页研究闭环完成。另一研究任务先遇到固定 60 秒模型时限，修复后又出现模型连接失败。备用问题集的部分存档修复已生效，但原始测量仍为 partial/timeout。随后经原平台精确会话与问题全文核对，恢复完整回答，通过知识库 UI 导入一次并刷新验证成功。恢复资料不冒充自动测量解析；首页自动编排、自动查回及该会话清理仍待完成。

按需正式内容生成已去除周期前提，对话可分页发现正文条目。最新一次续办成功受理两份文档，第四次模型调用等待响应头约 120 秒后超时，最终对话仍为 failed；后台执行独立继续。文章分支的阻断已定位为同一片段多条有效引文被误拒绝，PR #114 逐条验证后去重，原执行自动恢复为 closed、ready 2／2，未新建执行或手动重提。两份版本 1 的编辑器、冻结来源、检查结果、实际 Markdown 下载与刷新读回通过，既有 FAQ 完整版本及导出未变化。PR #115 已让新生成内容使用既有结构化表格契约；现有文章版本 1 的三处比较表仍为管道文本，需以新修订验收。现网 P00 尚缺正文修订工具；PR #118 的 `content_document_revise` 已通过审查及 CI，复用编辑器追加版本能力，仍待配套部署及真实模型验收。来源在独立证据面板展示；原研究会话清理与最终对话结果自动接续仍待完成。

当前固定报告含四次独立测量、三份有效推荐，前三份快照不变。两项精确同名产品在相邻有效样本中出现 1→2、3→4 的明确排名变化；缺测未计为未推荐，不归并不同名称或型号。报告页曾超过 30 秒仍显示加载骨架，最终读回通过，延迟原因仍需定位。

最新问题集路径已通过真实浏览器验收：创建购买决策问题集、测量其中一道优化题、自动保存六个推荐对象与八条引用，再从该版本的记录目录生成排名报告并进入 AI 对话。回复引用已保存证据、区分不同产品型号并给出资料改进建议，刷新后仍可读；报告分析没有新增测量。原平台会话清理已确认 deleted。针对“初始未知回执但已有可信解析”的工具说明已部署并通过真实模型核验，回复能使用存档证据且不声称当前实时表现。

第二个网页供应商已对同一优化题完成一次真实测量：单次提交、单次执行、一份完整原文，自动解析三个明确排名和七条引用，刷新读回一致，平台会话自动删除。此项证明该样本的完整链路，不代表全部账号、模式或其他供应商已经可用。新报告回复格式也已实际验证：先给产品表现、再给竞品依据与优先建议，无内部字段表，所有引用来自已存证据。

独立浏览器登录、问题集／临时测量目录及刷新保持登录已验证；上述结果不证明多供应商稳定性、真实发布或完整产品闭环。详细证据见工作日志，剩余验收见 TODO。

## 产品与实现边界

默认入口为 AI 对话，立即显示输入框，首次发送时才创建会话。专业页面查看和微调同一持久数据；任意话题测量不依赖企业资料或优化周期。导航合并要求见 [ux-consolidation.md](ux-consolidation.md)。

产品主线为：版本化企业资料 → 冻结文档清单的第一层 fan-out → 文档 × 平台的第二层 fan-out → 发布与独立测量证据 reduce → 不可变周报和下一轮增量。MemeLoop 组织循环与工作流；Rust 负责权限、状态、预算、幂等、持久记录及外部副作用。

当前实现包含同源 Cookie／CSRF 身份底座、资料导入与版本、内容生成／修订、分发清单／outbox、账号与网络资源、独立测量及报告。TXT／Markdown／CSV、异步 PDF 与 Office 各有独立导入契约。真实发布、无人值守跨周期、生产费用／容量等仍按 TODO 验收，不能由工具注册或一次样本推定完成。

当前正文使用 schema 2。Agent 旧知识 search／ask 注册及启动依赖已移除，资料导入、编辑、存档和显式来源版本读取保留。生成、检查和修正提示均已迁入既有 MJS 工作流，通过 PR #82／#84 合并并包含在部署镜像中；双文档真实生成、检查及导出通过，自动修正继续独立验收。供应商协议适配归 MJS，答案、引用和推荐由 AI 提取；Rust 保存通用来源、状态与审计。

## 代码入口

Rust 的 API、领域及持久化业务均按 `accounts`、`agent`、`content`、`knowledge`、`measurement`、`partners`、`projects`、`publishing` 归类；从各目录的 `mod.rs` 进入，旧的根级业务文件路径已失效。

| 关注点              | 入口                                                                                                                                                                                                                       |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 应用装配／模型路由  | [app/src/main.rs](../crates/app/src/main.rs)、[api/src/lib.rs](../crates/api/src/lib.rs)、[runtime-deployment.md](runtime-deployment.md)                                                                                   |
| AI 会话与执行       | [AgentWorkbenchPage.tsx](../apps/web/src/pages/AgentWorkbenchPage.tsx)、[api/src/agent/mod.rs](../crates/api/src/agent/mod.rs)、[run_executor.rs](../crates/api/src/agent/run_executor.rs)                                 |
| JS 工具／内容工作流 | [agent-loop.mjs](../packages/agent-runtime/src/agent-loop.mjs)、[content-workflow.mjs](../packages/agent-runtime/src/content-workflow.mjs)、[worker/src/host.rs](../crates/worker/src/host.rs)                             |
| 问题集／测量／报告  | [QuestionSetsPage.tsx](../apps/web/src/pages/QuestionSetsPage.tsx)、[api/src/measurement/mod.rs](../crates/api/src/measurement/mod.rs)、[persistence/src/measurement/mod.rs](../crates/persistence/src/measurement/mod.rs) |
| 账号／远程登录      | [api/src/accounts/mod.rs](../crates/api/src/accounts/mod.rs)、[browser-runner/src/server.mjs](../packages/browser-runner/src/server.mjs)、[remote-login-runtime.md](remote-login-runtime.md)                               |
| 内容／发布          | [api/src/content/mod.rs](../crates/api/src/content/mod.rs)、[api/src/publishing/mod.rs](../crates/api/src/publishing/mod.rs)、[ContentPages.tsx](../apps/web/src/pages/ContentPages.tsx)                                   |

## 本地运行

先查看 `git status --short` 与最近提交，保留已有工作树和并发改动。源码、构建缓存、临时目录及测试产物使用指定工作卷；Windows V8 预编译库恢复及校验见 [README](../README.md#windows-v8-ci-artifact)，无需重新编译 V8。

环境要求：Node.js 22.17+、pnpm 10.17.1、Rust 1.96+；持久依赖可用 Docker Compose。内存模式适合 UI／API 开发，重启即丢失数据：

```powershell
pnpm install --frozen-lockfile
Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
$env:GEO_DEV_PASSWORD = "choose-a-local-password"
cargo run -p geo-app
```

另开终端执行 `pnpm --dir apps/web dev --host 127.0.0.1`，访问 `http://127.0.0.1:5173`，使用合成开发账号 `demo@localhost` 和上述开发密码。Vite 将同源 API 请求转发至回环后端。

持久模式先按 [.env.example](../.env.example) 启动专用 PostgreSQL，并在进程环境设置 `DATABASE_URL`；复制 env 文件不会自动载入 Rust 进程。启动自动迁移，连接／迁移失败直接终止，不回退内存。迁移不创建用户、Host 或成员关系，需按 [runtime-deployment.md](runtime-deployment.md) 显式 `--bootstrap`。

复用持久本地环境的监督启动、真实浏览器软件登录检查及交互窗口见 [local-workspace.md](local-workspace.md)；私有配置保存在仓库外。持久模式使用 Secure Cookie，浏览器须走配置正确的 HTTPS 入口。

### 模型与 bundle

执行 `pnpm agent:bundle` 生成 `packages/agent-runtime/dist/` 下的对话和内容两份 bundle，产物不提交；重建后重新计算实际文件 SHA-256 并更新配置。

本地对话单回合须同时设置 `GEO_AI_BASE_URL`、`GEO_AI_API_KEY`、`GEO_AI_MODEL`、`GEO_AGENT_BUNDLE_PATH`、`GEO_AGENT_BUNDLE_SHA256`。默认只允许回环内存模式；持久本地开发的固定作用域开关、正式租户 Token Center 路由及两者限制见 [runtime-deployment.md](runtime-deployment.md) 和 [token-center-integration.md](token-center-integration.md)。

内容工作流另配置 `GEO_CONTENT_BUNDLE_PATH`／`GEO_CONTENT_BUNDLE_SHA256`。部分配置、摘要错误或超过 8 MiB 的 bundle 会被拒绝；无模型能力如实返回 `capability_missing`。已封存内容可在无模型时恢复分发准备，不能据此推定可以生成新正文。

## Runtime 必须保留的约束

- V8 isolate 在同一个 `spawn_blocking` 任务内创建、驱动、销毁，并使用自己的 current-thread Tokio runtime；host 能力调用投回应用 runtime，避免非 Send V8 状态跨线程以及 I/O 资源跨 runtime。
- JS 只加载批准的内存模块并调用封闭 host ops，不持有凭据或任意文件／网络／进程权限。以 `HOST_OPS_VERSION` 和 `HostOp::COUNT` 为当前契约依据；跨包变更须同步 bundle、实际摘要及调用方。
- 每回合默认 64 MiB V8 堆上限、near-heap 终止、墙钟截止与 Rust 输出预算；取消触达 isolate 及挂起的模型调用，新回合不复用旧取消状态。生产多副本和资源容量仍需独立验收。
- HTTP 提交只表示受理；持久原子领取才开始执行。queued 扫描、内容执行租约与 Agent running 恢复是不同机制。
- `GEO_SINGLE_PROCESS_EXECUTOR` 默认关闭；仅整个数据库严格单执行进程才可启用旧 running 启动对账，滚动部署／多副本不得启用。running 多副本租约、优雅关闭和中途 checkpoint 仍未完成。
- tool-call ledger 的 intent／attempt／outcome 和完成结果存档不等于中途恢复；未知外部结果须查回，不盲目重发。详见 [agent-tool-ledger.md](agent-tool-ledger.md)。
- 缺测不算未提及，发布未知不算失败；清理权限独立于答案解析成功。恢复不改写原始来源，不给历史证据补造归属、完整性或删除权限。

## 部署与秘密

容器、镜像与清单入口见 [cluster-demo.md](cluster-demo.md)。先核对不可变镜像摘要、迁移和跨组件契约，再选择升级顺序：

- 升级前停止新受理／派发，核对活跃任务、登录引用、浏览器占用及未知回执，保留数据库、凭据与正在使用的会话；执行器就绪后再恢复派发。就绪状态不能代替业务验证。
- 增加执行器回传字段时先部署能接受该字段的 API，再切换执行器；远程登录 `desktop-readiness` 则先执行器后 API，回滚反向。通用回执及 host DTO／op 的跨包变更按对应 PR 配套发布。
- 持久登录支持空到期时间；旧 API 不能读取新会话，不可混用不兼容实例或直接回滚旧二进制。回滚需兼容读取或显式会话处理。
- API／runner 通过 `GEO_BROWSER_RUNNER_URL`／`GEO_BROWSER_RUNNER_TOKEN` 连接；64 位十六进制 `GEO_CHANNEL_SECRET_KEY` 必须跨重启保留，否则已有加密登录态不可恢复。资源管理员池作用域由 `GEO_OPERATOR_POOL_TENANT_ID` 配置。
- 远程登录使用维护中的 noVNC／VNC 桌面栈；不恢复截图轮询、坐标／按键接口或自制输入、剪贴板协议。成熟桌面输入法、断线及中文输入仍需实际验证。
- 模型传输诊断使用 `RUST_LOG=geo_api=info,geo_provider=info`，只记录固定阶段、状态、耗时、请求大小及有限错误分类；模型列表可达不证明推理成功。
- 真实地址、研究对象、客户／供应商身份、账号、凭据、私有截图及运行上下文不写入 tracked 文件、日志、测试夹具、提交或 PR。秘密仅由服务端环境／秘密管理器注入，禁止放入 `VITE_*`、JS、消息或 checkpoint。
- PostgreSQL FORCE RLS、非 bypass 角色、正式对象存储、权益／结算及容量目标仍以 TODO 验收；Compose 中存在 NATS／Redis／MinIO 不代表产品已接入。

## 验证与交付

按变更范围运行定向回归；完整基线命令为：

```powershell
pnpm format:check
pnpm typecheck
pnpm test
pnpm build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

PostgreSQL 回归使用可丢弃空库，在环境设置 `GEO_TEST_DATABASE_URL` 后执行 `cargo test -p geo-persistence --test postgres -- --ignored`；测试会迁移并写数据，禁止指向共享或重要数据库。忽略测试必须显式运行，不把默认跳过当作通过。

生成 bundle 的无真实凭据装配验证：`cargo test -p geo-app generated_bundle_runs_one_turn_through_assembled_provider -- --ignored`。它使用测试 transport，不代表真实模型或渠道成功。

浏览器烟测入口为 `scripts/verify-web-smoke.mjs`：先编译本提交应用并设置 `GEO_SMOKE_APP_BINARY`，截图目录 `GEO_SMOKE_OUTPUT_DIR` 放仓库外。它使用真实 Rust API／Vite 和已有 Chromium，不下载浏览器；端口、临时目录及浏览器缓存可配置。内容模式 `GEO_SMOKE_CONTENT=1` 需先生成两份 bundle 及可用 Python 标准库，使用合成模型夹具，不继承工作站凭据。默认端口 8080／5173，夹具端口 18081，须避开已有服务。

烟测只清理自身启动的服务。可用 `GEO_SMOKE_API_PORT`／`GEO_SMOKE_WEB_PORT`／`GEO_SMOKE_PROVIDER_PORT` 指定端口，`GEO_SMOKE_TMP_DIR`、`PLAYWRIGHT_BROWSERS_PATH` 和 `GEO_SMOKE_PYTHON` 指定临时目录、已有浏览器及 Python。验收同时检查窄屏根节点溢出与内部裁切；合成流程不证明真实模型质量、发布、搜索或生产持久部署。

每个增量通过短 PR 交付至 main，最多两轮协调审查与指定 Kimi 审查，验证检查后 squash merge，标题保留 PR 编号；替代审查人须经产品负责人批准。跨包 API／工具／schema 改动列明适配要求。提交前检查 staged 内容与文件名是否含私有上下文；删除合并分支前核对无未合并工作并保留活跃工作树。

## 主题文档索引

- 测量与排名：[standalone-measurement.md](standalone-measurement.md)、[question-sets.md](question-sets.md)、[recommendation-measurement.md](recommendation-measurement.md)、[measurement-weekly-reports.md](measurement-weekly-reports.md)。
- 原文恢复与清理：[saved-observation-analysis.md](saved-observation-analysis.md)、[observation-completion-recovery.md](observation-completion-recovery.md)、[provider-conversation-lifecycle.md](provider-conversation-lifecycle.md)。
- 账号与网络：[operator-account-onboarding.md](operator-account-onboarding.md)、[operator-login-queue.md](operator-login-queue.md)、[account-network-bindings.md](account-network-bindings.md)、[durable-browser-ownership.md](durable-browser-ownership.md)、[browser-disposal-receipts.md](browser-disposal-receipts.md)。
- 资料导入：[w03-knowledge-contract.md](w03-knowledge-contract.md)、[csv-import.md](csv-import.md)、[pdf-import.md](pdf-import.md)、[office-import.md](office-import.md)、[p00-attachment-import-slice.md](p00-attachment-import-slice.md)。
- 内容与分发：[content-auto-repair.md](content-auto-repair.md)、[content-reuse.md](content-reuse.md)、[content-media-contract.md](content-media-contract.md)、[publication-reconciliation.md](publication-reconciliation.md)、[rich-publication-delivery.md](rich-publication-delivery.md)。
- 模型、报告与接入：[project-ai-settings.md](project-ai-settings.md)、[scoped-test-ai.md](scoped-test-ai.md)、[measurement-period-and-search.md](measurement-period-and-search.md)、[report-preview.md](report-preview.md)、[seo-geo-delivery.md](seo-geo-delivery.md)、[partner-integration.md](partner-integration.md)。
