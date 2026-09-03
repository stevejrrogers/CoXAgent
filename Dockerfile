# CoXAgent server image — the hub/control-plane + embedded dashboard.
# Multi-stage: compile the release binary, then a slim runtime.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
# rustls means no OpenSSL; git is handy for brownfield onboarding at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --bin coxagent

FROM debian:bookworm-slim
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
COPY --from=docker:cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker:cli /usr/local/libexec/docker/cli-plugins/ /usr/local/libexec/docker/cli-plugins/
# Reachable from the host when the port is published.
ENV COXAGENT_HOST=0.0.0.0 COXAGENT_WORKSPACE=/workspace
EXPOSE 4000
VOLUME ["/workspace"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
