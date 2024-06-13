FROM rust:latest as base
WORKDIR /usr/src/myapp
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch
COPY src ./src
RUN cargo install --path . --bin mangorpc-latency-tester
FROM debian:bookworm-slim as run
RUN apt-get update && apt-get -y install ca-certificates libc6 libssl3 libssl-dev openssl
COPY --from=0 /usr/local/cargo/bin/mangorpc-latency-tester /usr/local/bin/mangorpc-latency-tester
CMD ["mangorpc-latency-tester", "watch-measure-send-transaction", "--watch-interval-seconds", "21600"]
