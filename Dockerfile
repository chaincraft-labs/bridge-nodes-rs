FROM rust:1.83-slim-bullseye AS builder

WORKDIR /app/bridge-nodes-rs

COPY Cargo.toml Cargo.lock ./

COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update -y && \
    apt-get install -y --no-install-recommends \
    openssh-server \
    net-tools \
    curl \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

WORKDIR /app/bridge-nodes-rs

COPY --from=builder /app/bridge-nodes-rs/target/release/validator_node .

RUN echo "PS1='[\u@\h \$(hostname -I | awk '\''{print \$1}'\'') \W]\$ '" >> /root/.bashrc

EXPOSE 62649/udp
EXPOSE 62649

ENV RUST_LOG=info
CMD ["tail", "-f", "/dev/null"]
