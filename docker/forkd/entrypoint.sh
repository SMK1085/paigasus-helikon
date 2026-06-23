#!/usr/bin/env bash
set -euo pipefail
: "${PROXY_PORT:=8443}"
: "${EGRESS_ALLOW:=example.com}"
# PROXY_IP must be an address reachable from inside the child netns — typically the
# Docker bridge gateway (172.17.0.1) or the forkd veth peer, NOT 127.0.0.1 (which
# is the netns's OWN loopback). Must match the PROXY_ADDR the guest image was built with.
PROXY_IP="${PROXY_IP:-172.17.0.1}"
DNS_IP="${DNS_IP:-1.1.1.1}"

# Start the egress proxy.
EGRESS_BIND="0.0.0.0:${PROXY_PORT}" EGRESS_ALLOW="${EGRESS_ALLOW}" \
  /usr/local/bin/egress-proxy &

# forkd doctor: fail fast if KVM/cgroup-v2/Firecracker are missing.
forkd doctor

# Apply Layer-1 rules into each provisioned child netns (forkd's netns-setup runs first).
# NOTE: forkd creates per-child netns dynamically at fork time, not at container startup.
# This startup loop only covers netns that already exist (e.g. pre-provisioned by forkd init).
# Layer-1 default-deny rules MUST be (re)applied to each forkd-created netns at runtime;
# see the runbook for the per-fork netns rule application procedure.
export PROXY_IP PROXY_PORT DNS_IP
_netns_list=$(ip netns list | awk '{print $1}')
if [ -z "$_netns_list" ]; then
  echo "WARN: no child netns present at startup; forkd creates per-fork netns at runtime — Layer-1 default-deny rules MUST be (re)applied per forkd-created netns (see runbook). Startup rule-load covers only pre-existing netns."
else
  for ns in $_netns_list; do
    envsubst < /etc/forkd/netns-deny.rules | ip netns exec "$ns" iptables-restore
    # Assert the default policy is DROP; abort if not.
    ip netns exec "$ns" iptables -S OUTPUT | grep -q -- '-P OUTPUT DROP' \
      || { echo "FATAL: netns $ns OUTPUT policy is not DROP"; exit 1; }
  done
fi

# Start the controller over TLS with bearer auth.
exec forkd-controller \
  --tls-cert /etc/forkd/tls/cert.pem \
  --tls-key  /etc/forkd/tls/key.pem \
  --token-file /etc/forkd/token \
  --per-child-netns
