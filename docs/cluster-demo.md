# 隔离测试集群演示包装

本文只描述可复用的容器包装和一次性身份初始化。旧本地演示数据不迁移；
集群使用已经准备好的 PostgreSQL/PV 和受控的浏览器执行器。所有域名、账号、
租户 ID、数据库凭据和模型凭据都由部署环境或 Secret 管理器提供，不写入镜像、
仓库或命令日志。

## 镜像内容

根目录 [`Dockerfile`](../Dockerfile) 构建 API 镜像：

- Node 22.17.0 和锁定的 pnpm 10.17.1 阶段从工作区生成两份批准的
  MemeLoop ESM bundle；
- Rust 1.96 release 阶段编译 `geo-app`；
- 最终镜像以非 root 用户运行 `/usr/local/bin/geo-app`，监听容器端口
  `8080`；
- bundle 放在 `/opt/geo/bundles/`，并随镜像提供 `SHA256SUMS`；
- bundle、API 的第三方依赖声明和 Apache-2.0 根许可证统一放在
  `/usr/share/doc/memeloop-geo/`；
- 不设置任何模型、Token Center、数据库或账号凭据环境变量。

[`deploy/demo/Dockerfile.web`](../deploy/demo/Dockerfile.web) 使用同一份
pnpm lockfile 构建 React 静态资源，再交给非 root Nginx 镜像。Nginx 监听
`8080`，把同源 `/api`（含 SSE）转发到同一命名空间内的 `api:8080`，
并为 React Router 提供 `index.html` fallback。API 代理关闭响应缓冲并保留
长读取超时；根许可证也作为静态 `/LICENSE` 文件随 Web 镜像提供。部署时 API
Service 应使用名称 `api`，否则需要在镜像构建前调整上游名称。

构建示例（标签应由 CI 替换为不可变提交或镜像 digest）：

```text
docker build --file Dockerfile --tag <registry>/geo-api:<commit> .
docker build --file deploy/demo/Dockerfile.web --tag <registry>/geo-web:<commit> .
```

API 镜像中的摘要可用只读方式查看，作为部署配置的输入：

```text
docker run --rm --entrypoint cat <registry>/geo-api:<commit> \
  /opt/geo/bundles/SHA256SUMS
```

这些命令不会创建或迁移本地演示数据。Docker 构建仍需要从锁定的基础镜像、
Cargo registry 和 pnpm registry 获取依赖；本任务未执行构建或下载。

本包装尚未在当前工作区执行 Docker 构建、镜像启动或 Nginx 配置检查。首次 CI
应验证 Rust/V8 release 二进制在目标架构的动态库、bundle 摘要、WebSocket/SSE
长连接、100 MB 上传和 API Service DNS（`api`）；失败时保持镜像未发布。

## 集群注入配置

API 必须使用 PostgreSQL 模式；至少由 Secret/受控配置注入：

```text
DATABASE_URL=<PostgreSQL DSN from the secret manager>
GEO_BIND_ADDR=0.0.0.0:8080
GEO_ALLOWED_ORIGINS=https://<demo-host>
```

首次部署在 API 镜像上运行一次 `--bootstrap` Job。仓库内演示清单让 Job 从
运行时 Secret 取得 `DATABASE_URL`，并从 bootstrap Secret 取得下面的
`GEO_BOOTSTRAP_*` 变量；`--bootstrap` 在解析普通 HTTP/模型配置前退出。密码只
通过 Secret 注入，不能作为命令行参数。所有 ID、slug、显示名和精确 Host 由
部署者生成并保存到受控配置：

```text
GEO_BOOTSTRAP_OPERATOR_ID=<uuid>
GEO_BOOTSTRAP_OPERATOR_SLUG=<operator slug>
GEO_BOOTSTRAP_OPERATOR_NAME=<operator display name>
GEO_BOOTSTRAP_HOST=<exact browser Host, with port only when required>
GEO_BOOTSTRAP_TENANT_ID=<uuid>
GEO_BOOTSTRAP_TENANT_SLUG=<tenant slug>
GEO_BOOTSTRAP_TENANT_NAME=<tenant display name>
GEO_BOOTSTRAP_USER_ID=<uuid>
GEO_BOOTSTRAP_LOGIN_NAME=<email-shaped login>
GEO_BOOTSTRAP_USER_NAME=<user display name>
GEO_BOOTSTRAP_PASSWORD=<secret manager value>
GEO_BOOTSTRAP_ROLE=customer_admin
```

使用同一组身份配置重放是幂等的；不要用不同值覆盖已经存在的身份。Job
完成后不保留 bootstrap Secret 到镜像或日志。迁移失败或连接失败会使 Job/API
失败，不会回退到内存数据库。

### 生产模型能力（可选）

没有模型配置时 API 仍可启动，但 P00/内容生成会明确返回
`capability_missing`；系统不会生成假的模型回答。需要启用生产模型时，以下
五项必须同时由 Secret/受控配置提供，不能只设置其中一项：

```text
GEO_PRODUCTION_AI_BASE_URL=<approved model gateway URL>
GEO_TOKEN_CENTER_URL=<trusted Token Center URL>
GEO_TOKEN_CENTER_TOKEN=<service secret>
GEO_PRODUCTION_AGENT_BUNDLE_PATH=/opt/geo/bundles/memeloop-agent-loop.bundle.mjs
GEO_PRODUCTION_AGENT_BUNDLE_SHA256=<matching line from /opt/geo/bundles/SHA256SUMS>
```

数据库中的租户模型路由仍必须由部署者显式配置；普通推理能力不等于官方
搜索、引用或真实渠道测量验收。生产镜像绝不包含模型密钥。

原生内容 fan-out bundle 额外使用以下两项，必须一起设置或一起缺省：

