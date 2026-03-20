#!/usr/bin/env bash
set -euo pipefail

# Sign the extension for Firefox using AMO API credentials.
# Requires: JWT_ISSUER and JWT_SECRET env vars
# Produces: web-ext-artifacts/*.xpi

cd "$(dirname "$0")"

if [[ -z "${JWT_ISSUER:-}" || -z "${JWT_SECRET:-}" ]]; then
  echo "error: JWT_ISSUER and JWT_SECRET must be set" >&2
  exit 1
fi

web-ext sign \
  --api-key="$JWT_ISSUER" \
  --api-secret="$JWT_SECRET" \
  --channel=unlisted \
  --ignore-files sign.sh
