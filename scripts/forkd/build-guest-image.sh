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

# --- assemble a minimal rootfs (busybox + curl + ca-certs) ---
mkdir -p "$WORK/rootfs"/{bin,etc,proc,sys,dev}
busybox --install -s "$WORK/rootfs/bin"
cp "$(command -v curl)" "$WORK/rootfs/bin/" || true
cat > "$WORK/rootfs/etc/profile" <<EOF
export HTTP_PROXY=http://${PROXY_ADDR}
export HTTPS_PROXY=http://${PROXY_ADDR}
export http_proxy=http://${PROXY_ADDR}
export https_proxy=http://${PROXY_ADDR}
EOF

# --- SECRET SCAN: refuse to snapshot if any secret material is present ---
if grep -RInE '(BEGIN [A-Z ]*PRIVATE KEY|AKIA[0-9A-Z]{16}|Bearer [A-Za-z0-9._-]{20,})' "$WORK/rootfs"; then
  echo "FATAL: secret-like material found in rootfs — refusing to snapshot (CoW is shared to every child)."
  exit 1
fi

# Package $WORK/rootfs into $ROOTFS ext4 image.
# Determine required size: at least 32 MB or actual usage + 50% headroom.
ROOTFS_SIZE_MB=$(du -sm "$WORK/rootfs" | awk '{print int($1 * 1.5 + 32)}')
mkdir -p "$(dirname "$ROOTFS")"
dd if=/dev/zero of="$ROOTFS" bs=1M count="${ROOTFS_SIZE_MB}" status=none
mkfs.ext4 -F -L helikon "$ROOTFS" > /dev/null
MOUNT_DIR="$(mktemp -d)"
mount -o loop "$ROOTFS" "$MOUNT_DIR"
cp -a "$WORK/rootfs/." "$MOUNT_DIR/"
umount "$MOUNT_DIR"
rmdir "$MOUNT_DIR"
rm -rf "$WORK"

# --- warm + snapshot ---
curl -fsSL -X POST "${FORKD_URL%/}/v1/snapshots" \
  -H "Authorization: Bearer ${FORKD_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d "{\"tag\":\"${SNAPSHOT_TAG}\",\"kernel\":\"${KERNEL}\",\"rootfs\":\"${ROOTFS}\",\"rw\":true,\"tap\":\"forkd-tap0\",\"boot_wait_secs\":10}"
echo "snapshot '${SNAPSHOT_TAG}' requested; poll GET /v1/snapshots for status=ready"
