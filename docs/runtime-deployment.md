# 持久运行环境接入

本文描述当前可配置入口，不替代产品规格或真实渠道验收。所有真实地址、账号、密钥和映射值通过部署环境、秘密管理器及数据库配置注入，不写入公开仓库。

## 首次身份初始化

PostgreSQL 迁移不创建默认用户。部署者在服务器执行一次：

```text
cargo run -p geo-app -- --bootstrap
```

进程环境必须提供：

- `DATABASE_URL`
- `GEO_BOOTSTRAP_OPERATOR_ID`、`GEO_BOOTSTRAP_OPERATOR_SLUG`、`GEO_BOOTSTRAP_OPERATOR_NAME`
- `GEO_BOOTSTRAP_HOST`：浏览器请求中的精确 Host，需要时包含端口
- `GEO_BOOTSTRAP_TENANT_ID`、`GEO_BOOTSTRAP_TENANT_SLUG`、`GEO_BOOTSTRAP_TENANT_NAME`
- `GEO_BOOTSTRAP_USER_ID`、`GEO_BOOTSTRAP_LOGIN_NAME`、`GEO_BOOTSTRAP_USER_NAME`
- `GEO_BOOTSTRAP_PASSWORD`：仅秘密环境注入，不作为命令行参数
- `GEO_BOOTSTRAP_ROLE`：`resource_admin` 或 `customer_admin`

该命令不启动 HTTP 服务；在单个事务中建立身份、Host 与成员关系。相同配置重放不重置密码；不一致或部分已存在的身份拒绝覆盖。它是部署初始化，不是客户发布前的审批步骤。运营池管理另需 `GEO_OPERATOR_POOL_TENANT_ID` 指向相应资源管理租户。持久模式使用 Secure 会话 Cookie，浏览器验收应使用正确配置的 HTTPS 入口。

## 持久模型路由

数据库模式使用独立于本地开发凭据的配置，以下五项同时设置：

```text
GEO_PRODUCTION_AI_BASE_URL
GEO_TOKEN_CENTER_URL
GEO_TOKEN_CENTER_TOKEN
GEO_PRODUCTION_AGENT_BUNDLE_PATH
GEO_PRODUCTION_AGENT_BUNDLE_SHA256
```

部署者在 `tenant_model_routes` 中配置运营商、租户、可选项目、模型、默认/启用状态、Token Center 主体和 key/generation 映射。接口不接收客户或模型指定的网关、主体或密钥。项目级禁用记录不能被租户级默认路由绕过；调用时重新读取配置，轮换或撤销后不使用旧路由缓存。

这项映射是明确配置的模型调用能力，不等于套餐、余额、预算或费用结算已实现。Token Center 的普通推理也不等于消费端官方搜索或引用测量。

本地 `GEO_AI_*` 进程共享 key 仍仅允许 loopback 内存模式，不能用于 PostgreSQL 多租户部署。

## 原生内容工作流

先执行 `pnpm agent:bundle`，得到两个不入库的产物：

- `memeloop-agent-loop.bundle.mjs`：P00 对话入口。
- `memeloop-content-workflow.bundle.mjs`：MemeLoop 原生文档 fan-out 入口。

分别校验并配置实际文件的 SHA-256。内容工作流额外需要：

```text
GEO_CONTENT_BUNDLE_PATH
GEO_CONTENT_BUNDLE_SHA256
```

两项缺省时保留内容查询能力，但不假装可以启动生成；只配置一项或摘要不匹配时拒绝该错误部署配置。允许没有模型时装配已校验内容 bundle，用于恢复已封存内容的分发准备；新建周期内容与运行中的正文生成仍需模型。执行通过持久内容步骤及 fencing token 判定重放，不把 JS 内存状态当作生成结果。

P00/P08 可启动当前周期内容执行：若当前周期尚无封存文档清单，服务端以冻结周期配置及当前知识版本规划并封存，不复写已有封存清单。P08 提供恢复与取消；P09 保存新内容版本，历史版本不覆盖。PostgreSQL 模式下配置了内容 bundle 与模型后，30 秒分页扫描可启动活跃当前周期的缺失内容执行，并重领运行中的执行；无模型时仍扫描已封存但分发准备未完成的执行。迁移 `0019` 保存周期启动 claim、重试及错误码；独立调度租约、续租、失租中止和步骤 fencing 避免旧执行者继续提交。两轮自动事实修正新增 `geo.hostops.v7`，须部署配套生成 bundle 并更新摘要，契约见 `content-auto-repair.md`。下一轮增量优化、资产复用和完整富文本/媒体仍需继续交付。新增恢复逻辑的实库验收状态见 `WORKLOG.md`，不能仅凭本段配置说明推定已通过。

对话 Agent 的 queued 恢复独立于内容扫描：PostgreSQL 模式在运行时配置完成后，每 5 秒按迁移 `0022` 的队列索引分页发现未开始回合；未配置运行时则不领取。HTTP 与扫描都经过同一个数据库原子领取，多副本不会仅凭扫描结果开始重复执行。该机制不恢复 `running` 回合，也不提供中途工具副作用重放保证。`GEO_SINGLE_PROCESS_EXECUTOR` 的旧 running 启动对账仍默认关闭，多副本和滚动部署不得开启。

第一层交接可冻结独立的文档×平台分发执行清单，保存逐格覆盖、确定性渠道变体、逻辑发布意图与持久 outbox，并在 P12 查看/分页恢复。迁移 `0018` 已接正式发布消费者、发送前 attempt、报告投影及 P00 工具；原生工作流随后分页完成第二层准备，`distribution.prepared` 不表示发送成功。真实账号发布、官方搜索及完整闭环仍未验收；既有来源版本渠道计划保持独立，不用其成功代替生成版本验收。

## 浏览器及会话

API 设置 `GEO_BROWSER_RUNNER_URL` / `GEO_BROWSER_RUNNER_TOKEN`；runner 使用相同服务令牌。`GEO_CHANNEL_SECRET_KEY` 必须稳定保存，丢失会导致已有加密 session 无法恢复。

runner 默认使用固定 Playwright Chromium；部署环境也可显式设置 `GEO_BROWSER_CHANNEL=chrome` 或 `msedge` 使用已管理的浏览器。该配置仅服务端可设置，HTTP 请求不能指定浏览器可执行文件。CI 继续使用固定版本 Chromium；不同本地浏览器通过测试不替代固定版本回归。

账号首次登录及失效重连由账号主人在接入界面完成，后续任务复用服务端加密会话，无逐篇发布确认。真实身份、编辑器提交、公开读回和搜索引用必须分别取得实测证据。
