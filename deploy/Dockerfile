# Build the CoXAgent server binary and run it in a slim image.
# Context is the repo root: `docker build -f deploy/Dockerfile .`
# Use the slim-bookworm variant: the full `rust:1-bookworm` tag can stall during
# Docker Hub metadata fetch on slow networks (seen as `DeadlineExceeded` in the
# deploy gate). The slim image carries the same rustc/cargo toolchain and builds
# this workspace fine — just leaner.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p coxagent-app -p cox-gateway -p cox-runner -p cox-realtime -p cox-knowledge

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gpg \
    && rm -rf /var/lib/apt/lists/*
# GitHub CLI — lets the gateway detect signed-in accounts for the Git settings
# dropdown. Installed in the slim runtime (not the build stage) so it's available
# in all service roles without rebuilding Rust.
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/coxagent /usr/local/bin/coxagent
# Service-role binaries for the split (production) deploy shape.
COPY --from=build /src/target/release/cox-all /usr/local/bin/cox-all
COPY --from=build /src/target/release/cox-gateway /usr/local/bin/cox-gateway
COPY --from=build /src/target/release/cox-runner /usr/local/bin/cox-runner
COPY --from=build /src/target/release/cox-realtime /usr/local/bin/cox-realtime
COPY --from=build /src/target/release/cox-knowledge /usr/local/bin/cox-knowledge
EXPOSE 4000
# Single-project serve; state + managed codebase live on a mounted volume.
ENTRYPOINT ["coxagent"]
CMD ["serve", "--port", "4000", "--state-dir", "/data/state", "--work-dir", "/data/work"]
