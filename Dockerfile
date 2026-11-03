# Tilly's Telegram bot. Build: docker build -t tilly-bot .
# Secrets come from the environment at run time; none are baked into the image.

FROM rust:1-slim-bookworm AS build
WORKDIR /src
# solana-rpc-client and teloxide use reqwest's default TLS, which is OpenSSL on Linux.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
# rust-toolchain.toml pins the compiler; rustup installs it on first use.
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain || rustup toolchain install
COPY . .
# Checked queries come from .sqlx/, so the build needs no database.
ENV SQLX_OFFLINE=true
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p warden-telegram-bot \
    && cp target/release/warden-telegram-bot /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/warden-telegram-bot /usr/local/bin/
WORKDIR /app
# Non-secret config; WARDEN_* env vars override it.
COPY warden.toml ./
USER 10001
CMD ["warden-telegram-bot"]
