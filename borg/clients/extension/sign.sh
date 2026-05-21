#!/usr/bin/env bash
set -euo pipefail

# Sign the extension for Firefox using AMO API credentials.
# Requires: MOZILLA_JWT_ISSUER and MOZILLA_JWT_SECRET env vars
# Produces: web-ext-artifacts/*.xpi

cd "$(dirname "$0")"

if [[ -z "${MOZILLA_JWT_ISSUER:-}" || -z "${MOZILLA_JWT_SECRET:-}" ]]; then
  echo "error: MOZILLA_JWT_ISSUER and MOZILLA_JWT_SECRET must be set" >&2
  exit 1
fi

web-ext sign \
  --api-key="$MOZILLA_JWT_ISSUER" \
  --api-secret="$MOZILLA_JWT_SECRET" \
  --channel=unlisted \
  --ignore-files sign.sh
