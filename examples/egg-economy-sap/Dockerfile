FROM rust:1.83 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p egg-economy-sap

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/egg-economy-sap /usr/local/bin/
COPY examples/egg-economy-sap/config /app/config
CMD ["egg-economy-sap"]
