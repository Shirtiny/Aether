#!/bin/bash
# aether-hub 构建脚本
#
# 支持两种模式:
# 1) binary 模式（默认）: 构建多架构二进制并可上传 GitHub Release
# 2) image 模式: 构建并推送/加载 Docker 镜像（推荐生产发布用）
#
# 示例:
#   # binary 模式（兼容旧行为）
#   ./build.sh
#   ./build.sh amd64
#   ./build.sh --upload hub-v0.1.0
#
#   # image 模式（多架构推送）
#   ./build.sh --image --tag v0.2.5 --push --latest
#   ./build.sh --image --tag sha-abc123 --image-name ghcr.io/shirtiny/aether-hub --push
#   ./build.sh --image --tag local-test --platforms linux/amd64 --load

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$SCRIPT_DIR/dist"

# -------------------------------
# Defaults
# -------------------------------
MODE="binary"                  # binary | image

# binary mode options
UPLOAD=false
UPLOAD_TAG=""
BINARY_TARGETS=""

# image mode options
IMAGE_NAME="${IMAGE_NAME:-ghcr.io/shirtiny/aether-hub}"
IMAGE_TAG=""
IMAGE_PLATFORMS="linux/amd64,linux/arm64"
IMAGE_PUSH=false
IMAGE_LOAD=false
IMAGE_LATEST=false

usage() {
    cat <<'EOF'
用法:
  ./build.sh [binary-args]
  ./build.sh --image [image-args]

binary 模式（默认）:
  amd64|arm64              仅构建指定架构（可重复）
  --upload <hub-vX.Y.Z>    上传到 GitHub Release（需要 gh CLI）

image 模式:
  --image                  启用镜像模式
  --tag <tag>              镜像 tag（默认自动从 git describe 推导）
  --image-name <name>      镜像名（默认 ghcr.io/shirtiny/aether-hub）
  --platforms <list>       平台列表，逗号分隔（默认 linux/amd64,linux/arm64）
  --push                   推送镜像到仓库
  --load                   加载到本地 Docker（仅单平台）
  --latest                 额外打 latest tag

通用:
  -h, --help               显示帮助
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --image)
            MODE="image"
            shift
            ;;
        --tag)
            IMAGE_TAG="${2:-}"
            shift 2
            ;;
        --image-name)
            IMAGE_NAME="${2:-}"
            shift 2
            ;;
        --platforms)
            IMAGE_PLATFORMS="${2:-}"
            shift 2
            ;;
        --push)
            IMAGE_PUSH=true
            shift
            ;;
        --load)
            IMAGE_LOAD=true
            shift
            ;;
        --latest)
            IMAGE_LATEST=true
            shift
            ;;
        --upload)
            UPLOAD=true
            UPLOAD_TAG="${2:-}"
            shift 2
            ;;
        amd64|arm64)
            BINARY_TARGETS="$BINARY_TARGETS $1"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "❌ 未知参数: $1"
            usage
            exit 1
            ;;
    esac
done

