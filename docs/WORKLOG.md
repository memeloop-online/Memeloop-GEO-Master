# 工作日志

## 2026-09-18

- 核对仓库：`main` 尚无提交，只有未入库的旧版方案。
- 旧方案含已否决的员工授权流程、过时的维基 403 结论，且缺少完整页面、知识库和资源池设计，因此不作为实施基线。
- 使用一次性短上下文 Astra 完成架构/API 审阅，补齐账号池、执行池、出口池、发布分配模型、策略实体、模块写权限和事件契约。
- 使用一次性短上下文 Astra 完成 UI/文案审阅，补齐批量接入、异常状态、无人审批交互和 Fluent UI 布局。
- 将正式 V1 产品与开发规格写入 `product-plan-v1.md`；后续冻结该文件。
- 完成 W01 工程基础：Rust workspace、Axum API、租户作用域、Operation、事件信封、SSE、JSON 命令幂等中间件、OpenAPI、React/Fluent 工作台、P01–P16 路由、P01 向导和 P02 总览。
- 建立 PostgreSQL/SQLx 持久化契约与初始迁移、PostgreSQL/NATS/Redis/MinIO 本地 Compose、AGPL-3.0-only、贡献说明和前后端 CI。
- 前端通过格式、类型、3 项组件测试和生产构建；后端通过格式、Clippy `-D warnings` 和 11 项测试；API 真实启动后健康检查与 OpenAPI 返回正常。
- 使用本地浏览器实机检查 P01/P02 布局、中文状态和 P01–P16 导航；移除开发运行时的 Fluent Keyborg 控制台错误。
- Docker Compose 配置验证通过；Docker Hub 拉取 PostgreSQL 镜像超时。已增加需显式 `GEO_TEST_DATABASE_URL` 的实库迁移测试，待镜像可用后执行。
- 用户提供此前未被读取的完整 1.0 产品规格。确认旧实施基线是缩减版本，暂停 W02 前后端代理，避免继续按错误范围推进。
- 使用一次性、无历史上下文的 Astra 将附件全部内容按原顺序恢复到 `product-plan-v1.md`；验证附件 972 条非空内容全部保留。
- 在完整规格中落实“两层 fan-out → reduce → 下一轮”：企业资料展开为有限版本化知识文档清单，文档再展开为全平台覆盖矩阵，最后汇聚发布证据和独立 AI 渠道测量，形成不可变周报及下一轮增量。
- 新增分支失败隔离、未知发布持续查回、周报截止快照、迟到证据版本、背压和跨阶段恢复要求；不因单个平台失败阻塞整轮，也不把跨平台数据混算为单一指标。
- 重新划定实施顺序：先补齐完整计划定义的 W01 登录会话、身份边界、API 客户端和异步状态，再按新 W02 的资料输入、预算估算与启动任务验收现有代码。

## 2026-09-19

