#!/usr/bin/env bash
set -euo pipefail
: "${GCP_PROJECT:?set GCP_PROJECT}"
: "${GCP_ZONE:=europe-west1-b}"
: "${VM_NAME:=forkd-kvm}"
gcloud compute instances delete "$VM_NAME" --project "$GCP_PROJECT" --zone "$GCP_ZONE" --quiet
