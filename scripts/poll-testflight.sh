#!/usr/bin/env bash
# Wait for App Store Connect processing to reach a verified VALID state.
set -euo pipefail

[[ $# -ge 1 && $# -le 2 ]] || { echo "usage: poll-testflight.sh delivery-id [timeout-seconds]" >&2; exit 2; }
delivery_id=$1
timeout=${2:-1800}
: "${ASC_API_KEY_PATH:?ASC_API_KEY_PATH is required}"
: "${ASC_KEY_ID:?ASC_KEY_ID is required}"
: "${ASC_ISSUER_ID:?ASC_ISSUER_ID is required}"
[[ -f $ASC_API_KEY_PATH ]] || { echo 'App Store Connect API key file does not exist' >&2; exit 1; }
[[ $ASC_KEY_ID =~ ^[A-Za-z0-9._-]+$ ]] || { echo 'invalid App Store Connect key id' >&2; exit 1; }
[[ $delivery_id =~ ^[A-Za-z0-9-]+$ ]] || { echo 'invalid delivery id' >&2; exit 1; }
[[ $timeout =~ ^[0-9]+$ ]] || { echo 'invalid timeout' >&2; exit 1; }

deadline=$((SECONDS + timeout))
while (( SECONDS < deadline )); do
    status=$(xcrun altool --build-status --delivery-id "$delivery_id" \
        --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID" 2>&1) || {
        echo 'App Store Connect status request failed; retrying.' >&2
        sleep 30
        continue
    }
    if grep -Fq 'BUILD-STATUS: VALID' <<<"$status" &&
        grep -Fq 'IMPORT-STATUS: VALID' <<<"$status" &&
        grep -Fq 'IS-ON-APP-STORE-CONNECT: true' <<<"$status"; then
        echo 'TestFlight processing reached VALID.'
        exit 0
    fi
    if grep -Eiq 'BUILD-STATUS: (INVALID|FAILED)|IMPORT-STATUS: (INVALID|FAILED)' <<<"$status"; then
        echo 'App Store Connect rejected the uploaded build.' >&2
        exit 1
    fi
    sleep 30
done
echo 'Timed out waiting for App Store Connect processing.' >&2
exit 1