```text
GEO_CONTENT_BUNDLE_PATH=/opt/geo/bundles/memeloop-content-workflow.bundle.mjs
GEO_CONTENT_BUNDLE_SHA256=<matching line from /opt/geo/bundles/SHA256SUMS>
```

无模型时，经过摘要校验的内容 bundle 仍可用于已经封存内容的分发准备；
新的正文生成仍需要生产模型配置。缺少或错误摘要会使 API 拒绝启动相关配置。

### 浏览器交互执行器部署

[`deploy/demo/browser-runner.yaml`](../deploy/demo/browser-runner.yaml) 提供一个不含
命名空间的可复用演示 `Deployment` 和内部 `ClusterIP Service`。它只消费 CI 已经
构建并验证过的 `packages/browser-runner/Dockerfile.interactive` 镜像：镜像内已经
包含 Playwright/Chromium、TigerVNC、websockify 和 Node 依赖；noVNC 客户端由前端提供。Pod 启动时
不会运行 `apt`、`npm` 或源码安装步骤。清单中的 GHCR 镜像是故意使用的通用
占位引用；部署者必须从成功的 interactive-desktop CI 运行中取得对应提交的
不可变 digest，替换 `image` 后再应用，不能把 `latest` 或未经验证的标签当作
部署凭据。

旧的 runtime-install / app-volume 方案不得用于这个执行器。把依赖安装或源码
目录放进启动时的 `emptyDir` 会把大文件写入临时存储，在配额紧张时可能驱逐
Pod，也会绕过 CI 已验证的镜像内容；此清单只为 `/tmp` 提供 1 GiB 磁盘
`emptyDir`，为 `/dev/shm` 提供 1 GiB 内存 `emptyDir`，不挂载 `/app` 或源码。

在目标隔离命名空间中，由秘密管理器创建名为 `geo-browser-runner` 的 Secret，
并提供 `GEO_BROWSER_RUNNER_TOKEN` 键。该值同时配置给 API 的
`GEO_BROWSER_RUNNER_TOKEN`；API 在集群内使用
`http://browser-runner:38080` 访问 Service。不要把令牌写入清单、镜像、前端
变量或命令日志。应用清单和 Secret 由部署环境选择命名空间，例如：

```text
kubectl --namespace <demo-namespace> apply -f deploy/demo/browser-runner.yaml
kubectl --namespace <demo-namespace> rollout status deployment/browser-runner
```

容器以 UID/GID 1000 的非 root 用户运行，关闭 service-account token 自动挂载、
提权和额外 capabilities，并保留 `RuntimeDefault` seccomp；就绪检查只验证
容器的 TCP 38080 端口。资源请求/上限是 headed Chromium/VNC 的保守基线，用于
集群容量调度，不是业务并发或租户配额承诺，应按实际节点和 CI/运行时观测调整。

这是隔离测试集群中的单副本演示包装，不是生产级每会话隔离，也不代替
NetworkPolicy、TLS/Ingress、出口控制或 Secret 生命周期管理。当前 runner
进程和浏览器运行时本身不能作为租户边界；生产部署需要由平台为交互会话安排
受限的容器/Pod，并在集群侧补齐网络策略和入口认证。清单中的 ClusterIP 不应
直接暴露到公网。

### 其他服务端能力

只有已经部署对应服务时才注入下列受支持配置；它们不是前端变量：

- `GEO_BROWSER_RUNNER_URL`、`GEO_BROWSER_RUNNER_TOKEN`：受控浏览器执行器；
- `GEO_CHANNEL_SECRET_KEY`：跨重启保持不变的加密密钥；
- `GEO_OPERATOR_POOL_TENANT_ID`：运营账号池的资源管理租户；
- `GEO_PDF_PARSER_URL`、`GEO_OFFICE_PARSER_URL`：隔离解析服务。

不要在集群设置 `GEO_DEV_*`、`GEO_AI_*` 或 `GEO_DEV_PERSISTENT_AI`。这些变量
属于 loopback-only 开发模式；应用会拒绝将 process-wide 开发密钥与公开绑定或
正式 PostgreSQL 模式混用。多副本部署也不要启用
`GEO_SINGLE_PROCESS_EXECUTOR`，该开关只适用于整个数据库唯一执行进程。

## 发布顺序和边界

1. 使用 CI 构建并推送 API、Web 镜像，部署时固定镜像 digest；不要把 `.env`、
   本地 node_modules、Cargo target 或浏览器登录态复制进构建上下文。
2. 确认现有 PostgreSQL/PV、API Service（端口 8080，Service 名称 `api`）
   和 Web Service（端口 8080）位于同一隔离命名空间。
3. 先运行一次 bootstrap Job，再启动 API Deployment。仓库内
   [`deploy/demo/kubernetes.yaml`](../deploy/demo/kubernetes.yaml) 提供可审阅的演示
   Deployment、Service 和 Job 基线；部署环境仍需替换镜像 digest 并按 Secret
   管理器注入值。API 就绪探针使用
   `/health/live` 和 `/health/ready`；迁移、连接和显式配置失败应保持未就绪或
   直接退出。
4. Web 入口只暴露 Nginx；浏览器请求保持同源，API、SSE、Cookie 和 CSRF
   通过 `/api` 代理，不把数据库、模型或浏览器执行器地址放进前端构建变量。
5. 浏览器/账号执行和外部模型凭据继续使用独立的网络策略与 Secret；本包装不
   声称真实发布、官方搜索、计费或容量验收已完成。

这是便携演示包装，不是完整生产基础设施清单。仓库内清单仍需部署者补充
Ingress/TLS、持久卷、备份、网络策略、镜像 digest 和 Secret 引用；CI 发布流程
负责构建并推送镜像，不替代集群验收。
