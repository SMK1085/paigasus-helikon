#!/usr/bin/env bash
set -euo pipefail
: "${PROXY_PORT:=8443}"
: "${EGRESS_ALLOW:=example.com}"
# Interface names used in forkd's per-child netns topology:
#   GUEST_IF  — the tap interface the guest VM is attached to (host side of the tap pair)
#   UPLINK_IF — the veth interface that routes the netns's egress to the root ns (SNAT'd)
# These match forkd's default netns topology. Override via env if your forkd build differs.
GUEST_IF="${GUEST_IF:-forkd-tap0}"
UPLINK_IF="${UPLINK_IF:-veth0}"
# DNS resolver used by the in-netns egress proxy to resolve CONNECT targets.
# Written into /etc/netns/<ns>/resolv.conf so the proxy inside the netns can resolve.
DNS_IP="${DNS_IP:-8.8.8.8}"

# forkd doctor: fail fast if KVM/cgroup-v2/Firecracker are missing.
forkd doctor

# Apply Layer-1 rules into each provisioned child netns.
# For each netns we:
#   (a) write /etc/netns/<ns>/resolv.conf so the in-netns egress proxy can resolve CONNECT targets;
#   (b) start the egress proxy INSIDE the netns (bound to 0.0.0.0:${PROXY_PORT}, reachable by
#       the guest at the tap host IP 10.42.0.1:${PROXY_PORT});
#   (c) load netns-deny.rules + netns-deny6.rules (FORWARD-chain drop) via envsubst | *tables-restore;
#   (d) assert the FORWARD rule is present, failing the container otherwise.
#
# NOTE: forkd creates per-child netns dynamically at fork time, NOT at container startup.
# This startup loop covers ONLY netns that already exist (e.g. pre-provisioned by forkd init).
# IMPORTANT: per-fork netns created AFTER this controller starts are NOT covered by this loop.
# Each netns forkd creates at runtime MUST have ALL of the following applied before the child runs:
#   1. /etc/netns/<ns>/resolv.conf (nameserver ${DNS_IP}) — so the in-netns proxy can resolve.
#   2. The egress proxy started INSIDE that netns (ip netns exec <ns> egress-proxy ...).
#   3. The FORWARD-chain Layer-1 rules loaded (envsubst | ip netns exec <ns> iptables-restore).
# This is a known live-validation limitation — see the runbook (docs/runbooks/forkd-live-validation.md)
# for the per-fork netns application procedure. Failure to enforce this leaves the per-fork
# netns WITHOUT Layer-1 default-deny and WITHOUT an in-netns egress proxy.
export GUEST_IF UPLINK_IF DNS_IP PROXY_PORT EGRESS_ALLOW
_netns_list=$(ip netns list | awk '{print $1}')
if [ -z "$_netns_list" ]; then
  echo "WARN: no child netns present at startup."
  echo "WARN: forkd creates per-fork netns at runtime — these are NOT covered by this startup loop."
  echo "WARN: Each forkd-created netns MUST have, before the child process runs:"
  echo "WARN:   1. /etc/netns/<ns>/resolv.conf with nameserver ${DNS_IP}"
  echo "WARN:   2. The egress proxy started inside the netns (ip netns exec <ns> egress-proxy)"
  echo "WARN:   3. Layer-1 FORWARD rules loaded (envsubst | ip netns exec <ns> iptables-restore)"
  echo "WARN: See docs/runbooks/forkd-live-validation.md. Omitting any of these leaves"
  echo "WARN: the per-fork netns without egress enforcement."
else
  for ns in $_netns_list; do
    # (a) Write the per-netns DNS resolver config so the in-netns proxy can resolve CONNECT targets.
    mkdir -p "/etc/netns/${ns}"
    printf 'nameserver %s\n' "${DNS_IP}" > "/etc/netns/${ns}/resolv.conf"

    # (b) Start the egress proxy INSIDE the netns. The proxy binds to 0.0.0.0:${PROXY_PORT}
    # and is reachable by the guest at the tap host IP (10.42.0.1 by default). The proxy's
    # own outbound traffic uses the netns OUTPUT chain (proxy is a process inside the netns),
    # which is NOT restricted by our FORWARD-chain Layer-1 rules.
    ip netns exec "${ns}" \
      env EGRESS_BIND="0.0.0.0:${PROXY_PORT}" EGRESS_ALLOW="${EGRESS_ALLOW}" \
      /usr/local/bin/egress-proxy &

    # Assert the egress proxy came up inside the netns before proceeding.
    _ok=0
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      if ip netns exec "${ns}" ss -ltn 2>/dev/null | grep -q ":${PROXY_PORT} "; then _ok=1; break; fi
      sleep 0.3
    done
    [ "${_ok}" = 1 ] || { echo "FATAL: egress proxy failed to start in netns ${ns}"; exit 1; }

    # (c) Load Layer-1 FORWARD-chain rules (IPv4).
    # The FORWARD chain drops guest(${GUEST_IF}) -> uplink(${UPLINK_IF}) forwarding,
    # blocking raw/non-proxy egress while leaving the controller->agent path and the
    # guest->in-netns-proxy path (local delivery) untouched.
    envsubst < /etc/forkd/netns-deny.rules | ip netns exec "${ns}" iptables-restore

    # (d) Assert the FORWARD drop rule is present; abort if not.
    ip netns exec "${ns}" iptables -S FORWARD \
      | grep -q -- "-i ${GUEST_IF} -o ${UPLINK_IF} -j DROP" \
      || { echo "FATAL: netns ${ns} missing FORWARD drop rule (${GUEST_IF} -> ${UPLINK_IF})"; exit 1; }

    # Apply IPv6 FORWARD-chain companion rules.
    envsubst < /etc/forkd/netns-deny6.rules | ip netns exec "${ns}" ip6tables-restore

    # Assert the IPv6 FORWARD drop rule is present.
    ip netns exec "${ns}" ip6tables -S FORWARD \
      | grep -q -- "-i ${GUEST_IF} -o ${UPLINK_IF} -j DROP" \
      || { echo "FATAL: netns ${ns} missing IPv6 FORWARD drop rule (${GUEST_IF} -> ${UPLINK_IF})"; exit 1; }
  done
fi

# Start the controller over TLS with bearer auth. `serve` is the required
# subcommand. NOTE: per-child-netns is a per-FORK request field (ForkdBackend
# sends "per_child_netns": true in the POST /v1/sandboxes body) — it is NOT a
# daemon flag, so it must not be passed here. --snapshot-root points the daemon
# at where `forkd from-image`/`forkd snapshot` wrote the tag (override via
# FORKD_SNAPSHOT_ROOT to match your snapshot volume).
exec forkd-controller serve \
  --tls-cert /etc/forkd/tls/cert.pem \
  --tls-key  /etc/forkd/tls/key.pem \
  --token-file /etc/forkd/token \
  --snapshot-root "${FORKD_SNAPSHOT_ROOT:-/root/.local/share/forkd/snapshots}"
