# Build the CoXAgent server binary and run it in a slim image.
# Context is the repo root: `docker build -f deploy/Dockerfile .`
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p coxagent-app --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/coxagent /usr/local/bin/coxagent
EXPOSE 4000
# Single-project serve; state + managed codebase live on a mounted volume.
ENTRYPOINT ["coxagent"]
CMD ["serve", "--port", "4000", "--state-dir", "/data/state", "--work-dir", "/data/work"]
