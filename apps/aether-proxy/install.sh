#!/bin/sh
set -eu

REPO="${AETHER_PROXY_RELEASE_REPO:-fawney19/Aether}"
TAG="${AETHER_PROXY_RELEASE_TAG:-}"
INSTALL_DIR="${AETHER_PROXY_INSTALL_DIR:-}"
CONFIG_PATH="${AETHER_PROXY_CONFIG:-}"
TMP_DIR=""

say() { printf '%s\n' "[Aether Proxy] $1"; }
fail() { printf '%s\n' "[Aether Proxy] $1" >&2; exit 1; }

cleanup() {
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT INT TERM

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "缺少命令：$1"
}

download() {
  url="$1"
  out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --connect-timeout 10 -o "$out" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "$out" "$url"
  else
    fail "需要 curl 或 wget 下载 release 制品"
  fi
}

prompt_if_empty() {
  name="$1"
  value="$2"
  prompt="$3"
  if [ -n "$value" ]; then
    printf '%s' "$value"
    return
  fi
  printf '%s' "$prompt" >&2
  if [ -r /dev/tty ]; then
    IFS= read -r value < /dev/tty
  else
    fail "$name 未通过环境变量提供，且当前环境无法交互输入"
  fi
  [ -n "$value" ] || fail "$name 不能为空"
  printf '%s' "$value"
}

toml_quote() {
  value="$1"
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys; print(json.dumps(sys.argv[1], ensure_ascii=False))' "$value"
  else
    escaped=$(printf '%s' "$value" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '"%s"\n' "$escaped"
  fi
}

resolve_latest_proxy_tag() {
  [ -n "$TAG" ] && { printf '%s\n' "$TAG"; return; }
  api_url="https://api.github.com/repos/${REPO}/releases?per_page=100"
  releases="$TMP_DIR/releases.json"
  download "$api_url" "$releases" >/dev/null 2>&1 || fail "无法读取 GitHub Releases：$api_url"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$releases" <<'PY'
import json, sys
releases = json.load(open(sys.argv[1], encoding='utf-8'))
proxy = [r for r in releases if not r.get('draft') and str(r.get('tag_name', '')).startswith('proxy-v')]
proxy.sort(key=lambda r: r.get('published_at') or r.get('created_at') or '', reverse=True)
if proxy:
    print(proxy[0]['tag_name'])
PY
  else
    grep -o '"tag_name"[[:space:]]*:[[:space:]]*"proxy-v[^"]*"' "$releases" | head -n 1 | sed 's/.*"\(proxy-v[^"]*\)".*/\1/'
  fi
}

detect_asset() {
  os=$(uname -s 2>/dev/null || printf unknown)
  arch=$(uname -m 2>/dev/null || printf unknown)

  case "$os" in
    Linux) platform=linux ;;
    Darwin) platform=macos ;;
    MINGW*|MSYS*|CYGWIN*) fail "检测到 Windows shell，请使用 PowerShell：irm <install.ps1-url> | iex" ;;
    *) fail "不支持的系统：$os" ;;
  esac

  case "$arch" in
    x86_64|amd64) cpu=amd64 ;;
    aarch64|arm64) cpu=arm64 ;;
    *) fail "不支持的 CPU 架构：$arch" ;;
  esac

  if [ "$platform" = "linux" ] && command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
    printf 'aether-proxy-linux-musl-%s.tar.gz\n' "$cpu"
  else
    printf 'aether-proxy-%s-%s.tar.gz\n' "$platform" "$cpu"
  fi
}

choose_paths() {
  if [ -z "$INSTALL_DIR" ]; then
    if [ "$(id -u 2>/dev/null || printf 1)" = "0" ]; then
      INSTALL_DIR="/usr/local/bin"
    else
      INSTALL_DIR="$HOME/.local/bin"
    fi
  fi
  if [ -z "$CONFIG_PATH" ]; then
    if [ "$(id -u 2>/dev/null || printf 1)" = "0" ]; then
      CONFIG_PATH="/etc/aether-proxy/aether-proxy.toml"
    else
      CONFIG_PATH="$HOME/.aether-proxy/aether-proxy.toml"
    fi
  fi
}

verify_checksum() {
  archive="$1"
  sums="$2"
  asset="$3"
  [ -f "$sums" ] || return 0
  expected=$(awk -v asset="$asset" '$2 == asset { print $1 }' "$sums" | head -n 1)
  [ -n "$expected" ] || return 0
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | awk '{print $1}')
  else
    say "未找到 sha256sum/shasum，跳过校验"
    return 0
  fi
  [ "$actual" = "$expected" ] || fail "SHA256 校验失败：$asset"
}

