# Token Center 接入契约

实现依据：公开上游 `memeloop-online/memeloop-token-center`，检查版本
`186e92b16c94c2ced75477f1e2de87358ab17662`。本文补充实施接缝，不改变产品规格。

## 身份与凭据

GEO 的 operator/tenant/principal 映射必须由 Rust 根据已认证 scope 选择，
不得接受 JS 指定的服务令牌、上游租户或 endpoint。
服务令牌和下游推理 key 是不同凭据，不能互换。

| 接口 | 权限和用途 |
| --- | --- |
| `POST /internal/v1/keys` | 服务令牌 `keys:write`；创建租户/主体凭据，返回 key_id、account_id、generation、key 等。不是普通推理请求。 |
| `GET /internal/v1/keys` | `keys:read`；按 tenant_external_id、principal_external_id、key_id 查询元数据，不返回明文 key。 |
| `POST /internal/v1/keys/{key_id}/copy` | `keys:write` 且租户匹配；复制有效凭据，响应禁止缓存。 |
| `POST /internal/v1/integrations/memeloop-cloud/principals/ensure` | `keys:write` 且租户匹配；取得或创建稳定 cloud credential，返回可选 key。不会自动授予路由、充值或付费权益。 |
| `POST /internal/v1/keys/{key_id}/rotate` | 要求 Idempotency-Key；轮换 generation，旧代失效。 |
| `PATCH /internal/v1/keys/{key_id}/status` | active/suspended/revoked；revoked 终态并清除明文。 |
| `GET /internal/v1/keys/{key_id}/limits` | 读取额度快照，不代表 GEO 业务订阅已付费。 |

推理 gateway `/v1/chat/completions` 等接受下游 key（Bearer 或 x-api-key），
不接受内部服务令牌。首次创建时 route_ids/route_group_ids 为空意味着无可用路由。
不要通过重复创建新 key 回避账户额度或撤销状态。

## GEO 的实现约束

- 持久化租户、主体与稳定 key_id 的映射；私密值仅在服务端 secret provider 内存或专用密钥存储中处理。
- 分开检查 GEO 订阅权益、Token Center key 状态、路由和实际额度；“拿到 key”不等于可以消费。
- 明文 copy/ensure 响应不得进入浏览器、Agent DTO、checkpoint、工具日志或调试输出。
- 超时/取消跨凭据解析与推理共用截止时间；不要让每段操作重新获得完整预算。
- 轮换/撤销应使映射缓存失效，不能将凭据无限缓存。额度由上游实际策略执行，GEO 保留自身费用账本和请求关联。
- key 元数据中的 credential_copy_available 才说明当前代可复制；缺少存储明文时 copy 可返回 404，不能假定任何现有 key 都能取回。
- 初次接入先使用只读 key 元数据校验与显式预置映射，再调用 copy 解析已配置凭据；
  创建、充值、路由变更是单独的管理操作，不应在一次聊天失败后自动触发。

## 来源与待验收

上游源码：`src/api/routes/control.rs`、`src/api/routes/gateway.rs`、
`src/api/auth.rs`、`src/api/credentials/client.rs`、`src/api/credentials/service.rs`、
`src/api/cloud_principals.rs`、`src/db/credentials/keys.rs`、`src/model.rs`、
`schemas/key-create.schema.json`、`openapi/openapi.yaml`。

当前仓库有 TokenCenter trait、scope-aware Provider bridge 和 HTTP transport。
`crates/provider/src/token_center.rs` 已实现预置映射的元数据校验与 copy 协议，
六项本地 HTTP 契约测试覆盖成功、作用域/状态/代际不匹配、错误脱敏等。
正式租户映射的持久配置与应用装配尚未接通；开发环境进程级 key 不能冒充这一集成。
验收需要：跨租户拒绝、未付费拒绝、无路由、余额不足、撤销、轮换、响应缺 key、
请求超时和凭据不进入任何持久消息。测试使用本地契约服务器和测试凭据。
