FROM ubuntu:22.04

RUN apt-get update -y && \
    apt-get upgrade -y && \
    apt-get install -y curl git vim build-essential \
    openssh-server nano net-tools netcat && \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && \
    echo "source $HOME/.cargo/env" >> /root/.bashrc && \
    . "$HOME/.cargo/env"

ENV PATH="/root/.cargo/bin:${PATH}"

RUN mkdir -p /usr/src/app/bridge-nodes-rs

COPY . /usr/src/app/bridge-nodes-rs

WORKDIR /usr/src/app/bridge-nodes-rs

RUN cargo build --release

RUN echo "PS1='[\u@\h \$(hostname -I | awk '\''{print \$1}'\'') \W]\$ '" >> /root/.bashrc

# @dev this container will listen to these ports
EXPOSE 62649/udp
EXPOSE 62649

ENV RUST_LOG=info
CMD ["tail", "-f", "/dev/null"]
