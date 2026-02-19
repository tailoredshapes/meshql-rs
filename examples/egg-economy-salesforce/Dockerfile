FROM rust:1.83 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p egg-economy-salesforce

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/egg-economy-salesforce /usr/local/bin/
COPY examples/egg-economy-salesforce/config /app/config
CMD ["egg-economy-salesforce"]
