# forkd + Firecracker + Helikon egress proxy — x86_64 Linux KVM host only.
FROM ubuntu:22.04
ARG FORKD_VERSION=0.5.2
ARG FORKD_SHA256=786371cd10f75f7a24b44a9fae803569872f2cd45b7b2b19ded24a4c2d945102
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl iproute2 iptables busybox-static gettext-base && rm -rf /var/lib/apt/lists/*
# forkd controller (x86_64-linux release tarball) — verify checksum before extracting.
RUN curl -fsSL "https://github.com/deeplethe/forkd/releases/download/v${FORKD_VERSION}/forkd-v${FORKD_VERSION}-x86_64-linux.tar.gz" \
    -o /tmp/forkd.tgz \
    && echo "${FORKD_SHA256}  /tmp/forkd.tgz" | sha256sum -c - \
    && tar -xz -C /usr/local/bin -f /tmp/forkd.tgz \
    && rm /tmp/forkd.tgz
# The egress proxy is built on the host and COPYed in (see docker-compose build args),
# or mount the cargo-built binary at /usr/local/bin/egress-proxy.
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY netns-deny.rules /etc/forkd/netns-deny.rules
COPY netns-deny6.rules /etc/forkd/netns-deny6.rules
RUN chmod +x /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
