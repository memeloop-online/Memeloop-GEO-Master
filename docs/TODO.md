# 实施待办

只保留未完成工作和当前验收目标；完成项从本文件移除，细节写入 `WORKLOG.md`。
无历史对话的接手入口见 `HANDOFF.md`。

## 交付时间（北京时间）

- 2026-10-01 18:00：P00 Provider、真实 bundle、知识分支与恢复纵切；每 5 分钟检查可交付增量，只有验证通过的改动提交。
- 2026-10-02 12:00：发布连接器、账号/出口配置、幂等与公开查回纵切。
- 2026-10-02 20:00：独立 AI 搜索测量、引用证据、周报 reduce 纵切。
- 2026-10-03 12:00：全链路验收和交接；10 月 3 日为项目验收截止日，以上为内部目标，不代表已完成。

## 当前：W00 AI 工作台与 Agent Runtime

- [ ] 验收启动期重启对账：已实现显式 `GEO_SINGLE_PROCESS_EXECUTOR=true` 下 running → failed；需实库回归、部署文档和多副本租约方案。默认关闭，滚动部署或多副本不得开启；queued 恢复仍未实现。
- [ ] 把取消接到隔离体：`cancel_turn` 语义已正确（`finish_run` 不会覆盖 `Cancelled`），但取消不触达隔离体，turn 仍跑到 deadline 才结束。`HostBridge::with_cancellation` 已备好接口。
- [ ] 驱动中途 checkpoint 恢复与 Rust-owned tool-call ledger：executor 已接完成结果存档，但它不是中途恢复；工具 intent/attempt/outcome 必须由 Rust 实际调用前后写入，不能信任 JS 自报。
- [ ] 真实 MemeLoop 单回合已通过 64 MiB V8 探针；接下来接应用启动、工具循环、消息/附件契约、持久恢复与产物摘要。生成文件不入库，分发时携带第三方许可。
- [ ] 接应用 Provider 与租户 Token Center：HTTP transport、scope 路由和模型白名单已有测试；真实凭据解析、费用与应用装配未接，默认仍 capability_missing。
- [ ] 实现 GEO 工具桥接：`manifest_read`、`publish_submit`、`measure_sample` 目前如实返回 `capability_missing`。
- [ ] 跑通“附件入库 → 带来源回答 → 两个文档分支 fan-out → 恢复 → 结果汇总”首个纵切。

## 排队：W03 企业知识库

- [ ] 接入持久对象存储与 URL、PDF、DOCX、XLSX、CSV、OCR 解析器；补齐来源替换、重试、停用和版本生命周期。
- [ ] 建立产品/结构化事实提取、冲突判定、用途边界与可追溯证据；接入向量检索和可选 LLM 回答。
- [ ] 文档清单已支持项目级有限规划与封存；补产品/事实级规划、精确增量依赖、后续 revision、P07 UI，并实库验收迁移 0008。

## W04/W09 外部 AI 渠道与竞品验证

- [ ] 为每个 AI 供应商实现能力探针，分别验证普通模型、官方搜索、引用、流式和结构化输出；API、消费端网页和移动端观测面独立建模。
- [ ] 按 `provider + model + search_mode + surface + market + language` 冻结测量协议，保存原始答案、引用、截图/响应、request ID、用量、出口和连接器版本；降级只记为代理观测或缺测。
- [ ] 验证文章发布器的草稿、渠道变体、发布、更新、公开查回、结果未知和幂等重试；复用统一的账号、执行环境、出口 IP/IP 池和证据模型。

## W01 生产加固

- [ ] 完成所有 PostgreSQL repository 的事务级租户 scope 后启用 FORCE RLS，并使用非 bypass 角色实测。

## 主架构检查点

- [ ] W03/W05 完成文档生成与版本演进；项目级 fan-out 清单已有 API，尚未接完整 AI 生成。
- [ ] W06/W08 建立文档 × 平台 fan-out 覆盖矩阵和独立发布分支。
- [ ] W09 建立发布证据 + AI 渠道测量 reduce、不可变周报和下一轮增量。

## 待补验证

- [ ] 以非 bypass 角色实测 FORCE RLS。迁移、W02 原子启动、租户可见性、Agent 持久化与重启恢复、run 声明/完成已可用一次性 PostgreSQL 实库验证通过（14/14，见 `WORKLOG.md`）；`0004_tenant_rls.sql` 仍是安全 no-op。

## 纵切目标

- [ ] 知识 → 基线 → 策略 → 内容 → 检查 → 账号池 → WordPress/Ghost 发布 → 验证 → 复测。
