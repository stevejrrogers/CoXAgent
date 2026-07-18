# Build the CoXAgent server binary and run it in a slim image.
# Context is the repo root: `docker build -f deploy/Dockerfile .`
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p coxagent-app -p cox-gateway -p cox-runner -p cox-realtime -p cox-knowledge

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
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
