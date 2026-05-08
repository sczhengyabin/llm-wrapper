# Rust 构建阶段
FROM rust:1.88-slim AS rust-builder

WORKDIR /app

# 安装构建依赖
RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# 先复制依赖描述文件，利用 Docker 层缓存依赖下载和编译
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch || true

# 复制源代码（只有代码变更时才会重编）
COPY src ./src

# 构建 release 版本
RUN cargo build --release && \
    strip target/release/llm-wrapper

# Go 构建阶段（CLIProxyAPI）
FROM golang:1.24-alpine AS go-builder
WORKDIR /CLIProxyAPI
COPY CLIProxyAPI/go.mod CLIProxyAPI/go.sum ./
RUN go mod download
COPY CLIProxyAPI/ ./
RUN CGO_ENABLED=0 go build -ldflags="-s -w" -o CLIProxyAPI ./cmd/server/

# 运行阶段
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件和 webui
COPY --from=rust-builder /app/target/release/llm-wrapper /app/llm-wrapper
COPY --from=rust-builder /app/src/webui /app/src/webui
COPY --from=go-builder /CLIProxyAPI/CLIProxyAPI /app/cli-proxy-api/CLIProxyAPI

# 创建配置和 token 目录
RUN mkdir -p /app/config /app/.llm-wrapper

# 默认监听地址
ENV BIND_ADDR=0.0.0.0:3000
ENV CONFIG_PATH=/app/config/config.yaml

# 暴露端口
EXPOSE 3000 8317

# 启动命令
CMD ["/app/llm-wrapper"]
