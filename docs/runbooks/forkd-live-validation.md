# forkd Live-KVM Validation Runbook

> **Scope:** End-to-end operator procedure for running the SMA-437 live integration
> tests (`forkd_live`) against a real Firecracker microVM with egress enforcement.
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling.

## Prerequisites

- x86_64 host with `/dev/kvm` accessible (nested-virt VM or bare-metal; see
  [Alternative hosts](#alternative-hosts)).
- Linux kernel ≥ 5.10, cgroup v2 enabled (`ls /sys/fs/cgroup/cgroup.controllers`).
- Docker ≥ 23 with Compose v2 plugin (`docker compose version`).
- `gcloud` CLI authenticated — use `gcloud auth login`, **never paste service account
  keys into the shell or any file** (keys in the rootfs are caught by the secret scan,
  but the host shell is unguarded).
- A Rust toolchain ≥ 1.85 with the `microvm` feature available.

---

## Step 1 — Provision the GCP VM

```bash
export GCP_PROJECT=your-project
export GCP_ZONE=europe-west1-b   # any zone with n2 nested-virt support
bash scripts/forkd/gcp-launch.sh
```

The startup script installs Docker and the Compose plugin. Wait ~2 min, then:

```bash
gcloud compute ssh forkd-kvm --project "$GCP_PROJECT" --zone "$GCP_ZONE"
```

Inside the VM, verify KVM is present:

```bash
ls -l /dev/kvm
# Expected: crw-rw---- 1 root kvm 10, 232 …
```

---

## Step 2 — Build the egress proxy binary on your laptop

On the **macOS/Linux dev machine** (cross-compilation not required; compile for
`x86_64-unknown-linux-gnu` or build directly on the GCP VM):

```bash
# Option A: build on the GCP VM (recommended; avoids cross-compilation)
# SSH in, install Rust, then:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# ... then run the cargo build below inside the VM.

# Option B: cross-compile on macOS (requires cargo-cross or cross-rs)
cross build -p paigasus-helikon-tools --features microvm --example egress_proxy \
  --target x86_64-unknown-linux-gnu --release
scp target/x86_64-unknown-linux-gnu/release/examples/egress-proxy \
  forkd-kvm:~/docker/forkd/egress-proxy

# Option C: build directly on the VM (simplest)
cargo build -p paigasus-helikon-tools --features microvm --example egress_proxy --release
cp target/release/examples/egress_proxy ~/docker/forkd/egress-proxy
```

---

## Step 3 — Copy the harness to the VM

```bash
# From your local repo root:
gcloud compute scp --recurse docker/forkd \
  forkd-kvm:~/ --project "$GCP_PROJECT" --zone "$GCP_ZONE"
```

The `docker/forkd/` directory should now be present on the VM at `~/forkd/`.
The `egress-proxy` binary (from Step 2) must be at `~/forkd/egress-proxy`.

---

## Step 4 — Generate TLS cert/key + token

On the **VM**, in `~/forkd/`:

```bash
mkdir -p tls
# Self-signed cert for the controller (non-loopback TLS; see Note below).
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout tls/key.pem -out tls/cert.pem \
  -days 365 -subj "/CN=forkd-kvm" \
  -addext "subjectAltName=IP:$(hostname -I | awk '{print $1}')"

# Bearer token (random 32 bytes, base64-encoded, no newline).
openssl rand -base64 32 | tr -d '\n' > token
chmod 600 token tls/key.pem
```

> **Note — real-CA non-loopback TLS:** for long-running or shared validation
> environments, replace the self-signed cert with one issued by Let's Encrypt
> (add a DNS A record for the VM's public IP and use `certbot`). Pass the CA
> PEM path as `FORKD_CA` when running the live tests; for a self-signed cert,
> copy `tls/cert.pem` to the test machine and set `FORKD_CA` to that path.

---

## Step 5 — Start the harness (`docker compose up`)

```bash
cd ~/forkd
# Set the allow-list to suit your test (example.com is the default).
export EGRESS_ALLOW="example.com"
docker compose up --build
```

**What happens:**
1. The Docker image is built (forkd v0.5.2 + iptables + gettext-base).
2. The container starts with `/dev/kvm` passed through (`devices: ["/dev/kvm:/dev/kvm"]`)
   and `cap_add: NET_ADMIN` for `ip netns` + `iptables` inside.
3. `entrypoint.sh` starts the egress proxy on port 8443, runs `forkd doctor`
   (KVM/cgroup-v2/Firecracker check — fails fast if KVM is absent), loads the
   `netns-deny.rules` iptables ruleset into each child netns, and asserts each
   netns's OUTPUT policy is DROP before starting the controller.
4. The forkd controller listens on port 8889 (TLS, bearer-auth).

Verify it's up:

```bash
curl -sk https://localhost:8889/healthz
# Expected: {"status":"ok"} (no auth on /healthz)
```

---

## Step 6 — Build the guest image

```bash
export FORKD_URL="https://localhost:8889"
export FORKD_TOKEN="$(cat ~/forkd/token)"
export PROXY_ADDR="172.17.0.1:8443"   # Docker bridge IP → egress proxy in the container
bash scripts/forkd/build-guest-image.sh
```

The script:
- Assembles a minimal busybox + curl rootfs with `HTTP_PROXY`/`HTTPS_PROXY` baked in.
- **Secret-scans the rootfs** (grep for private keys, AWS access key IDs, bearer tokens)
  and fails if any are found — no secrets in the CoW base image shared to every child.
- Packages the rootfs into an ext4 image.
- POSTs `POST /v1/snapshots` to warm and register the snapshot as `helikon`.

Poll until ready:

```bash
curl -sk -H "Authorization: Bearer ${FORKD_TOKEN}" \
  "${FORKD_URL}/v1/snapshots" | jq '.[] | select(.tag=="helikon") | .status'
# Expected: "ready"
```

---

## Step 7 — Run the live integration tests

Back on the **dev machine** (or on the VM if Rust is installed there):

```bash
export FORKD_URL="https://<VM_EXTERNAL_IP>:8889"
export FORKD_TOKEN="$(cat ~/forkd/token)"   # or copy from the VM
export FORKD_SNAPSHOT="helikon"
export FORKD_PROXY="${FORKD_URL%:*}:8443"   # same host, egress proxy port
# If using a self-signed cert, point to the cert PEM:
export FORKD_CA="/path/to/forkd/tls/cert.pem"

cargo test -p paigasus-helikon-tools \
  --features microvm --test forkd_live \
  -- --nocapture
```

### Expected output

```
test live_forkd_runs_bash_in_a_microvm ... ok
test live_forkd_denies_nonallowlisted_egress ... ok
```

The egress-deny test must complete in **< 8 seconds** (the proxy returns 403
immediately for non-allowlisted domains; a hang indicates the netns default-deny
is not in effect and direct traffic is leaking past the proxy).

### Paste into the PR

Copy the full `cargo test … -- --nocapture` output and paste it into the PR
description under a `<details><summary>Live KVM validation output</summary>…</details>` block.

---

## Step 8 — Teardown

```bash
bash scripts/forkd/gcp-teardown.sh
```

This deletes the GCP VM and its boot disk. The `forkd-snapshots` Docker volume is
destroyed with the container.

---

## Alternative hosts

| Provider | Instance type | Notes |
|----------|--------------|-------|
| **GCP** | `n2-standard-4` or larger | `--enable-nested-virtualization` flag; cheapest nested-virt option |
| **AWS** | `c8i.*` (nested-virt) | Confirm `/dev/kvm` before starting; use `--enable-nested-virtualization` equiv in the launch template |
| **AWS** | `.metal` bare-metal | No nested-virt needed; `/dev/kvm` is directly available; any instance family with KVM support |
| **Hetzner** | AX-line bare-metal (`AX41`, `AX52`, etc.) | Dedicated x86_64; `/dev/kvm` available out of the box; hourly billing |
| **DigitalOcean** | `metal` bare-metal | DO bare-metal Droplets expose `/dev/kvm`; `n2-standard-4` equivalent in DO Premium Intel |

For non-GCP hosts, skip `gcp-launch.sh` and `gcp-teardown.sh` — provision the instance
using the provider's CLI/UI, install Docker manually, then follow Steps 3–8.

---

## Troubleshooting

**`forkd doctor` fails: "KVM not available"**
- Confirm `/dev/kvm` exists and is readable: `ls -l /dev/kvm`.
- For GCP: ensure `--enable-nested-virtualization` was set at VM creation (cannot be added after).
- For Docker: confirm `devices: ["/dev/kvm:/dev/kvm"]` is in `docker-compose.yml` and the host has KVM.

**`entrypoint.sh` exits with "FATAL: netns … OUTPUT policy is not DROP"**
- The iptables rules failed to load. Check `ip netns list` — if empty, forkd has not created any netns yet; the entrypoint runs the loop after `forkd doctor` but before `forkd-controller` is up. Adjust timing or run `forkd init-netns` first if forkd exposes that command.

**TLS handshake failure in tests**
- Confirm `FORKD_CA` points to the correct cert PEM.
- The cert SAN must include the VM's IP used in `FORKD_URL`. Regenerate with the correct `-addext "subjectAltName=IP:…"` if needed.

**Egress-deny test hangs**
- The netns default-deny is not in effect. Verify `iptables -S OUTPUT` inside the child netns shows `POLICY DROP`.
- Confirm `FORKD_PROXY` is reachable from the test machine (the proxy port 8443 should be the forkd host, not the container's internal address).

**Secret scan fails**
- Check `grep -RInE '(BEGIN [A-Z ]*PRIVATE KEY|AKIA…|Bearer …)' "$WORK/rootfs"` output.
- Never copy service account key files or tokens into the rootfs staging directory.
