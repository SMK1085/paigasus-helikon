#!/usr/bin/env bash
# Provision a GCP nested-virt VM to run the forkd+KVM harness. Requires gcloud auth.
set -euo pipefail
: "${GCP_PROJECT:?set GCP_PROJECT}"
: "${GCP_ZONE:=europe-west1-b}"
: "${VM_NAME:=forkd-kvm}"
: "${MACHINE:=n2-standard-4}"

gcloud compute instances create "$VM_NAME" \
  --project "$GCP_PROJECT" --zone "$GCP_ZONE" --machine-type "$MACHINE" \
  --enable-nested-virtualization \
  --image-family ubuntu-2204-lts --image-project ubuntu-os-cloud \
  --metadata=startup-script='#!/bin/bash
    set -e
    apt-get update && apt-get install -y docker.io docker-compose-plugin
    systemctl enable --now docker
    # KVM check inside the VM:
    ls -l /dev/kvm || echo "WARN: /dev/kvm absent — nested virt not enabled?"
  '
echo "VM $VM_NAME up in $GCP_ZONE. SSH in, copy docker/forkd + the cargo-built egress-proxy, then 'docker compose up'."
echo "See docs/runbooks/forkd-live-validation.md."
