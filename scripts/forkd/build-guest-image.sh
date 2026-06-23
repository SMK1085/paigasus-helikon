#!/usr/bin/env bash
# Build + warm a forkd guest snapshot for Helikon. Run on the x86_64 KVM host.
set -euo pipefail
: "${ROOTFS:=/var/lib/forkd/rootfs/helikon.ext4}"
: "${KERNEL:=/var/lib/forkd/kernels/vmlinux-6.1}"
: "${PROXY_ADDR:?set PROXY_ADDR=host:8443 (the egress proxy reachable from the guest netns)}"
: "${FORKD_URL:?set FORKD_URL}"
: "${FORKD_TOKEN:?set FORKD_TOKEN}"
: "${SNAPSHOT_TAG:=helikon}"
WORK="$(mktemp -d)"
MOUNT_DIR="$WORK/mnt"
mkdir -p "$MOUNT_DIR"
# Cleanup trap: unmount and remove work dir on any exit (success, error, or signal).
trap 'umount "$MOUNT_DIR" 2>/dev/null || true; rm -rf "$WORK"' EXIT

# --- assemble a minimal rootfs (busybox + ca-certs) ---
# The guest uses busybox wget (static) for HTTP egress — it is installed via
# `busybox --install` below. Do NOT copy the host curl binary: it is
# dynamically linked and its shared libraries are not present in the guest,
# causing it to crash at runtime. If curl is required, install a static build
# explicitly (e.g. from https://github.com/moparisthebest/static-curl/releases).
mkdir -p "$WORK/rootfs"/{bin,etc,proc,sys,dev}
busybox --install -s "$WORK/rootfs/bin"

# CA bundle: guest-side HTTPS clients need trusted roots to validate the proxy's
# upstream TLS. Copy the host bundle into the rootfs; fail hard if it is absent
# rather than shipping a guest that silently cannot validate certificates.
# (The guest also needs a TLS-capable HTTP client — confirm on first live
# bring-up, see docs/runbooks/forkd-live-validation.md.)
if [ ! -d /etc/ssl/certs ]; then
  echo "FATAL: host CA bundle /etc/ssl/certs not found — cannot provision guest TLS trust"
  exit 1
fi
mkdir -p "$WORK/rootfs/etc/ssl/certs"
cp -a /etc/ssl/certs/. "$WORK/rootfs/etc/ssl/certs/"

cat > "$WORK/rootfs/etc/profile" <<EOF
export HTTP_PROXY=http://${PROXY_ADDR}
export HTTPS_PROXY=http://${PROXY_ADDR}
export http_proxy=http://${PROXY_ADDR}
export https_proxy=http://${PROXY_ADDR}
EOF

# --- SECRET SCAN: refuse to snapshot if any secret material is present ---
if grep -RInE '(BEGIN [A-Z ]*PRIVATE KEY|AKIA[0-9A-Z]{16}|Bearer [A-Za-z0-9._-]{20,}|"private_key"|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})' "$WORK/rootfs"; then
  echo "FATAL: secret-like material found in rootfs — refusing to snapshot (CoW is shared to every child)."
  exit 1
fi

# Package $WORK/rootfs into $ROOTFS ext4 image.
# Determine required size: at least 32 MB or actual usage + 50% headroom.
ROOTFS_SIZE_MB=$(du -sm "$WORK/rootfs" | awk '{print int($1 * 1.5 + 32)}')
mkdir -p "$(dirname "$ROOTFS")"
dd if=/dev/zero of="$ROOTFS" bs=1M count="${ROOTFS_SIZE_MB}" status=none
mkfs.ext4 -F -L helikon "$ROOTFS" > /dev/null
mount -o loop "$ROOTFS" "$MOUNT_DIR"
cp -a "$WORK/rootfs/." "$MOUNT_DIR/"
umount "$MOUNT_DIR"

# --- warm + snapshot ---
curl -fsSL -X POST "${FORKD_URL%/}/v1/snapshots" \
  -H "Authorization: Bearer ${FORKD_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d "{\"tag\":\"${SNAPSHOT_TAG}\",\"kernel\":\"${KERNEL}\",\"rootfs\":\"${ROOTFS}\",\"rw\":true,\"tap\":\"forkd-tap0\",\"boot_wait_secs\":10}"
echo "snapshot '${SNAPSHOT_TAG}' requested; poll GET /v1/snapshots for status=ready"
