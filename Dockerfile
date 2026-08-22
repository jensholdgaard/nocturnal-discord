# Static musl binary in a distroless image: the artifact is the image, so a
# deploy is an image swap and a boot is process start + replay (no git pull,
# no npm install — the legacy bot's minutes of downtime per crash, gone).
FROM rust:1-bookworm AS build
WORKDIR /src
RUN rustup target add x86_64-unknown-linux-musl \
 && apt-get update && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*
# Dependency layer first: source edits don't re-download the world.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --target x86_64-unknown-linux-musl -p nocturnal \
 && strip target/x86_64-unknown-linux-musl/release/nocturnal

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /src/target/x86_64-unknown-linux-musl/release/nocturnal /usr/local/bin/nocturnal
# The ledger lives on a volume; everything else in the image is read-only.
VOLUME ["/data"]
USER nonroot:nonroot
ENV NOCTURNAL_DATA__DIR=/data
ENTRYPOINT ["/usr/local/bin/nocturnal"]
