FROM rust:1.99-slim-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p block-saver -p block-verifier

FROM debian:bookworm-slim
COPY --from=build /app/target/release/block-saver /usr/local/bin/block-saver
COPY --from=build /app/target/release/block-verifier /usr/local/bin/block-verifier
USER nobody
CMD ["block-saver"]