install_binary() {
  tag="$1"
  asset="$2"
  base="https://github.com/${REPO}/releases/download/${tag}"
  archive="$TMP_DIR/$asset"
  say "下载 $tag / $asset"
  download "$base/$asset" "$archive"
  download "$base/SHA256SUMS.txt" "$TMP_DIR/SHA256SUMS.txt" >/dev/null 2>&1 || true
  verify_checksum "$archive" "$TMP_DIR/SHA256SUMS.txt" "$asset"

  tar -xzf "$archive" -C "$TMP_DIR"
  [ -f "$TMP_DIR/aether-proxy" ] || fail "制品中未找到 aether-proxy"
  mkdir -p "$INSTALL_DIR"
  cp "$TMP_DIR/aether-proxy" "$INSTALL_DIR/aether-proxy"
  chmod +x "$INSTALL_DIR/aether-proxy"
  say "已安装二进制：$INSTALL_DIR/aether-proxy"
}

has_legacy_single_server_keys() {
  [ -f "$CONFIG_PATH" ] || return 1
  awk '
    /^[[:space:]]*\[/ { exit }
    /^[[:space:]]*(aether_url|management_token)[[:space:]]*=/ { found=1; exit }
    END { exit found ? 0 : 1 }
  ' "$CONFIG_PATH"
}

server_exists() {
  [ -f "$CONFIG_PATH" ] || return 1
  quoted_url="$1"
  quoted_name="$2"
  awk -v url="aether_url = $quoted_url" -v name="node_name = $quoted_name" '
    BEGIN { found_url=0; found_name=0 }
    /^\[\[servers\]\]/ {
      if (found_url && found_name) { found=1 }
      found_url=0; found_name=0
    }
    $0 == url { found_url=1 }
    $0 == name { found_name=1 }
    END { if (found_url && found_name) { found=1 }; exit found ? 0 : 1 }
  ' "$CONFIG_PATH"
}

append_server_config() {
  aether_url="$1"
  management_token="$2"
  node_name="$3"

  mkdir -p "$(dirname "$CONFIG_PATH")"
  quoted_url=$(toml_quote "$aether_url")
  quoted_token=$(toml_quote "$management_token")
  quoted_name=$(toml_quote "$node_name")

  if has_legacy_single_server_keys; then
    fail "现有配置仍使用旧的顶层 aether_url/management_token，请先运行 aether-proxy setup 迁移为 [[servers]] 后重试：$CONFIG_PATH"
  fi

  if server_exists "$quoted_url" "$quoted_name"; then
    say "配置中已存在相同 aether_url + node_name，跳过追加：$CONFIG_PATH"
    return
  fi

  if [ -f "$CONFIG_PATH" ]; then
    cp "$CONFIG_PATH" "$CONFIG_PATH.bak.$(date +%Y%m%d%H%M%S)"
  fi

  {
    if [ -f "$CONFIG_PATH" ] && [ -s "$CONFIG_PATH" ]; then
      printf '\n'
    fi
    printf '# Added by Aether Proxy one-click installer. Existing config is preserved.\n'
    printf '[[servers]]\n'
    printf 'aether_url = %s\n' "$quoted_url"
    printf 'management_token = %s\n' "$quoted_token"
    printf 'node_name = %s\n' "$quoted_name"
  } >> "$CONFIG_PATH"
  chmod 600 "$CONFIG_PATH" 2>/dev/null || true
  say "已追加 [[servers]] 到：$CONFIG_PATH"
}

main() {
  TMP_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t aether-proxy)
  need_cmd tar
  choose_paths

  aether_url=$(prompt_if_empty AETHER_PROXY_AETHER_URL "${AETHER_PROXY_AETHER_URL:-}" "Aether URL: ")
  management_token=$(prompt_if_empty AETHER_PROXY_MANAGEMENT_TOKEN "${AETHER_PROXY_MANAGEMENT_TOKEN:-}" "Management token (ae_xxx): ")
  node_name=$(prompt_if_empty AETHER_PROXY_NODE_NAME "${AETHER_PROXY_NODE_NAME:-}" "Node name: ")

  tag=$(resolve_latest_proxy_tag)
  [ -n "$tag" ] || fail "没有找到可用的 proxy-v* release"
  asset=$(detect_asset)
  install_binary "$tag" "$asset"
  append_server_config "$aether_url" "$management_token" "$node_name"

  say "完成。运行以下命令启动/配置服务："
  say "  $INSTALL_DIR/aether-proxy setup $CONFIG_PATH"
}

main "$@"
