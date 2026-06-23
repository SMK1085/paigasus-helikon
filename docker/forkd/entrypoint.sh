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
# NOTE: forkd creates per-child netns dynamically at fork time, NOT at container startup.
# This startup loop covers ONLY netns that already exist (e.g. pre-provisioned by forkd init).
# IMPORTANT: per-fork netns created AFTER this controller starts are NOT covered by this loop.
# Each netns forkd creates at runtime MUST have the Layer-1 default-deny rules applied via
# forkd's per-netns hook before the child process runs. This is a known live-validation
# limitation — see the runbook (docs/runbooks/forkd-live-validation.md) for the
# per-fork netns rule application procedure. Failure to enforce this leaves the per-fork
# netns WITHOUT Layer-1 default-deny.
export PROXY_IP PROXY_PORT DNS_IP
_netns_list=$(ip netns list | awk '{print $1}')
if [ -z "$_netns_list" ]; then
  echo "WARN: no child netns present at startup."
  echo "WARN: forkd creates per-fork netns at runtime — these are NOT covered by this startup loop."
  echo "WARN: Layer-1 default-deny rules MUST be applied to each forkd-created netns via the"
  echo "WARN: per-netns hook BEFORE the child process runs. See the runbook for the required"
  echo "WARN: procedure. Omitting this leaves per-fork netns without egress enforcement."
else
  for ns in $_netns_list; do
    envsubst < /etc/forkd/netns-deny.rules | ip netns exec "$ns" iptables-restore
    # Assert the IPv4 default policy is DROP; abort if not.
    ip netns exec "$ns" iptables -S OUTPUT | grep -q -- '-P OUTPUT DROP' \
      || { echo "FATAL: netns $ns IPv4 OUTPUT policy is not DROP"; exit 1; }
    # Apply IPv6 default-deny (companion to the IPv4 ruleset).
    ip netns exec "$ns" ip6tables-restore < <(envsubst < /etc/forkd/netns-deny6.rules)
    # Assert the IPv6 default policy is DROP; abort if not.
    ip netns exec "$ns" ip6tables -S OUTPUT | grep -q -- '-P OUTPUT DROP' \
      || { echo "FATAL: netns $ns IPv6 OUTPUT policy is not DROP"; exit 1; }
  done
fi

# Start the controller over TLS with bearer auth.
exec forkd-controller \
  --tls-cert /etc/forkd/tls/cert.pem \
  --tls-key  /etc/forkd/tls/key.pem \
  --token-file /etc/forkd/token \
  --per-child-netns
