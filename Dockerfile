# CoXAgent server image — the hub/control-plane + embedded dashboard.
# Multi-stage: compile the release binary, then a slim runtime.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
# rustls means no OpenSSL. `build-essential` provides a C compiler (gcc/cc):
# the tree-sitter grammars (codegraph, via coxagent-application) compile C at
# build time, and rust:1-slim ships no toolchain to build them with.
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
        build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --bin coxagent

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/coxagent /usr/local/bin/coxagent
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
# Reachable from the host when the port is published.
ENV COXAGENT_HOST=0.0.0.0 COXAGENT_WORKSPACE=/workspace
EXPOSE 4000
VOLUME ["/workspace"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
