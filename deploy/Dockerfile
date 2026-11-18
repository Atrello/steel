# Build stage
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo build --release -p steeld

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/steeld /usr/local/bin/steeld
ENV STEEL_BIND=0.0.0.0:8080 \
    STEEL_DATA_DIR=/data \
    STEEL_DATABASE_URL=sqlite:////data/steel.db?mode=rwc
VOLUME ["/data"]
EXPOSE 8080
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/steeld"]
