# 用交叉编译好的二进制，避免在镜像里重编整个依赖树。
# 先在宿主机执行：
#   cargo zigbuild -p lessord --release --target x86_64-unknown-linux-gnu
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends iproute2 ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY lessord /usr/local/bin/lessord
ENTRYPOINT ["/usr/local/bin/lessord"]
