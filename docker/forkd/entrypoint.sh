#!/usr/bin/env bash
set -euo pipefail
: "${PROXY_PORT:=8443}"
: "${EGRESS_ALLOW:=example.com}"
PROXY_IP="127.0.0.1"
DNS_IP="${DNS_IP:-1.1.1.1}"

# Start the egress proxy.
EGRESS_BIND="0.0.0.0:${PROXY_PORT}" EGRESS_ALLOW="${EGRESS_ALLOW}" \
  /usr/local/bin/egress-proxy &

# forkd doctor: fail fast if KVM/cgroup-v2/Firecracker are missing.
forkd doctor

# Apply Layer-1 rules into each provisioned child netns (forkd's netns-setup runs first).
export PROXY_IP PROXY_PORT DNS_IP
for ns in $(ip netns list | awk '{print $1}'); do
  envsubst < /etc/forkd/netns-deny.rules | ip netns exec "$ns" iptables-restore
  # Assert the default policy is DROP; abort if not.
  ip netns exec "$ns" iptables -S OUTPUT | grep -q -- '-P OUTPUT DROP' \
    || { echo "FATAL: netns $ns OUTPUT policy is not DROP"; exit 1; }
done

# Start the controller over TLS with bearer auth.
exec forkd-controller \
  --tls-cert /etc/forkd/tls/cert.pem \
  --tls-key  /etc/forkd/tls/key.pem \
  --token-file /etc/forkd/token \
  --per-child-netns
