#!/usr/bin/env bash
set -euo pipefail

REPO="greepar/gh-proxy-rs"
INSTALL_DIR="/opt/gh-proxy"
SERVICE_FILE="/etc/systemd/system/gh-proxy.service"
ENV_FILE="/etc/default/gh-proxy"
SYSCTL_FILE="/etc/sysctl.d/99-z-gh-proxy-throughput.conf"
LISTEN="${GH_PROXY_LISTEN:-0.0.0.0:1555}"
VERSION="${VERSION:-}"
TUNE=false

usage() {
    cat <<'EOF'
Usage: install.sh [--tune] [--listen ADDRESS] [--uninstall]

  --tune            Apply TCP tuning for high-bandwidth, high-RTT links
  --listen ADDRESS  Listen address, default: 0.0.0.0:1555
  --uninstall       Remove gh-proxy and its systemd service
EOF
}

need_root() {
    if [[ "${EUID}" -ne 0 ]]; then
        echo "Run as root." >&2
        exit 1
    fi
}

apply_tuning() {
    cat >"${SYSCTL_FILE}" <<'EOF'
net.core.rmem_max = 33554432
net.core.wmem_max = 33554432
net.ipv4.tcp_rmem = 4096 131072 33554432
net.ipv4.tcp_wmem = 4096 131072 33554432
net.ipv4.tcp_timestamps = 1
net.ipv4.tcp_mtu_probing = 1
EOF
    sysctl -p "${SYSCTL_FILE}"
}

uninstall_proxy() {
    systemctl disable --now gh-proxy.service 2>/dev/null || true
    rm -f "${SERVICE_FILE}" "${ENV_FILE}" "${SYSCTL_FILE}"
    rm -rf "${INSTALL_DIR}"
    systemctl daemon-reload
    systemctl reset-failed gh-proxy.service 2>/dev/null || true
    echo "gh-proxy removed. Reboot to fully reset live TCP tuning values."
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tune)
            TUNE=true
            shift
            ;;
        --listen)
            [[ $# -ge 2 ]] || { usage >&2; exit 1; }
            LISTEN="$2"
            shift 2
            ;;
        --uninstall)
            need_root
            uninstall_proxy
            exit 0
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 1
            ;;
    esac
done

need_root

[[ -d /run/systemd/system ]] || { echo "systemd is required." >&2; exit 1; }
[[ "${LISTEN}" != *[$'\n\r ']* ]] || { echo "Invalid listen address." >&2; exit 1; }

case "$(uname -m)" in
    x86_64|amd64) PLATFORM="linux-x86_64" ;;
    aarch64|arm64) PLATFORM="linux-aarch64" ;;
    *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

for command in curl tar sha256sum systemctl install getent groupadd useradd; do
    command -v "${command}" >/dev/null || { echo "Missing command: ${command}" >&2; exit 1; }
done

if [[ -z "${VERSION}" ]]; then
    latest_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")
    VERSION="${latest_url##*/}"
fi

archive="gh-proxy-${VERSION}-${PLATFORM}.tar.gz"
base_url="https://github.com/${REPO}/releases/download/${VERSION}"
tmp_dir=$(mktemp -d)
trap 'rm -rf "${tmp_dir}"' EXIT

curl -fL --retry 3 -o "${tmp_dir}/${archive}" "${base_url}/${archive}"
curl -fL --retry 3 -o "${tmp_dir}/${archive}.sha256" "${base_url}/${archive}.sha256"
(
    cd "${tmp_dir}"
    sha256sum -c "${archive}.sha256"
    tar -xzf "${archive}"
)

if ! getent group gh-proxy >/dev/null 2>&1; then
    groupadd --system gh-proxy
fi
if ! id -u gh-proxy >/dev/null 2>&1; then
    nologin=$(command -v nologin || printf '/usr/sbin/nologin')
    useradd --system --gid gh-proxy --home-dir /nonexistent --shell "${nologin}" gh-proxy
fi

install -d -m 0755 -o root -g root "${INSTALL_DIR}"
install -m 0755 -o root -g root "${tmp_dir}/gh-proxy" "${INSTALL_DIR}/gh-proxy.new"
mv -f "${INSTALL_DIR}/gh-proxy.new" "${INSTALL_DIR}/gh-proxy"

cat >"${ENV_FILE}" <<EOF
GH_PROXY_LISTEN=${LISTEN}
EOF

cat >"${SERVICE_FILE}" <<'EOF'
[Unit]
Description=gh-proxy Pingora reverse proxy
Documentation=https://github.com/greepar/gh-proxy-rs
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=gh-proxy
Group=gh-proxy
EnvironmentFile=/etc/default/gh-proxy
ExecStart=/opt/gh-proxy/gh-proxy --listen ${GH_PROXY_LISTEN}
Restart=always
RestartSec=2
LimitNOFILE=1048576
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/tmp
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true

[Install]
WantedBy=multi-user.target
EOF

if [[ "${TUNE}" == true ]]; then
    apply_tuning
fi

systemctl daemon-reload
systemctl enable --now gh-proxy.service
systemctl restart gh-proxy.service

echo "Installed gh-proxy ${VERSION} on ${LISTEN}"
systemctl --no-pager --full status gh-proxy.service
