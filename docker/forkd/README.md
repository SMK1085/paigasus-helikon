# docker/forkd — Dockerized forkd + KVM harness

This directory contains the Docker Compose harness for running the forkd Firecracker
microVM controller with the Helikon egress proxy on a **x86_64 Linux KVM host**.

## What's here

| File | Purpose |
|------|---------|
| `Dockerfile` | Ubuntu 22.04 image: forkd v0.5.2 + iptables + the egress-proxy binary (mounted from host) |
| `docker-compose.yml` | Compose config: `/dev/kvm` passthrough, `NET_ADMIN` cap, volume mounts |
| `entrypoint.sh` | Loads per-netns iptables rules, asserts OUTPUT=DROP, starts proxy + controller |
| `netns-deny.rules` | Layer-1 default-deny iptables ruleset (template; env-substituted by entrypoint) |

## Quick start

See [`docs/runbooks/forkd-live-validation.md`](../../docs/runbooks/forkd-live-validation.md)
for the full end-to-end procedure: GCP VM provisioning, TLS cert generation, proxy binary
copy, `docker compose up`, guest image build, and running the live integration tests.

> **Host requirements:** x86_64, `/dev/kvm` available, Linux kernel ≥ 5.10 (cgroup v2),
> Docker ≥ 23 with Compose v2 plugin.
