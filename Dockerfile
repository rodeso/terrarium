# syntax=docker/dockerfile:1
# Terrarium's web server and browser assets. Used by compose.yaml; Ollama
# runs outside this image (see README).

# ---- Build ----
FROM rust:1.99-slim-trixie AS build
RUN rustup target add wasm32-unknown-unknown \
 && apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# The prebuilt cargo-leptos from its GitHub release, checked against the
# release's checksum. Compiling it here instead took hours and ran out of
# memory in Docker Desktop's default VM. (0.3.12 has no prebuilt binaries.)
ARG CARGO_LEPTOS_VERSION=0.3.11
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "No prebuilt cargo-leptos for $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && name="cargo-leptos-$arch-unknown-linux-musl" \
 && url="https://github.com/leptos-rs/cargo-leptos/releases/download/v$CARGO_LEPTOS_VERSION/$name.tar.gz" \
 && cd /tmp \
 && curl -fsSLO "$url" \
 && curl -fsSLO "$url.sha256" \
 && sha256sum -c "$name.tar.gz.sha256" \
 && tar xzf "$name.tar.gz" \
 && install "$name/cargo-leptos" /usr/local/cargo/bin/ \
 && rm -rf /tmp/cargo-leptos-*

WORKDIR /app
COPY . .
# The caches keep dependencies and cargo-leptos's tools (wasm-bindgen,
# wasm-opt) between builds; the results are copied out of them.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/root/.cache \
    cargo leptos build --release \
 && mkdir -p /out \
 && cp target/release/terrarium /out/ \
 && cp -r target/site /out/site

# ---- Run ----
FROM debian:trixie-slim
# CA certificates for fetching news feeds over HTTPS.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home terrarium \
 && mkdir /data && chown terrarium /data

WORKDIR /app
COPY --from=build /out/terrarium /app/terrarium
COPY --from=build /out/site /app/site

ENV LEPTOS_ENV=PROD \
    LEPTOS_SITE_ROOT=site \
    LEPTOS_SITE_ADDR=0.0.0.0:8080 \
    TERRARIUM_DB=/data/terrarium.db \
    OLLAMA_URL=http://host.docker.internal:11434

USER terrarium
EXPOSE 8080
VOLUME /data
CMD ["/app/terrarium"]
