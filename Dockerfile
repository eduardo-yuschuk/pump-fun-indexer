FROM rust:1.99-slim-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p block-saver

FROM debian:bookworm-slim
COPY --from=build /app/target/release/block-saver /usr/local/bin/block-saver
USER nobody
CMD ["block-saver"]
