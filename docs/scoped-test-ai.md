# 项目级模型测试部署

本文细化冻结规格 0.10 的显式部署凭据模式。它用于隔离测试，不代替多租户生产
Token Center、权益、费用或凭据轮换验收。实现与测试结果以 `WORKLOG.md` 为准。

## 配置边界

生产默认仍使用正式 Token Center 服务凭据和持久租户模型映射。
测试部署可以显式注入一份推理凭据，仅供一个精确的运营商、租户和项目使用。
缺少配置不会自动借用网页测量账号或解析模型凭据；API 推理也不冒充消费端搜索测量。
已有本机开发模式及其 loopback 约束保持不变。

以下配置必须同时存在：

```text
GEO_SCOPED_TEST_AI=true
GEO_SCOPED_TEST_AI_OPERATOR_ID=<operator uuid>
GEO_SCOPED_TEST_AI_TENANT_ID=<tenant uuid>
GEO_SCOPED_TEST_AI_PROJECT_ID=<project uuid>
GEO_SCOPED_TEST_AI_BASE_URL=<approved OpenAI-compatible endpoint>
GEO_SCOPED_TEST_AI_API_KEY=<injected secret>
GEO_SCOPED_TEST_AI_MODEL=<approved model>
GEO_SCOPED_TEST_AGENT_BUNDLE_PATH=/opt/geo/bundles/memeloop-agent-loop.bundle.mjs
GEO_SCOPED_TEST_AGENT_BUNDLE_SHA256=<digest from the same tested image>
```

要求 PostgreSQL、非空且非 nil 的三层身份和与镜像一致的工具 bundle 摘要。
不允许只填写部分配置、关闭开关后残留配置，或同时设置旧开发/生产 AI 配置。
不会在配置失败时切换到其他凭据来源。

凭据通过 Kubernetes `secretKeyRef` 或同等秘密管理机制注入，不写入镜像、
公开清单、命令参数、日志或前端变量。可以明确引用既有测试 Secret 的键，
但不得自动扫描或复制其他服务凭据。
需要原生内容工作流时，继续配置现有 `GEO_CONTENT_BUNDLE_PATH` 和
`GEO_CONTENT_BUNDLE_SHA256`；它们同样必须来自正在部署的镜像。

## 请求隔离

模型调用在取凭据或发出请求之前比较完整 `TenantScope`。
运营商、租户、项目任一不符，或缺少项目，均拒绝；不将租户范围视为项目授权。
P00 和内容生成使用同一受限 Provider；模型不能选择其他 endpoint、凭据或模型。
既有身份会话、成员权限、Origin/CSRF、工具作用域及副作用账本继续生效。

这是一种部署配置授权，不提供 Token Center 的实时撤销/代际检查。
轮换测试凭据后需要滚动部署；只用于隔离测试环境，不作为通用生产回退。

## 验证与退出

- 验证完整、部分、混合、错误身份、内存模式与错误摘要的启动行为。
- 验证指定项目调用成功，其他运营商/租户/项目及替换模型在出站前被拒绝。
- 验证真实工作台回合、持久消息读回和只读工具调用；构建与桩测试不替代实测。
- 退出测试模式时同时移除全部测试配置，再按生产配置接通正式租户模型路由。

公开交接记录只保留通用行为和脱敏验证结果；实际身份、入口和凭据由部署环境保存。
