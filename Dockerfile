# 构建阶段
FROM rust:1.88-slim AS builder

WORKDIR /app

# 安装构建依赖
RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# 复制依赖缓存文件，利用 Docker 层缓存
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch || true

# 复制源代码
COPY . .

# 构建 release 版本
RUN cargo build --release && \
    strip target/release/llm-wrapper

# 运行阶段
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件
COPY --from=builder /app/target/release/llm-wrapper /app/llm-wrapper

# 创建配置目录
RUN mkdir -p /app/config

# 默认监听地址
ENV BIND_ADDR=0.0.0.0:3000
ENV CONFIG_PATH=/app/config/config.yaml

# 暴露端口
EXPOSE 3000

# 启动命令
CMD ["/app/llm-wrapper"]
