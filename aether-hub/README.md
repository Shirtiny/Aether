# aether-hub

`aether-hub` 是 Tunnel Hub 服务，负责在 proxy 与 worker 之间路由帧。

已集成在Docker镜像中, 无需单独部署。

## 部署端指定 Hub 版本并构建

```bash
cd /path/to/Aether
./deploy.sh --hub-tag hub-v0.1.0
```

不指定 `--hub-tag` 时，`./deploy.sh` 会自动解析最新 `hub-v*` release，并在构建 app 镜像时从 GitHub Release 下载对应架构的 Hub 二进制。

## build.sh 模式说明

- 默认是 `binary` 模式（`cross` 构建二进制）。
- `--upload <hub-vX.Y.Z>` 会把构建产物上传到 GitHub Release。
- 加 `--image` 后进入镜像模式（`docker buildx`，可选）。

常用参数：

- `--tag <tag>`: 镜像 tag
- `--image-name <name>`: 镜像名（默认 `ghcr.io/fawney19/aether-hub`）
- `--platforms <list>`: 例如 `linux/amd64,linux/arm64`
- `--push`: 推送镜像
- `--load`: 加载到本地 Docker（单平台）
- `--latest`: 额外打 `latest` tag

## 与部署脚本关系

- `./deploy.sh`: 本地构建部署（会本地构建 app/base，并在构建 app 时从 GitHub Release 下载 Hub，可用 `--hub-tag` 固定版本）。
