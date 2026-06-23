# forkd Live-KVM Validation Runbook

> **Scope:** End-to-end operator procedure for running the SMA-437 live integration
> tests (`forkd_live`) against a real Firecracker microVM with egress enforcement.
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling.
>
> **Validated 2026-06-23 on GCP europe-west3-b (n2-standard-4, Ubuntu 24.04,
> nested-virt).** The fork→exec→destroy + layered-egress path was confirmed: raw
> guest egress to 1.1.1.1:443 blocked; allowlisted `example.com` via proxy → CONNECT
> 200; non-allowlisted `www.google.com` via proxy → 403. Fork→exec→destroy REST flow
> returned `{"stdout":"from-a-microvm\nkernel=6.1.141\n...","exit_code":0}` (guest
> kernel 6.1.141 ≠ host — confirmed microVM isolation).

## Prerequisites

- x86_64 host with `/dev/kvm` accessible (nested-virt VM or bare-metal; see
  [Alternative hosts](#alternative-hosts)).
- **Host OS: Ubuntu 24.04 (glibc 2.39).** forkd v0.5.2 binaries require glibc ≥2.38
  and do **NOT** run on Ubuntu 22.04 (glibc 2.35), despite forkd's documentation
  claiming "22.04 or newer." Ubuntu 24.04 is required.
- Linux kernel ≥ 5.10, cgroup v2 enabled (`ls /sys/fs/cgroup/cgroup.controllers`).
- `gcloud` CLI authenticated — use `gcloud auth login`, **never paste service account
  keys into the shell or any file** (keys in the rootfs are caught by the secret scan,
  but the host shell is unguarded).
- A Rust toolchain ≥ 1.85 with the `microvm` feature available.

---

## Live-topology validation (confirm on first bring-up)

Before running the integration tests, verify these items against the real system:

1. **forkd v0.5.2 release tarball contents** — the tarball contains **only two
   binaries**: `forkd` and `forkd-controller`. There are no bundled scripts, kernel,
   or firecracker binary. Additional host setup (firecracker, guest kernel, tap,
   per-child netns) must be sourced separately; see Step 1.

2. **`FORKD_SCRIPTS_DIR` required for `forkd from-image`** — `forkd from-image
   <docker-image> --tag <tag>` builds a snapshot but shells out to `build-rootfs.sh`
   from the forkd repo. You must set `FORKD_SCRIPTS_DIR=<repo>/scripts` (pointing to
   your `git clone https://github.com/deeplethe/forkd` checkout) or the command fails.

3. **Layer-1 rules: FORWARD chain, not OUTPUT** — in forkd's per-child netns, the
   guest sits behind `forkd-tap0` and its egress is **routed** (FORWARD chain) out
   through `veth0` to the root namespace (SNAT'd to the uplink). The guest's packets
   do **NOT** traverse the netns OUTPUT chain. OUTPUT belongs to processes running
   *inside* the netns (i.e. the egress proxy). Therefore:
   - Layer-1 blocks `FORWARD -i forkd-tap0 -o veth0 -j DROP` (drops raw/forwarded
     guest egress).
   - A blanket `:OUTPUT DROP` (the old harness approach) is **incorrect**: it would
     break the in-netns proxy and the controller→agent management path, and would
     **not** block the guest's forwarded egress.

4. **Egress proxy runs INSIDE each child netns** — the proxy must be started with
   `ip netns exec forkd-child-N egress-proxy` bound to `0.0.0.0:${PROXY_PORT}`. The
   guest reaches it at the tap host-side IP `10.42.0.1:${PROXY_PORT}`. A proxy running
   in the root namespace or the Docker container namespace is **not reachable** by the
   guest.

5. **Per-netns DNS resolver** — the in-netns proxy needs a working resolver to resolve
   CONNECT targets. Write `/etc/netns/forkd-child-N/resolv.conf` with
   `nameserver 8.8.8.8` (or your chosen resolver) before starting the proxy. Without
   this, the proxy cannot resolve hostnames and all CONNECT requests fail.

6. **Layer-1 rules applied per forkd-created netns (not just at startup)** — the
   `entrypoint.sh` startup loop only covers pre-existing netns. Each netns forkd creates
   at fork time must receive (a) `/etc/netns/<ns>/resolv.conf`, (b) the in-netns proxy,
   and (c) the FORWARD-chain iptables ruleset before the child process starts.

7. **Proxy port (8443) reachable from wherever `cargo test` runs** — if running
   `cargo test` from the dev machine (not the GCP VM), ensure the VM's firewall allows
   inbound 8443 from the dev machine's IP.

---

## Step 1 — Provision the GCP VM

```bash
# GCP image family: ubuntu-2404-lts-amd64 (Ubuntu 24.04, glibc 2.39).
# Ubuntu 22.04 does NOT work — forkd v0.5.2 binaries require glibc ≥2.38.
export GCP_PROJECT=your-project
export GCP_ZONE=europe-west3-b   # zone with n2 nested-virt support (validated)
gcloud compute instances create forkd-kvm \
  --project "$GCP_PROJECT" \
  --zone "$GCP_ZONE" \
  --machine-type n2-standard-4 \
  --image-family ubuntu-2404-lts-amd64 \
  --image-project ubuntu-os-cloud \
  --enable-nested-virtualization \
  --boot-disk-size 50GB
```

Wait ~2 min for the VM to boot, then:

```bash
gcloud compute ssh forkd-kvm --project "$GCP_PROJECT" --zone "$GCP_ZONE"
```

Inside the VM, verify KVM is present:

```bash
ls -l /dev/kvm
# Expected: crw-rw---- 1 root kvm 10, 232 …
```

---

## Step 2 — Install forkd and dependencies on the VM

The forkd v0.5.2 release tarball contains **only** `forkd` and `forkd-controller`.
Additional setup is required from the forkd repo and the firecracker releases.

```bash
# Download forkd v0.5.2 binaries (sha256 verified).
FORKD_VERSION=0.5.2
FORKD_SHA256=786371cd10f75f7a24b44a9fae803569872f2cd45b7b2b19ded24a4c2d945102
curl -fsSL "https://github.com/deeplethe/forkd/releases/download/v${FORKD_VERSION}/forkd-v${FORKD_VERSION}-x86_64-linux.tar.gz" \
  -o /tmp/forkd.tgz
echo "${FORKD_SHA256}  /tmp/forkd.tgz" | sha256sum -c -
sudo tar -xz -C /usr/local/bin -f /tmp/forkd.tgz
rm /tmp/forkd.tgz
# Verify: both forkd and forkd-controller must be present.
forkd --version
forkd-controller --version

# Clone the forkd repo (scripts only — binaries are already installed above).
git clone https://github.com/deeplethe/forkd ~/forkd-repo
export FORKD_SCRIPTS_DIR=~/forkd-repo/scripts

# Install firecracker v1.10.1.
FC_VERSION=1.10.1
curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/v${FC_VERSION}/firecracker-v${FC_VERSION}-x86_64.tgz" \
  -o /tmp/fc.tgz
sudo tar -xz -C /usr/local/bin -f /tmp/fc.tgz --strip-components=1
rm /tmp/fc.tgz

# Install the guest kernel (from the forkd repo scripts).
sudo bash ~/forkd-repo/scripts/install-guest-kernel.sh

# Set up the host tap interface.
sudo bash ~/forkd-repo/scripts/host-tap.sh

# Set up per-child netns (4 slots for a 4-vCPU host).
sudo bash ~/forkd-repo/scripts/netns-setup.sh 4

# Verify all prerequisites are met.
sudo forkd doctor
# Expected: all checks green.
```

---

## Step 3 — Build the snapshot

```bash
# Build a guest snapshot using a Docker image as the rootfs source.
# FORKD_SCRIPTS_DIR must point to the cloned forkd repo's scripts directory.
sudo env FORKD_SCRIPTS_DIR=~/forkd-repo/scripts \
  forkd from-image python:3.12-slim --tag helikon
```

---

## Step 4 — Generate TLS cert/key + token

On the **VM**, in `~/`:

```bash
mkdir -p ~/forkd-tls
# Use the VM's EXTERNAL IP (the same IP used in FORKD_URL) for the SAN.
VM_IP="<your-vm-external-ip>"   # e.g. 34.90.12.34
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout ~/forkd-tls/key.pem -out ~/forkd-tls/cert.pem \
  -days 365 -subj "/CN=forkd-kvm" \
  -addext "subjectAltName=IP:${VM_IP}"

# Bearer token (random 32 bytes, base64-encoded, no newline).
openssl rand -base64 32 | tr -d '\n' > ~/forkd-token
chmod 600 ~/forkd-token ~/forkd-tls/key.pem
```

> **Note — use `VM_IP` consistently:** the same IP must appear in both the SAN above
> and in the `FORKD_URL` env var when running the integration tests (Step 7). A
> mismatch causes TLS hostname verification to fail.

---

## Step 5 — Start the forkd controller

```bash
forkd-controller serve \
  --token-file ~/forkd-token \
  --snapshot-root /root/.local/share/forkd/snapshots \
  --tls-cert ~/forkd-tls/cert.pem \
  --tls-key  ~/forkd-tls/key.pem &

# For local loopback validation only (plain HTTP, no TLS):
# forkd-controller serve --token-file ~/forkd-token \
#   --snapshot-root /root/.local/share/forkd/snapshots &
```

Verify it's up:

```bash
curl -sk https://localhost:8889/healthz
# Expected: {"status":"ok"} (no auth on /healthz)
```

---

## Step 6 — Build the egress proxy binary on the VM

```bash
# Install Rust if not already present.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Build the egress proxy example (runs inside each child netns).
cargo build -p paigasus-helikon-tools --features microvm --example egress_proxy --release
# Rename: cargo outputs egress_proxy (underscore); the harness expects egress-proxy (hyphen).
cp target/release/examples/egress_proxy /usr/local/bin/egress-proxy
chmod +x /usr/local/bin/egress-proxy
```

---

## Step 7 — Apply per-netns proxy + rules (validated topology)

In forkd's per-child netns topology:
- The guest VM is behind `forkd-tap0` (host-side IP `10.42.0.1`, guest IP `10.42.0.2`).
- Guest egress is **routed** (FORWARD chain) via `veth0` to the root namespace (SNAT'd).
- The guest's packets do **NOT** traverse the netns OUTPUT chain.
- Layer-1 blocks `FORWARD -i forkd-tap0 -o veth0 -j DROP`.
- The egress proxy must run **inside** each child netns.

For each forkd child netns (replace `forkd-child-N` with the actual netns names from
`ip netns list`):

```bash
NS=forkd-child-0   # repeat for each netns

# (a) DNS resolver for the in-netns proxy.
mkdir -p /etc/netns/${NS}
echo "nameserver 8.8.8.8" > /etc/netns/${NS}/resolv.conf

# (b) Start egress proxy inside the netns.
ip netns exec ${NS} \
  env EGRESS_BIND=0.0.0.0:8443 EGRESS_ALLOW=example.com \
  /usr/local/bin/egress-proxy &

# (c) Apply Layer-1 FORWARD-chain rules.
GUEST_IF=forkd-tap0 UPLINK_IF=veth0 \
  envsubst < docker/forkd/netns-deny.rules | ip netns exec ${NS} iptables-restore
GUEST_IF=forkd-tap0 UPLINK_IF=veth0 \
  envsubst < docker/forkd/netns-deny6.rules | ip netns exec ${NS} ip6tables-restore

# (d) Assert the FORWARD drop rule is present.
ip netns exec ${NS} iptables -S FORWARD \
  | grep -q -- '-i forkd-tap0 -o veth0 -j DROP' \
  || { echo "FATAL: FORWARD drop rule missing in ${NS}"; exit 1; }
```

**Validate egress enforcement** (expected results from the live run):

```bash
# Raw egress to 1.1.1.1:443 — BLOCKED by the FORWARD rule (no response / timeout).
ip netns exec ${NS} curl --max-time 3 https://1.1.1.1/ || echo "blocked as expected"

# Via proxy, allowlisted domain — ALLOWED (CONNECT 200).
ip netns exec ${NS} curl -x http://10.42.0.1:8443 https://example.com/ -so /dev/null \
  && echo "proxy: example.com allowed"

# Via proxy, non-allowlisted domain — DENIED (403 fast, no hang).
ip netns exec ${NS} curl -x http://10.42.0.1:8443 https://www.google.com/ -so /dev/null \
  && echo "should not reach here" \
  || echo "proxy: www.google.com denied (403)"
```

---

## Step 8 — Run the live integration tests

Back on the **dev machine** (or on the VM if Rust is installed there):

```bash
export FORKD_URL="https://<VM_EXTERNAL_IP>:8889"
export FORKD_TOKEN="$(cat ~/forkd-token)"   # or copy from the VM
export FORKD_SNAPSHOT="helikon"
export FORKD_PROXY="${FORKD_URL%:*}:8443"   # same host, egress proxy port
# If using a self-signed cert, point to the cert PEM:
export FORKD_CA="/path/to/forkd-tls/cert.pem"

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
immediately for non-allowlisted domains; a hang indicates the netns FORWARD-chain
default-deny is not in effect and direct traffic is leaking past the proxy).

### Paste into the PR

Copy the full `cargo test … -- --nocapture` output and paste it into the PR
description under a `<details><summary>Live KVM validation output</summary>…</details>` block.

---

## Step 9 — Teardown

```bash
gcloud compute instances delete forkd-kvm \
  --project "$GCP_PROJECT" --zone "$GCP_ZONE" --quiet
```

---

## Alternative hosts

| Provider | Instance type | Notes |
|----------|--------------|-------|
| **GCP** | `n2-standard-4` or larger | `--enable-nested-virtualization` flag; image family `ubuntu-2404-lts-amd64` (24.04 required); cheapest nested-virt option |
| **AWS** | `c8i.*` (nested-virt) | Confirm `/dev/kvm` before starting; use Ubuntu 24.04 AMI |
| **AWS** | `.metal` bare-metal | No nested-virt needed; `/dev/kvm` is directly available; use Ubuntu 24.04 AMI |
| **Hetzner** | AX-line bare-metal (`AX41`, `AX52`, etc.) | Dedicated x86_64; `/dev/kvm` available out of the box; hourly billing; Ubuntu 24.04 image available |
| **DigitalOcean** | `metal` bare-metal | DO bare-metal Droplets expose `/dev/kvm`; use Ubuntu 24.04 |

For non-GCP hosts, provision the instance using the provider's CLI/UI with Ubuntu 24.04,
install the dependencies from Step 2 manually, then follow Steps 3–9.

---

## Troubleshooting

**`forkd doctor` fails: "KVM not available"**
- Confirm `/dev/kvm` exists and is readable: `ls -l /dev/kvm`.
- For GCP: ensure `--enable-nested-virtualization` was set at VM creation (cannot be added after).
- For Docker: confirm `devices: ["/dev/kvm:/dev/kvm"]` is in `docker-compose.yml` and the host has KVM.

**`forkd from-image` fails with "build-rootfs.sh not found" or similar**
- `FORKD_SCRIPTS_DIR` must point to the `scripts/` directory of a `git clone
  https://github.com/deeplethe/forkd`. The release tarball does not include scripts.

**`forkd` or `forkd-controller` fails with "GLIBC_2.3x not found"**
- The host OS is too old. forkd v0.5.2 binaries require glibc ≥2.38. Upgrade to
  Ubuntu 24.04 (glibc 2.39).

**`entrypoint.sh` exits with "FATAL: netns … missing FORWARD drop rule"**
- The iptables rules failed to load or the interface names don't match. Check
  `ip netns list` and `ip netns exec <ns> ip link list` to confirm `GUEST_IF`/`UPLINK_IF`
  values. If forkd uses different interface names, set `GUEST_IF` and `UPLINK_IF`
  environment variables accordingly.

**TLS handshake failure in tests**
- Confirm `FORKD_CA` points to the correct cert PEM.
- The cert SAN must include the VM's IP used in `FORKD_URL`. Regenerate with the correct
  `-addext "subjectAltName=IP:…"` if needed.

**Egress-deny test hangs (> 8 seconds)**
- The netns FORWARD-chain default-deny is not in effect.
- Verify: `ip netns exec <ns> iptables -S FORWARD` shows `-A FORWARD -i forkd-tap0 -o veth0 -j DROP`.
- Do NOT look for `OUTPUT DROP` — that is the old (incorrect) approach.

**In-netns proxy fails to resolve CONNECT targets**
- Check that `/etc/netns/<ns>/resolv.conf` exists and contains a valid `nameserver` line.
- Without this file, the proxy uses the host's resolver configuration, which may not be
  visible from inside the netns.

**Secret scan fails**
- Check `grep -RInE '(BEGIN [A-Z ]*PRIVATE KEY|AKIA…|Bearer …)' "$WORK/rootfs"` output.
- Never copy service account key files or tokens into the rootfs staging directory.
