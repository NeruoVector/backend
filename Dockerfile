# ---- 빌드 스테이지 ---------------------------------------------------------
FROM rust:1-slim-bookworm AS builder

WORKDIR /app

# zstd-sys/lz4-sys/aws-lc-sys 등 C 의존성 빌드에 필요
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config cmake perl \
    && rm -rf /var/lib/apt/lists/*

# 의존성 레이어 캐시: 더미 main.rs 로 먼저 빌드
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# 실제 소스
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

# ---- 런타임 스테이지 -------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/neurovector-backend /usr/local/bin/neurovector-backend

ENV LISTEN_ADDR=0.0.0.0:8081
EXPOSE 8081

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=5 \
    CMD curl -fsS http://localhost:8081/health || exit 1

USER nobody
CMD ["neurovector-backend"]
