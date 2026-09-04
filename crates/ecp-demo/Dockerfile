# syntax=docker/dockerfile:1.7
# Public demo image: `ecp` + `ecp-demo` built from this commit. Repositories
# are cloned and indexed at runtime, on request, under /data (ephemeral: a
# restart starts empty, which is fine for a demo).
#
# Build from the repository root:
#   docker build -f crates/ecp-demo/Dockerfile -t ecp-demo .

FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential git pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# Plain `release`, not `release-dist`: LTO triples the build for a binary
# whose CPU share on the free tier is the bottleneck anyway.
RUN cargo build --release -p egent-code-plexus --bin ecp -p ecp-demo --bin ecp-demo \
 && install -D target/release/ecp /out/ecp \
 && install -D target/release/ecp-demo /out/ecp-demo

FROM debian:bookworm-slim AS runtime
# git clones the repositories and ecp reads HEAD; curl asks the GitHub API
# for a repository's size before the clone.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 1000 --create-home demo \
 && mkdir -p /data/repos /data/ecp \
 && chown -R demo:demo /data
COPY --from=build /out/ecp /out/ecp-demo /usr/local/bin/

ENV ECP_HOME=/data/ecp \
    ECP_DEMO_REPOS=/data/repos \
    ECP_NO_TELEMETRY=1 \
    ECP_SKIP_BG_REBUILD=1 \
    PORT=8080

USER demo
WORKDIR /data
EXPOSE 8080
# Liveness is the host's job (`healthCheckPath: /healthz` in render.yaml).
CMD ["ecp-demo"]
