FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libwebkit2gtk-4.1-dev \
    build-essential \
    curl \
    wget \
    file \
    libssl-dev \
    libxdo-dev \
    libgtk-3-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev
RUN cargo build --release --bin friendshipper-server

FROM debian:bookworm AS runner
WORKDIR /app
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    pkg-config 
COPY --from=builder /app/target/release/friendshipper-server .
ENTRYPOINT ["./friendshipper-server"]
