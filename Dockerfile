FROM --platform=$TARGETPLATFORM rust:1.94.0-bookworm AS builder

ARG TARGETARCH

WORKDIR /workspace

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY README.md LICENSE ./

RUN set -eux; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "${arch}" in \
        amd64|x86_64) rust_target="x86_64-unknown-linux-musl" ;; \
        arm64|aarch64) rust_target="aarch64-unknown-linux-musl" ;; \
        *) echo "unsupported TARGETARCH=${arch}" >&2; exit 1 ;; \
    esac; \
    rustup target add "${rust_target}"; \
    cargo build --release --locked --target "${rust_target}"; \
    cp "target/${rust_target}/release/salus" /salus

FROM scratch

COPY --from=builder /salus /bin/salus

USER 65532:65532

ENTRYPOINT ["/bin/salus"]
CMD ["--help"]
