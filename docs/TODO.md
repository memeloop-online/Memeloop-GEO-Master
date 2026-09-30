# 实施待办

只保留未完成工作和当前验收目标；完成项从本文件移除，细节写入 `WORKLOG.md`。
无历史对话的接手入口见 `HANDOFF.md`。

## 当前：W00 AI 工作台与 Agent Runtime

- [ ] 完成 Rust JS Runtime、MemeLoop server bundle、Promise/ESM/取消/隔离兼容探针。
- [ ] 将 Conversation、Message、Turn、Run、Checkpoint、ToolCallLedger、附件引用与 SSE 落到 PostgreSQL。
- [ ] 建立隔离的 Rust JS Worker、MemeLoop loop bundle、模型 Provider host op 和 GEO 工具桥接。
- [ ] 跑通“附件入库 → 带来源回答 → 两个文档分支 fan-out → 恢复 → 结果汇总”首个纵切。

## 排队：W03 企业知识库

- [ ] 接入持久对象存储与 URL、PDF、DOCX、XLSX、CSV、OCR 解析器；补齐来源替换、重试、停用和版本生命周期。
- [ ] 建立产品/结构化事实提取、冲突判定、用途边界与可追溯证据；接入向量检索和可选 LLM 回答。
- [ ] 由 KnowledgeRelease 驱动有限、版本化的文档覆盖清单，只失效受影响分支。

## W04/W09 外部 AI 渠道与竞品验证

- [ ] 为每个 AI 供应商实现能力探针，分别验证普通模型、官方搜索、引用、流式和结构化输出；API、消费端网页和移动端观测面独立建模。
- [ ] 按 `provider + model + search_mode + surface + market + language` 冻结测量协议，保存原始答案、引用、截图/响应、request ID、用量、出口和连接器版本；降级只记为代理观测或缺测。
- [ ] 验证文章发布器的草稿、渠道变体、发布、更新、公开查回、结果未知和幂等重试；复用统一的账号、执行环境、出口 IP/IP 池和证据模型。
- [ ] 通过人工完成一次 geo.istarshine.com 图形验证码后，再只读核验其账号内的 AI 供应商接入、搜索模式、测量证据和文章发布流程；未验证前不把其能力写成已实现。

## W01 生产加固

- [ ] 完成所有 PostgreSQL repository 的事务级租户 scope 后启用 FORCE RLS，并使用非 bypass 角色实测。
- [ ] 将 SSE 改为持久化游标恢复，并在长连接期间重新校验会话与成员关系。

## 主架构检查点

- [ ] W03/W05 建立有限、版本化的知识文档 fan-out 清单。
- [ ] W06/W08 建立文档 × 平台 fan-out 覆盖矩阵和独立发布分支。
- [ ] W09 建立发布证据 + AI 渠道测量 reduce、不可变周报和下一轮增量。

## 待补验证

- [ ] 以 `GEO_TEST_DATABASE_URL` 在空 PostgreSQL 上执行迁移、W02 原子启动、租户可见性、RLS 和重启恢复集成测试。

## 纵切目标

- [ ] 知识 → 基线 → 策略 → 内容 → 检查 → 账号池 → WordPress/Ghost 发布 → 验证 → 复测。