build_binary() {
    if [ -z "$BINARY_TARGETS" ]; then
        BINARY_TARGETS="amd64 arm64"
    fi

    if ! command -v cross >/dev/null 2>&1; then
        echo "❌ 需要安装 cross: cargo install cross --git https://github.com/cross-rs/cross"
        exit 1
    fi

    mkdir -p "$DIST_DIR"

    echo "🔨 开始构建 aether-hub 二进制..."
    echo "   目标平台: $BINARY_TARGETS"
    echo ""

    ARTIFACTS=""
    for arch in $BINARY_TARGETS; do
        case "$arch" in
            amd64) target="x86_64-unknown-linux-gnu" ;;
            arm64) target="aarch64-unknown-linux-gnu" ;;
            *) echo "❌ 未知架构: $arch"; exit 1 ;;
        esac

        echo ">>> 构建 $arch ($target)..."
        cd "$SCRIPT_DIR"
        cross build --release --target "$target" --locked

        BIN="target/$target/release/aether-hub"
        if [ ! -f "$BIN" ]; then
            echo "❌ 未找到二进制文件: $BIN"
            exit 1
        fi

        ARCHIVE="$DIST_DIR/aether-hub-linux-$arch.tar.gz"
        tar czf "$ARCHIVE" -C "target/$target/release" aether-hub
        ARTIFACTS="$ARTIFACTS $ARCHIVE"

        SIZE=$(du -h "$ARCHIVE" | cut -f1)
        echo "✅ $arch 构建完成: $ARCHIVE ($SIZE)"
        echo ""
    done

    cd "$DIST_DIR"
    shasum -a 256 aether-hub-*.tar.gz > SHA256SUMS.txt
    echo "📋 SHA256 校验和:"
    cat SHA256SUMS.txt
    echo ""

    if [ "$UPLOAD" = true ]; then
        if [ -z "$UPLOAD_TAG" ]; then
            echo "❌ --upload 需要指定 tag，例如: ./build.sh --upload hub-v0.1.0"
            exit 1
        fi
        if ! command -v gh >/dev/null 2>&1; then
            echo "❌ 需要安装 GitHub CLI: brew install gh"
            exit 1
        fi

        echo "📦 上传到 GitHub Release: $UPLOAD_TAG"
        cd "$PROJECT_DIR"

        if ! git rev-parse "$UPLOAD_TAG" >/dev/null 2>&1; then
            git tag "$UPLOAD_TAG"
            git push origin "$UPLOAD_TAG"
        fi

        gh release create "$UPLOAD_TAG" \
            --title "aether-hub ${UPLOAD_TAG#hub-}" \
            --generate-notes \
            $ARTIFACTS \
            "$DIST_DIR/SHA256SUMS.txt"

        echo "✅ 上传完成!"
    fi

    echo "🎉 binary 模式完成!"
}

build_image() {
    if ! command -v docker >/dev/null 2>&1; then
        echo "❌ 未找到 docker，请先安装 Docker"
        exit 1
    fi
    if ! docker buildx version >/dev/null 2>&1; then
        echo "❌ 未找到 docker buildx，请先启用 buildx"
        exit 1
    fi

    if [ "$IMAGE_PUSH" = true ] && [ "$IMAGE_LOAD" = true ]; then
        echo "❌ --push 与 --load 不能同时使用"
        exit 1
    fi

    if [ "$IMAGE_PUSH" = false ] && [ "$IMAGE_LOAD" = false ]; then
        # image 模式默认走 push，符合发布场景
        IMAGE_PUSH=true
    fi

    if [ -z "$IMAGE_TAG" ]; then
        IMAGE_TAG=$(git -C "$PROJECT_DIR" describe --tags --always 2>/dev/null | sed 's/^v//')
        if [ -z "$IMAGE_TAG" ]; then
            IMAGE_TAG=$(date +%Y%m%d%H%M%S)
        fi
    fi

    if [ "$IMAGE_LOAD" = true ] && [[ "$IMAGE_PLATFORMS" == *,* ]]; then
        echo "❌ --load 仅支持单平台，请用 --platforms linux/amd64（或 arm64）"
        exit 1
    fi

    local ref="${IMAGE_NAME}:${IMAGE_TAG}"
    local cmd=(docker buildx build
        --platform "$IMAGE_PLATFORMS"
        -f "$SCRIPT_DIR/Dockerfile"
        -t "$ref"
    )

    if [ "$IMAGE_LATEST" = true ]; then
        cmd+=(-t "${IMAGE_NAME}:latest")
    fi

    if [ "$IMAGE_PUSH" = true ]; then
        cmd+=(--push)
    else
        cmd+=(--load)
    fi

    cmd+=("$SCRIPT_DIR")

    echo "🔨 开始构建 aether-hub 镜像..."
    echo "   image: $ref"
    echo "   platforms: $IMAGE_PLATFORMS"
    echo "   mode: $([ "$IMAGE_PUSH" = true ] && echo push || echo load)"
    echo ""

    "${cmd[@]}"

    if [ "$IMAGE_PUSH" = true ]; then
        echo "✅ 镜像已推送: $ref"
        if [ "$IMAGE_LATEST" = true ]; then
            echo "✅ 镜像已推送: ${IMAGE_NAME}:latest"
        fi
    else
        echo "✅ 镜像已加载到本地: $ref"
    fi

    echo "🎉 image 模式完成!"
}

if [ "$MODE" = "image" ]; then
    build_image
else
    build_binary
fi
