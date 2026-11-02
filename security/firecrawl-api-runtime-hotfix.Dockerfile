# syntax=docker/dockerfile:1
FROM node:22.23.4-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da AS runtime-base

FROM runtime-base AS build-base
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
ENV CI=true
RUN corepack enable && corepack prepare pnpm@11.4.0 --activate

# Build Go shared library
# TEMPORARY LOCAL SECURITY HOTFIX.
# Based on reviewed upstream API v2.11.642, preserving current fork's pinned runtime base.
# Remove once upstream satisfies firecrawl-local-mcp issue #66 promotion gates.
FROM golang:1.25.14@sha256:699337d620559a59b4a2bb298ad59611e535d2ee755a34cf2d2a98f37578dc80 AS go-build
# The pinned builder is build-time only; install the patched Go compiler from
# the official signed-distribution SHA256 values before compiling the shared library.
# This avoids leaving an unverified/floating builder digest in the release path.
RUN set -eu; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) suffix=amd64; digest=ccfe7ae822d2c2f41f4fe7c956199967a4dac22d0ee0d2b2bfdb440aab3b20c5 ;; \
      arm64) suffix=arm64; digest=3f4227ce7b206a513e794f1393adb473a07b46e94d18bd15d1c684bbb7319797 ;; \
      *) echo "Unsupported Go builder architecture: $arch" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://dl.google.com/go/go1.26.10.linux-${suffix}.tar.gz" -o /tmp/go12610.tar.gz; \
    echo "$digest  /tmp/go12610.tar.gz" | sha256sum -c -; \
    rm -rf /usr/local/go; \
    tar -C /usr/local -xzf /tmp/go12610.tar.gz; \
    rm -f /tmp/go12610.tar.gz
ENV GOTOOLCHAIN=local
RUN go version | grep -F 'go1.26.10'
WORKDIR /app
COPY sharedLibs/go-html-to-md ./sharedLibs/go-html-to-md

RUN cd sharedLibs/go-html-to-md && \
    go get golang.org/x/net@v0.60.0 && \
    go mod download && \
    go list -m golang.org/x/net | grep -Fx 'golang.org/x/net v0.60.0' && \
    go version | grep -F 'go1.26.10' && \
    go build -o libhtml-to-markdown.so -buildmode=c-shared html-to-markdown.go && \
    go version -m libhtml-to-markdown.so | grep -F 'golang.org/x/net' | grep -F 'v0.60.0'

FROM build-base AS build
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

# Pin enforcement: upstream v2.11.642 already carries the patched Node
# overrides. Do not re-insert obsolete dependency overrides or downgrade undici.
# The frozen lock install above must resolve exactly the reviewed upstream set.
RUN set -eu; \
    grep -F '"proxy-addr@>=2.0.0 <2.0.8": "2.0.8"' pnpm-workspace.yaml; \
    grep -F '"source-map-js@>=1.0.0 <1.2.2": "1.2.2"' pnpm-workspace.yaml; \
    grep -F '"smol-toml@>=1.0.0 <1.9.0": "1.9.0"' pnpm-workspace.yaml; \
    grep -F '"shell-quote@<1.11.0": "1.11.0"' pnpm-workspace.yaml; \
    grep -F 'undici: "7.30.1"' pnpm-workspace.yaml; \
    grep -F '"undici": "7.30.1"' package.json

# Build the application from a clean output directory.
RUN rm -rf dist && pnpm run build
ARG GIT_SHA=unknown
RUN test -s dist/src/harness.js && \
    test -s dist/src/controllers/v0/crawl-status.js && \
    printf '%s\n' "$GIT_SHA" > BUILD_SHA

# Remove dev dependencies
RUN pnpm prune --prod --ignore-scripts

# TypeScript 7 is only used by development/watch commands. The Docker runtime
# executes prebuilt dist/*.js via --start-docker, so keep its native Go-built
# compiler out of the production image.
RUN rm -rf \
      node_modules/typescript-7 \
      node_modules/.pnpm/typescript@7.0.2 \
      node_modules/.pnpm/@typescript+typescript-linux-x64@7.0.2 && \
    test ! -e node_modules/typescript-7 && \
    test ! -e node_modules/.pnpm/typescript@7.0.2 && \
    test ! -e node_modules/.pnpm/@typescript+typescript-linux-x64@7.0.2

# Fail the build if the production app tree contains a vulnerable node-tar.
# CVE-2026-59873 is fixed in tar >= 7.5.19.
RUN find node_modules -type f -path '*/tar/package.json' -print > /tmp/tar-package-jsons && \
    node - <<'NODE'
const fs = require("fs");
const files = fs.readFileSync("/tmp/tar-package-jsons", "utf8").trim().split(/\r?\n/).filter(Boolean);
for (const file of files) {
  const { version } = JSON.parse(fs.readFileSync(file, "utf8"));
  const parts = String(version).split(".").map(Number);
  const ok =
    parts[0] > 7 ||
    (parts[0] === 7 && (parts[1] > 5 || (parts[1] === 5 && parts[2] >= 19)));
  if (!ok) throw new Error(`vulnerable node-tar ${version} at ${file}`);
}
NODE

# Runtime stage
FROM runtime-base AS runtime

# Install security updates for Debian packages inherited from the pinned Node runtime,
# not just dependencies touched by apt-get install. Keep Node 22 and the pinned base digest.
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && apt-get install -y --no-install-recommends \
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

# Verify every Go binary/module visible in the assembled runtime filesystem.
# This catches hidden/prebuilt dependencies that might embed an older x/net.
FROM runtime AS verify-go-runtime
COPY --from=go-build /usr/local/go /usr/local/go
RUN set -eu; \
    find /app /usr/bin /usr/local/bin -xdev -type f -size +64k \
      -exec sh -c 'for f do /usr/local/go/bin/go version -m "$f" 2>/dev/null || true; done' sh {} + \
      > /tmp/go-runtime-modules.txt; \
    xnet="$(grep -F 'golang.org/x/net' /tmp/go-runtime-modules.txt || true)"; \
    test -n "$xnet"; \
    printf '%s\n' "$xnet"; \
    if printf '%s\n' "$xnet" | grep -v -F 'v0.60.0'; then \
      echo "unexpected golang.org/x/net version in runtime" >&2; \
      exit 1; \
    fi; \
    node - <<'NODE'
const fs = require("fs");
const text = fs.readFileSync("/tmp/go-runtime-modules.txt", "utf8");
const lines = text.split(/\r?\n/);
const vulnerable = [];
for (const line of lines) {
  const match = line.match(/^(.*): go1\.(\d+)\.(\d+)(?:\D|$)/);
  if (!match) continue;
  const [, file, minorText, patchText] = match;
  const minor = Number(minorText);
  const patch = Number(patchText);
  // Fail for older binaries even if they passed last month's Go advisory:
  // new Go standard library issues require at least go1.26.10.
  const safe = minor > 26 || (minor === 26 && patch >= 10);
  if (!safe) vulnerable.push({ file, version: `go1.${minor}.${patch}` });
}
if (vulnerable.length) {
  throw new Error(
    "runtime contains Go binaries built with toolchains vulnerable to CVE-2026-39821: " +
    JSON.stringify(vulnerable)
  );
}
NODE

FROM runtime AS final
COPY --from=verify-go-runtime /tmp/go-runtime-modules.txt ./GO_RUNTIME_MODULES.txt

CMD ["node", "dist/src/harness.js", "--start-docker"]
