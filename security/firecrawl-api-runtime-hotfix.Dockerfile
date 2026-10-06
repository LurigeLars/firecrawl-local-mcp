# syntax=docker/dockerfile:1
FROM node:22-slim@sha256:c3de60bf2f9dd0ac6370e6117950ff62d6e339527e7472301c9c78a017978392 AS base

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
ENV CI=true

RUN corepack enable && corepack prepare pnpm@11.4.0 --activate

# Build Go shared library
# TEMPORARY LOCAL SECURITY HOTFIX.
# Keep this file byte-aligned with pinned upstream except for this Go build stage.
# Remove once upstream satisfies firecrawl-local-mcp issue #66 promotion gates.
FROM golang:1.25.13@sha256:cbff9d1a9041b316010f2da6b701b6c0d597718cb90928c85eb597334a0d23d4 AS go-build
ENV GOTOOLCHAIN=local
WORKDIR /app
COPY sharedLibs/go-html-to-md ./sharedLibs/go-html-to-md

RUN cd sharedLibs/go-html-to-md && \
    go get golang.org/x/net@v0.55.0 && \
    go mod download && \
    go list -m golang.org/x/net | grep -Fx 'golang.org/x/net v0.55.0' && \
    go version | grep -F 'go1.25.13' && \
    go build -o libhtml-to-markdown.so -buildmode=c-shared html-to-markdown.go

FROM base AS build
WORKDIR /app

# Install system dependencies
RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    pkg-config \
    python3 \
    && rm -rf /var/lib/apt/lists/*

# FoundationDB client library (libfdb_c + headers), needed by the
# `foundationdb` npm package at install time on arches without prebuilds
ARG FDB_VERSION=7.3.63
RUN ARCH=$(dpkg --print-architecture) && \
    case "$ARCH" in arm64) FDB_ARCH=aarch64 ;; *) FDB_ARCH=$ARCH ;; esac && \
    curl -fsSL -o /tmp/fdb-clients.deb \
      "https://github.com/apple/foundationdb/releases/download/${FDB_VERSION}/foundationdb-clients_${FDB_VERSION}-1_${FDB_ARCH}.deb" && \
    dpkg -i /tmp/fdb-clients.deb && rm /tmp/fdb-clients.deb

# Install Rust
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path \
    && chmod -R a+w $RUSTUP_HOME $CARGO_HOME

# Copy dependency inputs. The native workspace is included here because its
# install script builds the Rust package; API TypeScript source is copied after
# dependency install so source-only changes do not invalidate this layer.
COPY pnpm-lock.yaml pnpm-workspace.yaml package.json ./
COPY patches ./patches
COPY native ./native

# Install dependencies
RUN --mount=type=cache,id=pnpm,target=/pnpm/store \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/native/target \
    pnpm install --frozen-lockfile

COPY . .

# TEMPORARY LOCAL SECURITY HOTFIX.
# Firecrawl's pinned lock resolves Express -> proxy-addr 2.0.7. Force all
# transitive consumers to patched 2.0.8 inside this image build only.
RUN python3 - <<'PY'
from pathlib import Path

path = Path("pnpm-workspace.yaml")
text = path.read_text(encoding="utf-8")
needle = "overrides:\n"
if needle not in text:
    raise SystemExit("pnpm-workspace.yaml has no overrides block")
text = text.replace(needle, 'overrides:\n  proxy-addr: "2.0.8"\n', 1)
path.write_text(text, encoding="utf-8")
PY
RUN pnpm install --no-frozen-lockfile && \
    grep -q '^  proxy-addr@2\.0\.8:' pnpm-lock.yaml && \
    ! grep -q '^  proxy-addr@2\.0\.7:' pnpm-lock.yaml

# Build the application from a clean output directory.
RUN rm -rf dist && pnpm run build
ARG GIT_SHA=unknown
RUN test -s dist/src/harness.js && \
    test -s dist/src/controllers/v0/crawl-status.js && \
    printf '%s\n' "$GIT_SHA" > BUILD_SHA

# Remove dev dependencies
RUN pnpm prune --prod --ignore-scripts

# Runtime stage
FROM base AS runtime

# Runtime only needs node. Removing npm/corepack/pnpm eliminates their unused
# package trees (including vulnerable node-tar) from the final image.
RUN rm -rf /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/corepack /pnpm && \
    rm -f /usr/local/bin/npm /usr/local/bin/npx /usr/local/bin/corepack \
          /usr/local/bin/pnpm /usr/local/bin/pnpx /usr/local/bin/yarn /usr/local/bin/yarnpkg

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    git \
    procps \
    curl \
    && rm -rf /var/lib/apt/lists/*

# FoundationDB client library (libfdb_c + fdbcli)
ARG FDB_VERSION=7.3.63
RUN ARCH=$(dpkg --print-architecture) && \
    case "$ARCH" in arm64) FDB_ARCH=aarch64 ;; *) FDB_ARCH=$ARCH ;; esac && \
    curl -fsSL -o /tmp/fdb-clients.deb \
      "https://github.com/apple/foundationdb/releases/download/${FDB_VERSION}/foundationdb-clients_${FDB_VERSION}-1_${FDB_ARCH}.deb" && \
    dpkg -i /tmp/fdb-clients.deb && rm /tmp/fdb-clients.deb

EXPOSE 8080
WORKDIR /app
ARG GIT_SHA=unknown
ENV FIRECRAWL_BUILD_SHA=$GIT_SHA

# Copy built application
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/dist ./dist
COPY --from=build /app/native ./native
COPY --from=build /app/BUILD_SHA ./BUILD_SHA

# Copy Go shared library
COPY --from=go-build /app/sharedLibs/go-html-to-md/libhtml-to-markdown.so ./sharedLibs/go-html-to-md/

RUN node -e 'const fs = require("fs"); for (const p of ["dist/src/harness.js", "dist/src/controllers/v0/crawl-status.js", "BUILD_SHA"]) { if (!fs.statSync(p).size) throw new Error(`${p} missing or empty`); }'

CMD ["node", "dist/src/harness.js", "--start-docker"]
