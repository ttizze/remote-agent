# Sourced by both Mac build commands. Keep the Host's identity stable across builds:
# Keychain authorizes its designated requirement, not its executable path.
bex_resolve_signing_identity() {
    if [ "${BEX_CODE_SIGN_IDENTITY:-}" = "-" ]; then
        echo 'Bex requires certificate signing: ad-hoc signing loses Keychain authorization on every update.' >&2
        return 1
    fi
    if [ -z "${BEX_CODE_SIGN_IDENTITY:-}" ]; then
        bex_signing_candidates=$(/usr/bin/security find-identity -v -p codesigning |
            /usr/bin/awk '/"(Apple Development:|Developer ID Application:)/ {print $2}')
        bex_signing_count=$(printf '%s\n' "$bex_signing_candidates" | /usr/bin/awk 'NF {count++} END {print count+0}')
        if [ "$bex_signing_count" != 1 ]; then
            echo 'Set BEX_CODE_SIGN_IDENTITY to one Apple Development or Developer ID Application certificate identity; no unique identity was found.' >&2
            return 1
        fi
        BEX_CODE_SIGN_IDENTITY=$bex_signing_candidates
    fi
    export BEX_CODE_SIGN_IDENTITY
}
