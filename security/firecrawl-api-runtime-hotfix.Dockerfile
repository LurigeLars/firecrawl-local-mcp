# syntax=docker/dockerfile:1
FROM node:22.23.3-slim@sha256:c3de60bf2f9dd0ac6370e6117950ff62d6e339527e7472301c9c78a017978392 AS runtime-base

FROM runtime-base AS build-base
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
ENV CI=true
RUN corepack enable && corepack prepare pnpm@11.4.0 --activate

# Build Go shared library
# TEMPORARY LOCAL SECURITY HOTFIX.
# Keep this file byte-aligned with pinned upstream except for this Go build stage.
# Remove once upstream satisfies firecrawl-local-mcp issue #66 promotion gates.
FROM golang:1.25.14@sha256:699337d620559a59b4a2bb298ad59611e535d2ee755a34cf2d2a98f37578dc80 AS go-build
ENV GOTOOLCHAIN=local
WORKDIR /app
COPY sharedLibs/go-html-to-md ./sharedLibs/go-html-to-md

RUN cd sharedLibs/go-html-to-md && \
    go get golang.org/x/net@v0.58.0 && \
    go mod download && \
    go list -m golang.org/x/net | grep -Fx 'golang.org/x/net v0.58.0' && \
    go version | grep -F 'go1.25.14' && \
    go build -o libhtml-to-markdown.so -buildmode=c-shared html-to-markdown.go && \
    go version -m libhtml-to-markdown.so | grep -F 'golang.org/x/net' | grep -F 'v0.58.0'

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
text = text.replace(
    needle,
    'overrides:\n  proxy-addr: "2.0.8"\n  source-map-js: "1.2.2"\n',
    1,
)
path.write_text(text, encoding="utf-8")
PY
RUN --mount=type=cache,id=pnpm,target=/pnpm/store \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/native/target \
    pnpm install --no-frozen-lockfile && \
    grep -q '^  proxy-addr@2\.0\.8:' pnpm-lock.yaml && \
    ! grep -q '^  proxy-addr@2\.0\.7:' pnpm-lock.yaml && \
    grep -q '^  source-map-js@1\.2\.2:' pnpm-lock.yaml && \
    ! grep -q '^  source-map-js@1\.2\.1:' pnpm-lock.yaml

# Build the application from a clean output directory.
RUN rm -rf dist && pnpm run build
ARG GIT_SHA=unknown
RUN test -s dist/src/harness.js && \
    test -s dist/src/controllers/v0/crawl-status.js && \
    printf '%s\n' "$GIT_SHA" > BUILD_SHA

# Remove dev dependencies
RUN pnpm prune --prod --ignore-scripts

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
    if printf '%s\n' "$xnet" | grep -v -F 'v0.58.0'; then \
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
  const safe =
    minor > 26 ||
    (minor === 26 && patch >= 6) ||
    (minor === 25 && patch >= 13);
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