- 完成 W01 同源 Cookie 会话：Argon2id 登录、不透明随机令牌、数据库仅存令牌哈希、CSRF 与 Origin 校验、Host 推导 Operator、Membership 校验 Tenant；旧身份请求头不能授权。
- 完成登录、工作区选择、退出、路由守卫、租户隔离查询键和统一加载/错误/空态；本地开发必须显式配置密码且只能监听回环地址。
- 接入项目列表、创建、详情、乐观并发更新、空数据总览、估算和启动 API；viewer 只读，普通 PATCH 不能绕过启动命令修改生命周期。
- P01 已能执行“服务端估算 → 创建草稿 → 启动 Operation”；创建成功而启动失败时只重试启动，并复用稳定幂等键。
- 启动 Operation 使用由租户、项目和幂等键推导的稳定 ID；并发同键请求、响应丢失和已持久化恢复标记均有测试覆盖。
- 真实浏览器完成“登录 → 选择工作区 → 四步配置 → 展示估算 → 创建并启动 → 总览”验收；总览明确区分“已启动”“等待知识处理”“尚未建立基线”，不伪称 W03 已解析资料。
- 统一 Fluent 依赖的 Keyborg 版本，消除同一页面加载两个不兼容主版本导致的焦点实例释放错误。
- 前端格式、类型检查、8 项测试和生产构建通过；Rust 格式、Clippy `-D warnings`、16 项测试通过，另有 1 项 PostgreSQL 实库测试因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- Astra 对照冻结计划审计 W02：现有估算的文档/平台/样本推导仍无真实能力快照依据；下一步改为三类独立且可为 unknown 的分母，并在同一事务创建配置版本、Cycle、两份未封存 manifest、Operation、Workflow 和 outbox。
- `0004_tenant_rls.sql` 仍是安全 no-op；在所有 repository 完成事务级 scope 和非 bypass 角色实测前，不宣称生产 RLS 已完成。
- 完成 W02 配置契约：项目时区、周报日/时间与截止、上一自然周窗口、文档范围、发布范围、目标、版本化来源引用；草稿允许不完整，启动使用独立严格校验。
- 周报窗口按项目时区的下一次周报触发计算，覆盖周日 23:59 截止、跨年和 DST 缺口；总览更新时间不再错误使用未来截止时间。
- 估算改为文档、文档×平台、AI 测量三类独立分母及三阶段费用；KnowledgeRelease、CapabilitySnapshot、MeasurementProtocol、PricingSnapshot 未形成时返回 unknown 和明确阻塞原因，不根据资料数量伪造覆盖。
- PostgreSQL `0005_atomic_project_start.sql` 与 repository 在同一事务创建 ProjectConfigRevision、OptimizationCycle、两份未封存 manifest、WorkflowRun、Operation、`cycle.created` outbox、启动记录和项目当前句柄；内存实现使用单锁并通过故障注入验证失败不留部分状态。
- 启动命令使用 `expected_revision`、稳定 Operation ID、哈希后的幂等键和请求哈希；同键同请求重放同一回执，同键异请求或不同键重复启动冲突；GET `/projects/:id/start` 支持刷新恢复。
- P01 收口为三步，草稿只创建一次并按 revision 串行自动保存；产品和目标用户可选且可通过 PATCH `null` 清空；P02 展示持久化配置、周期和两份 manifest 句柄。
- 文件上传 API 尚未实现，P01 明确提示且只提交真实可用的 URL、文本、对象引用和已有知识集合引用；上传会话与解析转入 W03。
- 真实浏览器完成“登录 → 工作区 → 三步配置 → unknown 估算 → 原子启动 → 总览 → 刷新恢复”验收；两份清单显示为未封存骨架，资料、基线和计划均未伪称完成，本轮无新增控制台错误。
- 前端格式、类型、11 项测试和生产构建通过；Rust 格式、Clippy `-D warnings`、13 项 API、1 项应用、3 项领域、3 项持久化测试及 doctest 通过。另有 2 项 PostgreSQL 条件测试因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- 完成 W03-A 企业知识库纵向切片：上传会话、原始字节核验、批量导入、来源/来源版本、确定性文本分段、不可变 KnowledgeRelease、可定位证据检索和 evidence-only 问答均具有内存与 PostgreSQL repository 契约。
- P01 文件入口已走“SHA-256 → 创建上传会话 → PUT 字节 → 完成核验”的真实 API；启动前将 URL、粘贴文本和已上传来源显式物化到知识库，重复执行不会复制已上传来源。
- P03—P05 已接入真实资料列表、来源详情、产品/事实空态、能力快照、当前知识版本、证据摘录模式和来源回链；未配置 LLM 时不会把检索片段伪装成完整回答。
- 浏览器实机回归发现知识 API 客户端漏传 `tenant_id`，导致资料物化报 `tenant selector is required`；修复为所有知识请求同时携带租户和项目资源选择器，并增加请求 URL 回归断言。
- 真实浏览器完成“登录 → 创建项目 → 粘贴公开资料 → 资料物化 → 启动 → 总览 → P03 → P04 → P05 查询‘质保’ → 回到来源详情”验收；总览显示知识版本已形成但文档清单仍待规划/封存，浏览器控制台无错误。
- W03-A 验证通过：Rust 格式、Clippy `-D warnings`、14 项 API、1 项应用、8 项领域、3 项持久化测试及 doctest；前端格式、类型、23 项测试和生产构建。2 项 PostgreSQL 条件测试仍因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- 当前能力边界如实暴露：只有 text/plain 与 text/markdown 能确定性解析；PDF/DOCX/XLSX/CSV、URL 抓取、OCR、向量、LLM 回答和结构化事实提取尚未接入；内存对象适配器不具备重启持久性。
