FROM rust:alpine AS builder

ARG TARGETARCH

WORKDIR /app

RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN case "$TARGETARCH" in \
        amd64) RUST_TARGET=x86_64-unknown-linux-musl ;; \
        arm64) RUST_TARGET=aarch64-unknown-linux-musl ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$RUST_TARGET" \
    && cargo build --release --target "$RUST_TARGET" \
    && cp "target/$RUST_TARGET/release/docker-exporter" /docker-exporter

FROM scratch

COPY --from=builder /docker-exporter /docker-exporter

EXPOSE 9417

ENTRYPOINT ["/docker-exporter"]
