FROM rust:1-slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/svc-gitconfig-proxy /usr/local/bin/
EXPOSE 8470
# NOTE: Set SVC_ALLOWED_IPS to your DayZ server's Docker gateway IP (e.g. 172.17.0.1)
# The default 127.0.0.1 will block all connections in Docker bridge mode.
ENTRYPOINT ["svc-gitconfig-proxy"]
CMD ["--bind", "0.0.0.0"]
