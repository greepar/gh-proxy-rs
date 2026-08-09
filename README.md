# gh-proxy

基于 Pingora 的 GitHub / Docker Hub 流式反向代理，默认监听 `0.0.0.0:1555`。

## 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/greepar/gh-proxy-rs/main/install.sh | sudo bash
```

指定监听地址并启用高延迟链路调优：

```bash
curl -fsSL https://raw.githubusercontent.com/greepar/gh-proxy-rs/main/install.sh | sudo bash -s -- --listen 0.0.0.0:1555 --tune
```

管理服务：

```bash
systemctl status gh-proxy
journalctl -u gh-proxy -f
curl -H 'Host: gh.qwq.lu' http://127.0.0.1:1555/healthz
```

卸载：

```bash
curl -fsSL https://raw.githubusercontent.com/greepar/gh-proxy-rs/main/install.sh | sudo bash -s -- --uninstall
```

## 域名

公网 HTTPS 反代到 `127.0.0.1:1555`，并保留原始 `Host`：

| Host | 上游 |
| --- | --- |
| `gh.qwq.lu` | GitHub、Release、Raw、Codeload |
| `docker.qwq.lu` | `registry-1.docker.io` |
| `auth.docker.qwq.lu` | `auth.docker.io` |

```bash
git clone https://gh.qwq.lu/owner/repository.git
```

Docker `/etc/docker/daemon.json`：

```json
{"registry-mirrors":["https://docker.qwq.lu"]}
```

## 系统调优

`--tune` 会写入 `/etc/sysctl.d/99-z-gh-proxy-throughput.conf`：

```conf
net.core.rmem_max = 33554432
net.core.wmem_max = 33554432
net.ipv4.tcp_rmem = 4096 131072 33554432
net.ipv4.tcp_wmem = 4096 131072 33554432
net.ipv4.tcp_timestamps = 1
net.ipv4.tcp_mtu_probing = 1
```

手动应用：

```bash
sudo sysctl -p /etc/sysctl.d/99-z-gh-proxy-throughput.conf
```

调优主要改善高 RTT 链路的单连接速度；多连接总速度仍受线路、丢包和 CPU 限制。

## 本地构建

```bash
cargo build --release
./target/release/gh-proxy --listen 0.0.0.0:1555
```
