# gh-proxy

基于 Cloudflare Pingora `0.8.1` 的极简高性能 GitHub 和 Docker Hub 反向代理。请求与响应全程流式转发，不缓存、不压缩、不聚合大文件。

## 域名白名单

程序只接受以下 `Host`，路径和查询参数完全不变：

| 代理域名 | 上游 |
| --- | --- |
| `gh.qwq.lu` | `github.com` |
| `docker.qwq.lu` | `registry-1.docker.io` |
| `auth.docker.qwq.lu` | `auth.docker.io` |

例如：

```text
https://gh.qwq.lu/owner/repository
                 ↓
https://github.com/owner/repository
```

Git clone 直接使用：

```bash
git clone https://gh.qwq.lu/owner/repository.git
```

GitHub 返回的 Release、Raw、Codeload 等外部域名重定向会由客户端直接访问，不做额外路由和 URL 改写。

## Docker Hub

为以下两个域名配置 DNS 和 HTTPS：

```text
docker.qwq.lu
auth.docker.qwq.lu
```

Docker daemon 的 `/etc/docker/daemon.json`：

```json
{
  "registry-mirrors": ["https://docker.qwq.lu"]
}
```

Registry 返回的 Docker Hub token 地址会自动改为 `https://auth.docker.qwq.lu/`。

## 运行

默认监听 `0.0.0.0:1555`，worker 数自动等于可用 CPU 数，其余连接池和超时使用程序内置高吞吐配置：

```bash
cargo run --release
```

只有监听地址可选：

```bash
./target/release/gh-proxy --listen 127.0.0.1:1555
```

或：

```bash
GH_PROXY_LISTEN=127.0.0.1:1555 ./target/release/gh-proxy
```

健康检查：

```bash
curl http://127.0.0.1:1555/healthz
```

## 部署

```bash
docker compose up -d --build
```

生产环境建议由 Caddy、nginx、HAProxy 或云负载均衡器负责公网 HTTPS，并将三个域名转发到 Pingora 的 `1555` 端口。转发时必须保留原始 `Host`。

## Release 构建

推送 `v*` 标签后，GitHub Actions 会自动创建 Release，并上传以下平台产物：

- Linux x86_64，Zig 交叉编译，兼容 glibc 2.17 及以上。
- Linux ARM64，Zig 交叉编译，兼容 glibc 2.17 及以上。
- Windows x86_64 GNU，Zig 交叉编译。
- macOS x86_64 和 ARM64，使用 GitHub macOS runner 与 Apple SDK 编译。

Linux 和 Windows 构建使用 `cargo-zigbuild`。macOS 目标必须使用 Apple SDK，因此不能仅靠 Zig 在 Linux runner 上合法、可靠地生成。

## 性能设计

- Pingora 流式转发，无业务层 body 缓冲和复制。
- worker 自动使用全部可用 CPU，启用 work stealing。
- 每 worker 保留较大的上游 keepalive 连接池。
- 上游优先协商 HTTP/2，并兼容 HTTP/1.1。
- 不对 zip、tar、Git pack、OCI layer 做无意义的二次压缩。
- Release 构建启用 Fat LTO、单 codegen unit、符号剥离和 abort panic。
- 非错误日志默认关闭，减少高并发路径上的日志开销。

Linux 生产环境建议设置：

```bash
ulimit -n 1048576
```

本服务不是持久化缓存，也不会绕过 GitHub 或 Docker Hub 的鉴权、限流和服务条款。
