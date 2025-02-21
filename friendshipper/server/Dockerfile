FROM rust:1.84-bookworm AS builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    pkg-config \
    libsoup2.4-dev \
    libsoup3-dev \
    javascriptcoregtk-4.0 \
    libgtk-3-dev \
    webkit2gtk-4.0
RUN cargo build --release --bin friendshipper-server

FROM debian:bookworm AS runner
WORKDIR /app
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    pkg-config 
COPY --from=builder /app/target/release/friendshipper-server .
ENTRYPOINT ["./friendshipper-server"]
