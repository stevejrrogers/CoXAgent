# CoXAgent server image — the hub/control-plane + embedded dashboard.
# Multi-stage: compile the release binary, then a slim runtime.
#
# CXA-B313: base images are pinned per-architecture by digest (the tag alone
# floats; these digests are immutable). To re-pin after an upgrade run:
#   docker buildx imagetools inspect rust:1-slim-bookworm | grep -A12 'linux/arm64/v8'
FROM rust:1-slim-bookworm@sha256:86234e857a413004348b471d11a4ebe1aa92d2eae2b6b143cac68fd1ac3c48be AS builder
WORKDIR /build
# git + ssh: harxes-core is a PRIVATE ssh git dependency — cargo must fetch it
# through the caller's forwarded ssh agent (BuildKit `--ssh default`). Without
# this the Linux gate failed EVERY diff since the harxes flip (velocity 0).
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config git openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p -m 0700 /root/.ssh \
    && ssh-keyscan github.com >> /root/.ssh/known_hosts

# CXA-B313: dependency layer is separated from the source-copy layer. Copying
# only manifests + lockfiles first means an unrelated source edit reuses the
# cached registry index instead of re-fetching every crate — a `cargo build`
# re-run then compiles just the changed crates. The BuildKit cache mount keeps
# ~/.cargo/registry across runs so even `docker compose build --no-cache` skips
# the network for already-downloaded dependencies.
COPY Cargo.toml Cargo.lock ./
COPY crates/domain/Cargo.toml crates/domain/Cargo.toml
COPY crates/application/Cargo.toml crates/application/Cargo.toml
COPY crates/contracts/Cargo.toml crates/contracts/Cargo.toml
COPY crates/infrastructure/Cargo.toml crates/infrastructure/Cargo.toml
COPY crates/presentation/Cargo.toml crates/presentation/Cargo.toml
COPY crates/services/gateway/Cargo.toml crates/services/gateway/Cargo.toml
COPY crates/services/runner/Cargo.toml crates/services/runner/Cargo.toml
COPY crates/services/realtime/Cargo.toml crates/services/realtime/Cargo.toml
COPY crates/services/knowledge/Cargo.toml crates/services/knowledge/Cargo.toml
COPY crates/app/Cargo.toml crates/app/Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    mkdir -p crates/app/src crates/domain/src crates/application/src \
             crates/contracts/src crates/infrastructure/src \
             crates/presentation/src \
    && echo "fn main() {}" > crates/app/src/main.rs \
    && for c in domain application contracts infrastructure presentation; do \
         echo "" > crates/$c/src/lib.rs; done \
    && for s in gateway runner realtime knowledge; do mkdir -p crates/services/$s/src && echo "" > crates/services/$s/src/lib.rs; done \
    && cargo build --release --bin coxagent || true

COPY . .
RUN --mount=type=ssh \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --release --bin coxagent

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/coxagent /usr/local/bin/coxagent
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
# The hub runs `docker` (and `docker compose`) against the HOST daemon through
# the socket the compose file mounts — the docker janitor's sweeps (CXA-B143)
# and the deploy adapter are fail-closed on every probe, so a hub image without
# the CLI silently no-ops them all (CXA-B153). The docker:cli image ships the
# CLI plus the compose/buildx plugins in the path the CLI scans by default.
# Pinned by digest like every other base image (CXA-B313).
COPY --from=docker:cli@sha256:5e6baf67325a79f64ed3bed6863954d5e05d0f3d4e57cbdb14809346814bbfdb \
     /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker:cli@sha256:5e6baf67325a79f64ed3bed6863954d5e05d0f3d4e57cbdb14809346814bbfdb \
     /usr/local/libexec/docker/cli-plugins/ /usr/local/libexec/docker/cli-plugins/
# Reachable from the host when the port is published.
ENV COXAGENT_HOST=0.0.0.0 COXAGENT_WORKSPACE=/workspace
EXPOSE 4000
VOLUME ["/workspace"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
