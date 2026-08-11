FROM rust:1.96-trixie AS builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y \
    ca-certificates \
    build-essential \
    libssl-dev \
    pkg-config
RUN cargo build --release --bin friendshipper-server

FROM debian:trixie AS runner
WORKDIR /app
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    pkg-config 
COPY --from=builder /app/target/release/friendshipper-server .
ENTRYPOINT ["./friendshipper-server"]
