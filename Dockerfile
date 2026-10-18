# CoXAgent server image — the hub/control-plane + embedded dashboard.
# Multi-stage: compile the release binary, then a slim runtime.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
# git + ssh: harxes-core is a PRIVATE ssh git dependency — cargo must fetch it
# through the caller's forwarded ssh agent (BuildKit `--ssh default`). Without
# this the Linux gate failed EVERY diff since the harxes flip (velocity 0).
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config git openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p -m 0700 /root/.ssh \
    && ssh-keyscan github.com >> /root/.ssh/known_hosts
COPY . .
RUN --mount=type=ssh CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --release --bin coxagent

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
