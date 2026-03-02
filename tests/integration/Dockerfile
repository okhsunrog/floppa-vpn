FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
    wireguard-tools \
    wireguard-go \
    iproute2 \
    iputils-ping \
    curl \
    dnsutils \
    ca-certificates \
    procps \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /test
CMD ["sleep", "infinity"]
