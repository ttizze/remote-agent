#!/usr/bin/env bash
# Exercise signing setup with fake command-line tools; no Apple APIs or credentials.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT

fake_bin="$temporary/bin"
mkdir -p "$fake_bin"

cat > "$fake_bin/security" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
{
    printf 'argv'
    for argument in "$@"; do
        printf '[%s]' "$argument"
    done
    printf '\n'
} >> "$FAKE_SECURITY_LOG"
last_argument=
for argument in "$@"; do
    last_argument=$argument
done
case ${1:-} in
    create-keychain)
        : > "$last_argument"
        ;;
    delete-keychain)
        rm -f "$2"
        ;;
    find-identity)
        printf '  1) TEST "Developer ID Application: Contract Test"\n'
        printf '  1 valid identities found\n'
        ;;
    cms)
        printf 'fake-profile\n'
        ;;
esac
EOF
chmod 700 "$fake_bin/security"

cat > "$fake_bin/openssl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
{
    printf 'argv'
    for argument in "$@"; do
        printf '[%s]' "$argument"
    done
    printf '\n'
} >> "$FAKE_OPENSSL_LOG"
if [[ ${FAKE_OPENSSL_FAIL:-0} == 1 ]]; then
    exit 91
fi
output=
previous=
for argument in "$@"; do
    if [[ $previous == -out ]]; then
        output=$argument
        break
    fi
    previous=$argument
done
: "${output:?fake openssl expected -out}"
printf 'fake-pem\n' > "$output"
EOF
chmod 700 "$fake_bin/openssl"

cat > "$fake_bin/PlistBuddy" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '01234567-89ab-cdef-0123-456789abcdef\n'
EOF
chmod 700 "$fake_bin/PlistBuddy"

assert_clean_private_files() {
    local private_dir=$1
    local keychain=$2
    test -f "$keychain"
    test ! -e "$private_dir/macos-distribution.p12"
    test ! -e "$private_dir/ios-distribution.p12"
    test ! -e "$private_dir/macos-distribution-password"
    test ! -e "$private_dir/ios-distribution-password"
    test ! -e "$private_dir/macos-distribution.pem"
    test ! -e "$private_dir/ios-distribution.pem"
}

run_macos_success() {
    local root=$1
    mkdir -p "$root/runner" "$root/home"
    : > "$root/security.log"
    : > "$root/openssl.log"
    env -i \
        PATH="$PATH" \
        HOME="$root/home" \
        GITHUB_ENV="$root/github-env" \
        RUNNER_TEMP="$root/runner" \
        MACOS_SIGNING_CERTIFICATE_BASE64=Y2VydA== \
        MACOS_SIGNING_CERTIFICATE_PASSWORD=unit-test-certificate-password \
        SECURITY_BIN="$fake_bin/security" \
        OPENSSL_BIN="$fake_bin/openssl" \
        FAKE_SECURITY_LOG="$root/security.log" \
        FAKE_OPENSSL_LOG="$root/openssl.log" \
        "$repo_dir/scripts/prepare-macos-signing.sh" >/dev/null
    private_dir="$root/runner/macos-distribution"
    assert_clean_private_files "$private_dir" "$private_dir/macos-distribution.keychain-db"
    test -f "$root/runner/macos-distribution-keychain"
    test "$(stat -c '%a' "$private_dir" 2>/dev/null || stat -f '%Lp' "$private_dir")" = 700
    grep -F 'APP_CODE_SIGN_IDENTITY=Developer ID Application: Contract Test' "$root/github-env" >/dev/null
    grep -F '[create-keychain][-p][]' "$root/security.log" >/dev/null
    grep -F '[unlock-keychain][-p][]' "$root/security.log" >/dev/null
    grep -F '[-k][]' "$root/security.log" >/dev/null
    grep -F '[-passin][file:' "$root/openssl.log" >/dev/null
    if grep -F 'unit-test-certificate-password' "$root/security.log" "$root/openssl.log" >/dev/null; then
        return 1
    fi
    env -i \
        PATH="$PATH" \
        HOME="$root/home" \
        RUNNER_TEMP="$root/runner" \
        APPLE_API_KEY_ID=CONTRACT-APPLE-KEY \
        SECURITY_BIN="$fake_bin/security" \
        FAKE_SECURITY_LOG="$root/security.log" \
        "$repo_dir/scripts/cleanup-macos-release.sh"
    test ! -e "$private_dir"
    test ! -e "$root/runner/macos-distribution-keychain"
    grep -F '[delete-keychain][' "$root/security.log" >/dev/null
}

