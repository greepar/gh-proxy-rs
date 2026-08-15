#!/usr/bin/env bash
set -euo pipefail

REPO="greepar/gh-proxy-rs"
INSTALL_DIR="/opt/gh-proxy"
CONFIG_FILE="${INSTALL_DIR}/gh-proxy.conf"
LINUX_SERVICE_FILE="/etc/systemd/system/gh-proxy.service"
LINUX_SYSCTL_FILE="/etc/sysctl.d/99-z-gh-proxy-throughput.conf"
MACOS_PLIST_FILE="/Library/LaunchDaemons/com.greepar.gh-proxy.plist"
MACOS_LOG_FILE="/var/log/gh-proxy.log"
DEFAULT_LISTEN="0.0.0.0:1555"
DEFAULT_GITHUB_HOST=""
DEFAULT_DOCKER_HOST=""
DEFAULT_DOCKER_AUTH_HOST=""
OS=""
PLATFORM=""

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

need_root() {
    [[ "${EUID}" -eq 0 ]] || die "run with sudo or as root"
}

read_input() {
    local prompt="$1"
    local default="${2:-}"
    local value

    if [[ -r /dev/tty ]]; then
        if [[ -n "${default}" ]]; then
            read -r -p "${prompt} [${default}]: " value </dev/tty || true
        else
            read -r -p "${prompt}: " value </dev/tty || true
        fi
    else
        value=""
    fi

    printf '%s' "${value:-${default}}"
}

confirm() {
    local answer
    answer=$(read_input "$1 (y/N)" "n")
    [[ "${answer,,}" == "y" || "${answer,,}" == "yes" ]]
}

detect_platform() {
    case "$(uname -s)" in
        Linux) OS="linux" ;;
        Darwin) OS="macos" ;;
        *) die "unsupported operating system: $(uname -s)" ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64) PLATFORM="${OS}-x86_64" ;;
        aarch64|arm64) PLATFORM="${OS}-aarch64" ;;
        *) die "unsupported architecture: $(uname -m)" ;;
    esac
}

validate_listen() {
    local address="$1"
    local host port

    if [[ "${address}" =~ ^\[([0-9A-Fa-f:]+)\]:([0-9]{1,5})$ ]]; then
        host="${BASH_REMATCH[1]}"
        port="${BASH_REMATCH[2]}"
    elif [[ "${address}" =~ ^([A-Za-z0-9._-]+):([0-9]{1,5})$ ]]; then
        host="${BASH_REMATCH[1]}"
        port="${BASH_REMATCH[2]}"
    else
        die "invalid listen address: ${address}"
    fi

    [[ -n "${host}" && "${port}" -ge 1 && "${port}" -le 65535 ]] || die "invalid listen address: ${address}"
}

validate_hostname() {
    local host="$1"
    [[ "${host}" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*[A-Za-z0-9]$ ]] || die "invalid hostname: ${host}"
}

read_listen() {
    if [[ -r "${CONFIG_FILE}" ]]; then
        # shellcheck disable=SC1090
        source "${CONFIG_FILE}"
        printf '%s' "${GH_PROXY_LISTEN:-${DEFAULT_LISTEN}}"
    else
        printf '%s' "${DEFAULT_LISTEN}"
    fi
}

read_config_value() {
    local name="$1"
    local default="$2"
    if [[ -r "${CONFIG_FILE}" ]]; then
        # shellcheck disable=SC1090
        source "${CONFIG_FILE}"
        printf '%s' "${!name:-${default}}"
    else
        printf '%s' "${default}"
    fi
}

read_github_host() {
    read_config_value GH_PROXY_GITHUB_HOST "${DEFAULT_GITHUB_HOST}"
}

read_docker_host() {
    read_config_value GH_PROXY_DOCKER_HOST "${DEFAULT_DOCKER_HOST}"
}

read_docker_auth_host() {
    read_config_value GH_PROXY_DOCKER_AUTH_HOST "${DEFAULT_DOCKER_AUTH_HOST}"
}

