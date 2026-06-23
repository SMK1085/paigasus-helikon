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
    # KVM check — fail hard; a missing /dev/kvm means nested virtualization was not
    # enabled at VM creation (cannot be added after the fact on GCP). A broken host
    # silently producing non-KVM runs is worse than a loud provisioning failure.
    if [ ! -e /dev/kvm ]; then echo "FATAL: /dev/kvm absent — nested virtualization not enabled"; exit 1; fi
  '
echo "VM $VM_NAME up in $GCP_ZONE. SSH in, copy docker/forkd + the cargo-built egress-proxy, then 'docker compose up'."
echo "See docs/runbooks/forkd-live-validation.md."