run_ios_success() {
    local root=$1
    mkdir -p "$root/runner" "$root/home"
    : > "$root/security.log"
    : > "$root/openssl.log"
    env -i \
        PATH="$PATH" \
        HOME="$root/home" \
        GITHUB_ENV="$root/github-env" \
        RUNNER_TEMP="$root/runner" \
        IOS_DISTRIBUTION_CERTIFICATE_BASE64=Y2VydA== \
        IOS_DISTRIBUTION_CERTIFICATE_PASSWORD=unit-test-certificate-password \
        IOS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ== \
        SECURITY_BIN="$fake_bin/security" \
        OPENSSL_BIN="$fake_bin/openssl" \
        PLIST_BUDDY_BIN="$fake_bin/PlistBuddy" \
        FAKE_SECURITY_LOG="$root/security.log" \
        FAKE_OPENSSL_LOG="$root/openssl.log" \
        "$repo_dir/scripts/prepare-ios-signing.sh" >/dev/null
    private_dir="$root/runner/ios-distribution"
    assert_clean_private_files "$private_dir" "$private_dir/ios-distribution.keychain-db"
    test -f "$root/runner/ios-distribution-keychain"
    test "$(stat -c '%a' "$private_dir" 2>/dev/null || stat -f '%Lp' "$private_dir")" = 700
    test -f "$root/home/Library/MobileDevice/Provisioning Profiles/01234567-89ab-cdef-0123-456789abcdef.mobileprovision"
    grep -F 'IOS_SIGNING_KEYCHAIN=' "$root/github-env" >/dev/null
    grep -F '[create-keychain][-p][]' "$root/security.log" >/dev/null
    grep -F '[unlock-keychain][-p][]' "$root/security.log" >/dev/null
    grep -F '[-k][]' "$root/security.log" >/dev/null
    grep -F '[-passin][file:' "$root/openssl.log" >/dev/null
    if grep -F 'unit-test-certificate-password' "$root/security.log" "$root/openssl.log" >/dev/null; then
        return 1
    fi
    profile_path=$(sed -n 's/^IOS_SIGNING_PROFILE_PATH=//p' "$root/github-env")
    test -n "$profile_path"
    env -i \
        PATH="$PATH" \
        HOME="$root/home" \
        RUNNER_TEMP="$root/runner" \
        ASC_KEY_ID=CONTRACT-ASC-KEY \
        SECURITY_BIN="$fake_bin/security" \
        FAKE_SECURITY_LOG="$root/security.log" \
        "$repo_dir/scripts/cleanup-ios-release.sh"
    test ! -e "$private_dir"
    test ! -e "$root/runner/ios-distribution-keychain"
    test ! -e "$profile_path"
    grep -F '[delete-keychain][' "$root/security.log" >/dev/null
}

run_failure_cleanup() {
    local root=$1
    mkdir -p "$root/runner"
    if env -i \
        PATH="$PATH" \
        GITHUB_ENV="$root/github-env" \
        RUNNER_TEMP="$root/runner" \
        MACOS_SIGNING_CERTIFICATE_BASE64=Y2VydA== \
        MACOS_SIGNING_CERTIFICATE_PASSWORD=unit-test-certificate-password \
        SECURITY_BIN="$fake_bin/security" \
        OPENSSL_BIN="$fake_bin/openssl" \
        FAKE_SECURITY_LOG="$root/security.log" \
        FAKE_OPENSSL_LOG="$root/openssl.log" \
        FAKE_OPENSSL_FAIL=1 \
        "$repo_dir/scripts/prepare-macos-signing.sh" >/dev/null 2>&1; then
        echo 'signing setup unexpectedly succeeded after fake openssl failure' >&2
        exit 1
    fi
    test ! -e "$root/runner/macos-distribution"
}

run_missing_input() {
    local root=$1
    mkdir -p "$root/runner"
    if env -u MACOS_SIGNING_CERTIFICATE_PASSWORD \
        RUNNER_TEMP="$root/runner" \
        MACOS_SIGNING_CERTIFICATE_BASE64=Y2VydA== \
        "$repo_dir/scripts/prepare-macos-signing.sh" >/dev/null 2>&1; then
        echo 'signing setup unexpectedly accepted a missing certificate password' >&2
        exit 1
    fi
    test ! -e "$root/runner/macos-distribution"
}

run_macos_success "$temporary/macos"
run_ios_success "$temporary/ios"
run_failure_cleanup "$temporary/failure"
run_missing_input "$temporary/missing"

run_android_cleanup() {
    local root=$1
    mkdir -p "$root/runner"
    touch "$root/runner/android-release.keystore" "$root/runner/play-service-account.json"
    env -i PATH="$PATH" RUNNER_TEMP="$root/runner" "$repo_dir/scripts/cleanup-android-release.sh"
    test ! -e "$root/runner/android-release.keystore"
    test ! -e "$root/runner/play-service-account.json"
}

run_orphaned_keychain_cleanup() {
    local root=$1
    mkdir -p "$root/runner"
    printf '%s\n' "$root/runner/macos-distribution/macos-distribution.keychain-db" \
        > "$root/runner/macos-distribution-keychain"
    : > "$root/security.log"
    env -i \
        PATH="$PATH" \
        RUNNER_TEMP="$root/runner" \
        SECURITY_BIN="$fake_bin/security" \
        FAKE_SECURITY_LOG="$root/security.log" \
        "$repo_dir/scripts/cleanup-macos-release.sh"
    test ! -e "$root/runner/macos-distribution-keychain"
    grep -F '[delete-keychain][' "$root/security.log" >/dev/null
}

run_android_cleanup "$temporary/android"
run_orphaned_keychain_cleanup "$temporary/orphaned"
echo 'signing preflight contract checks passed'