write_config() {
    local listen="$1"
    local github_host="$2"
    local docker_host="$3"
    local docker_auth_host="$4"
    validate_listen "${listen}"
    validate_hostname "${github_host}"
    if [[ -n "${docker_host}" ]]; then
        validate_hostname "${docker_host}"
        validate_hostname "${docker_auth_host}"
    fi
    install -d -m 0755 -o root -g root "${INSTALL_DIR}"
    cat >"${CONFIG_FILE}" <<EOF
# gh-proxy service configuration
GH_PROXY_LISTEN=${listen}
GH_PROXY_GITHUB_HOST=${github_host}
GH_PROXY_DOCKER_HOST=${docker_host}
GH_PROXY_DOCKER_AUTH_HOST=${docker_auth_host}
EOF
    chmod 0644 "${CONFIG_FILE}"
}

latest_version() {
    local latest_url
    latest_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")
    printf '%s' "${latest_url##*/}"
}

install_binary() {
    local version archive base_url tmp_dir
    version=$(latest_version)
    archive="gh-proxy-${version}-${PLATFORM}.tar.gz"
    base_url="https://github.com/${REPO}/releases/download/${version}"
    tmp_dir=$(mktemp -d)

    printf 'Downloading gh-proxy %s for %s...\n' "${version}" "${PLATFORM}"
    curl -fL --retry 3 -o "${tmp_dir}/${archive}" "${base_url}/${archive}"
    tar -xzf "${tmp_dir}/${archive}" -C "${tmp_dir}"

    install -d -m 0755 -o root -g root "${INSTALL_DIR}"
    install -m 0755 -o root -g root "${tmp_dir}/gh-proxy" "${INSTALL_DIR}/gh-proxy.new"
    mv -f "${INSTALL_DIR}/gh-proxy.new" "${INSTALL_DIR}/gh-proxy"
    printf '%s' "${version}" >"${INSTALL_DIR}/VERSION"
    rm -rf "${tmp_dir}"
}

ensure_linux_account() {
    getent group gh-proxy >/dev/null 2>&1 || groupadd --system gh-proxy
    if ! id -u gh-proxy >/dev/null 2>&1; then
        useradd --system --gid gh-proxy --home-dir /nonexistent --shell /usr/sbin/nologin gh-proxy
    fi
}

write_linux_service() {
    ensure_linux_account
    cat >"${LINUX_SERVICE_FILE}" <<'EOF'
[Unit]
Description=gh-proxy Pingora reverse proxy
Documentation=https://github.com/greepar/gh-proxy-rs
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=gh-proxy
Group=gh-proxy
EnvironmentFile=/opt/gh-proxy/gh-proxy.conf
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
    systemctl daemon-reload
    systemctl enable --now gh-proxy.service
    systemctl restart gh-proxy.service
}

write_macos_service() {
    local listen
    listen=$(read_listen)
    cat >"${MACOS_PLIST_FILE}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.greepar.gh-proxy</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/gh-proxy</string>
    <string>--listen</string>
    <string>${listen}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>1048576</integer></dict>
  <key>HardResourceLimits</key><dict><key>NumberOfFiles</key><integer>1048576</integer></dict>
  <key>StandardOutPath</key><string>${MACOS_LOG_FILE}</string>
  <key>StandardErrorPath</key><string>${MACOS_LOG_FILE}</string>
</dict>
</plist>
EOF
    chmod 0644 "${MACOS_PLIST_FILE}"
    plutil -lint "${MACOS_PLIST_FILE}" >/dev/null
    launchctl bootout system "${MACOS_PLIST_FILE}" 2>/dev/null || true
    launchctl bootstrap system "${MACOS_PLIST_FILE}"
    launchctl kickstart -k system/com.greepar.gh-proxy
}

restart_service() {
    if [[ "${OS}" == "linux" ]]; then
        write_linux_service
    else
        write_macos_service
    fi
}

install_proxy() {
    local listen github_host docker_host docker_auth_host
    listen=$(read_input "Listen address and port" "$(read_listen)")
    github_host=$(read_input "GitHub proxy domain" "$(read_github_host)")
    docker_host=$(read_input "Docker Registry proxy domain (leave empty to disable)" "$(read_docker_host)")
    if [[ -n "${docker_host}" ]]; then
        docker_auth_host=$(read_input "Docker auth proxy domain" "$(read_docker_auth_host)")
    else
        docker_auth_host=""
    fi
    validate_listen "${listen}"
    validate_hostname "${github_host}"
    install_binary
    write_config "${listen}" "${github_host}" "${docker_host}" "${docker_auth_host}"
    restart_service
    printf 'Installed gh-proxy on %s.\n' "${listen}"
}

