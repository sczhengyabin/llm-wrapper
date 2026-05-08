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

# 先复制依赖描述文件，利用 Docker 层缓存依赖下载和编译
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch || true

# 复制源代码（只有代码变更时才会重编）
COPY src ./src

# 构建 release 版本
RUN cargo build --release && \
    strip target/release/llm-wrapper

# auth2api 构建阶段
FROM node:20-slim AS auth2api-builder
WORKDIR /auth2api
COPY auth2api/package.json auth2api/package-lock.json ./
RUN npm ci --production
COPY auth2api/src ./src
COPY auth2api/tsconfig.json ./
RUN npx tsc

# 运行阶段
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖 (含 Node.js 用于 auth2api)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件、webui 和 auth2api
COPY --from=builder /app/target/release/llm-wrapper /app/llm-wrapper
COPY --from=builder /app/src/webui /app/src/webui
COPY --from=auth2api-builder /auth2api/dist /app/auth2api/dist
COPY --from=auth2api-builder /auth2api/node_modules /app/auth2api/node_modules
COPY --from=auth2api-builder /auth2api/package.json /app/auth2api/

# 创建配置和 token 目录
RUN mkdir -p /app/config /app/.llm-wrapper

# 默认监听地址
ENV BIND_ADDR=0.0.0.0:3000
ENV CONFIG_PATH=/app/config/config.yaml

# 暴露端口
EXPOSE 3000 8317

# 启动命令
CMD ["/app/llm-wrapper"]
