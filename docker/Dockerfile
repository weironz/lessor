# 用交叉编译好的二进制，避免在镜像里重编整个依赖树。
# 先在宿主机执行：
#   cargo zigbuild -p lessord --release --target x86_64-unknown-linux-gnu
FROM debian:trixie-slim
# libcap2-bin 是给 setcaptest.sh 用的（setcap/getcap）——
# 发布镜像 Dockerfile.dist 里没有，那边不需要
RUN apt-get update \
 && apt-get install -y --no-install-recommends iproute2 ca-certificates libcap2-bin \
 && rm -rf /var/lib/apt/lists/*
COPY lessord /usr/local/bin/lessord
ENTRYPOINT ["/usr/local/bin/lessord"]
