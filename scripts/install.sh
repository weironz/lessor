#!/bin/sh
# 安装 latest release 中的 Linux x86_64 CLI；可用 LESSOR_VERSION=0.0.7 锁定版本。
set -eu

repo="${LESSOR_REPOSITORY:-weironz/lessor}"
version="${LESSOR_VERSION:-latest}"
install_dir="${LESSOR_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux) ;;
  *) echo "仅支持 Linux；请在 Windows PowerShell 使用 install.ps1。" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) ;;
  *) echo "目前仅提供 x86_64 Linux CLI。" >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "需要 curl。" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "需要 tar。" >&2; exit 1; }

if [ "$version" = "latest" ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\([^"]*\)".*/\1/p' \
    | head -n 1)"
  [ -n "$version" ] || { echo "无法确定最新发行版本。" >&2; exit 1; }
else
  version="${version#v}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
archive="$tmp_dir/lessor.tar.gz"
url="https://github.com/$repo/releases/download/v$version/lessor-$version-x86_64-linux.tar.gz"

echo "下载 lessor CLI v$version…"
curl -fL --retry 3 --output "$archive" "$url"
tar -xzf "$archive" -C "$tmp_dir" lessord lessor-netcfg lessorctl

mkdir -p "$install_dir"
for binary in lessord lessor-netcfg lessorctl; do
  install -m 755 "$tmp_dir/$binary" "$install_dir/$binary"
done

echo "已安装 lessor CLI v$version 到 $install_dir"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) echo "请将 $install_dir 加入 PATH 后重新打开终端。" ;;
esac
