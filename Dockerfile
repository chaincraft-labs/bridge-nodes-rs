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

RUN mkdir -p /tmp/bridge-nodes-rs
COPY . /tmp/bridge-nodes-rs

WORKDIR /tmp
# RUN git clone -b feat-identify-node https://github.com/chaincraft-labs/bridge-nodes-rs.git

WORKDIR /tmp/bridge-nodes-rs

RUN cargo build --release

RUN echo "PS1='[\u@\h \$(hostname -I | awk '\''{print \$1}'\'') \W]\$ '" >> /root/.bashrc

# EXPOSE 62649
# EXPOSE 62649/udp

# Start a listener on port 62649
CMD ["nc", "-u", "-l", "-p", "62649"]