configure_proxy() {
    [[ -x "${INSTALL_DIR}/gh-proxy" ]] || { printf 'gh-proxy is not installed.\n'; return; }
    local listen github_host docker_host docker_auth_host
    listen=$(read_input "Listen address and port" "$(read_listen)")
    github_host=$(read_input "GitHub proxy domain" "$(read_github_host)")
    docker_host=$(read_input "Docker Registry proxy domain (leave empty to disable)" "$(read_docker_host)")
    if [[ -n "${docker_host}" ]]; then
        docker_auth_host=$(read_input "Docker auth proxy domain" "$(read_docker_auth_host)")
    else
        docker_auth_host=""
    fi
    write_config "${listen}" "${github_host}" "${docker_host}" "${docker_auth_host}"
    restart_service
    printf 'Updated configuration and restarted gh-proxy on %s.\n' "${listen}"
}

apply_tuning() {
    if [[ "${OS}" != "linux" ]]; then
        printf 'Persistent Linux TCP tuning is not applicable on macOS.\n'
        return
    fi

    cat >"${LINUX_SYSCTL_FILE}" <<'EOF'
net.core.rmem_max = 33554432
net.core.wmem_max = 33554432
net.ipv4.tcp_rmem = 4096 131072 33554432
net.ipv4.tcp_wmem = 4096 131072 33554432
net.ipv4.tcp_timestamps = 1
net.ipv4.tcp_mtu_probing = 1
EOF
    sysctl -p "${LINUX_SYSCTL_FILE}"
    printf 'Linux TCP throughput tuning applied.\n'
}

show_logs() {
    if [[ "${OS}" == "linux" ]]; then
        journalctl -u gh-proxy.service --no-pager -n 100
    else
        [[ -f "${MACOS_LOG_FILE}" ]] && tail -n 100 "${MACOS_LOG_FILE}" || printf 'No log file yet.\n'
    fi
}

uninstall_proxy() {
    if ! confirm "Remove gh-proxy and its service"; then
        printf 'Cancelled.\n'
        return
    fi

    if [[ "${OS}" == "linux" ]]; then
        systemctl disable --now gh-proxy.service 2>/dev/null || true
        rm -f "${LINUX_SERVICE_FILE}" "${LINUX_SYSCTL_FILE}"
        systemctl daemon-reload
        systemctl reset-failed gh-proxy.service 2>/dev/null || true
    else
        launchctl bootout system "${MACOS_PLIST_FILE}" 2>/dev/null || true
        rm -f "${MACOS_PLIST_FILE}" "${MACOS_LOG_FILE}"
    fi
    rm -rf "${INSTALL_DIR}"
    printf 'gh-proxy removed.\n'
}

menu() {
    local default_option="$1"
    while true; do
        printf '\n%s\n' 'gh-proxy management'
        printf '%s\n' '1. Install or update'
        printf '%s\n' '2. Configure listener'
        printf '%s\n' '3. Apply TCP tuning'
        printf '%s\n' '4. View logs'
        printf '%s\n' '5. Uninstall'
        printf '%s\n' '0. Exit'
        case "$(read_input 'Select an option' "${default_option}")" in
            1) install_proxy ;;
            2) configure_proxy ;;
            3) apply_tuning ;;
            4) show_logs ;;
            5) uninstall_proxy ;;
            0) return ;;
            *) printf 'Invalid option.\n' ;;
        esac
        default_option="0"
    done
}

main() {
    need_root
    detect_platform
    for command in curl tar install; do
        command -v "${command}" >/dev/null || die "missing command: ${command}"
    done
    if [[ "${OS}" == "linux" ]]; then
        command -v systemctl >/dev/null || die "systemd is required on Linux"
    else
        command -v launchctl >/dev/null || die "launchd is required on macOS"
    fi

    if [[ ! -x "${INSTALL_DIR}/gh-proxy" ]]; then
        printf 'gh-proxy is not installed. Select 1 to start first-time setup.\n'
        menu "1"
    else
        menu "0"
    fi
}

main "$@"
