#!/usr/bin/env bash
# Usage: scripts/remote-run.sh [--native] SSH-DESTINATION COMMAND...
# Run COMMAND through scripts/dev-env.sh on a Linux builder, against the files
# a commit of this worktree would contain. The builder keeps one Git worktree
# per local worktree; Cargo shares compilation outputs. Files created or
# changed come back: sources, and reports under target/, but not Cargo profiles
# or tool caches. An unreachable builder fails the command; nothing runs here.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ${1-} == --native ]]; then
    builder=$2
    shift 2
    set -- --native "$@"
else
    builder=$1
    shift
fi
# Relative to the builder's home directory.
root=.cache/agent-remote-builds
key=$(printf '%s\n' "$PWD" | git hash-object --stdin)
# Serialize the snapshot, push, execution and returned outputs together. The
# builder's lock starts after the push and ends before the output download.
exec 9> "$(git rev-parse --git-path agent-remote-run.lock)"
perl -MFcntl=:flock -e '
    if (!flock(STDIN, LOCK_EX | LOCK_NB)) {
        warn "Waiting for another remote run of this worktree\n";
        flock(STDIN, LOCK_EX) or die "Could not lock remote worktree: $!\n";
    }
' <&9
# OpenSSH appends a socket suffix; inherited Nix TMPDIR paths can be too long.
work=$(mktemp -d /tmp/agent-remote.XXXXXX)
trap 'ssh -o ControlPath="$work/control" -O exit "$builder" 2> /dev/null || true; rm -rf "$work"' EXIT

quote() {
    local argument
    for argument; do
        printf "'%s' " "${argument//\'/\'\\\'\'}"
    done
}

read -r -d '' remote_script << 'EOF' || true
set -uo pipefail
root=$1 key=$2
shift 2
command -v nix > /dev/null || . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
cd "$root" || exit 1
exec 9> "$key.lock"
# Stop when the client's heartbeats end, so a run it abandoned cannot hold or
# queue for the lock. A lost connection does not always reach this end as EOF.
exec 3<&0
while read -r -t 120 -u 3; do :; done > /dev/null 2>&1 9>&- &
client=$!
if ! flock -n 9; then
    echo 'Waiting for another remote run of this worktree' >&2
    flock 9 3<&- &
    locker=$!
    wait -n -p finished "$locker" "$client"
    if [[ $finished == "$client" ]]; then
        kill "$locker" 2> /dev/null
        exit 1
    fi
fi
rm -f "$key.tar"
if [[ -e $key/.git ]]; then
    git -C "$key" checkout --quiet --force --detach "refs/snapshots/$key" && git -C "$key" clean -fdq
else
    git -C repo.git worktree prune && git -C repo.git worktree add --quiet --detach "$PWD/$key" "refs/snapshots/$key"
fi || exit 1
touch "$key.marker"
cd "$key" || exit 1
setsid --wait scripts/dev-env.sh "$@" < /dev/null 3<&- 9>&- &
command=$!
wait -n -p finished "$command" "$client"
status=$?
if [[ $finished == "$client" ]]; then
    kill -TERM -- "-$command" 2> /dev/null
    wait "$command"
    exit 1
fi
kill "$client" 2> /dev/null
git update-index -q --refresh
{
    git ls-files -z --modified --others --exclude-standard
    find target \( -path target/tool-cache -o -path target/tmp -o -path 'target/.*' \
        -o \( -type d \( -exec test -e '{}/.fingerprint' \; -o -exec test -e '{}/.cargo-artifact-lock' \; \) \) \) -prune -o -type f -newer "../$key.marker" -print0
} | while IFS= read -r -d '' path; do
    [[ -e $path ]] && printf '%s\0' "$path"
done | tar --create --null --no-recursion --files-from=- --file="../$key.tar" || ((status)) || status=1
exit "$status"
EOF

# Snapshot without touching the worktree's index.
# Preserve the timestamp Git uses to invalidate racy stat-cache entries.
cp -p "$(git rev-parse --git-path index)" "$work/index"
GIT_INDEX_FILE=$work/index git add --all
tree=$(GIT_INDEX_FILE=$work/index git write-tree)
snapshot=$(GIT_AUTHOR_NAME='Remote build' GIT_AUTHOR_EMAIL=remote-build@localhost \
    GIT_COMMITTER_NAME='Remote build' GIT_COMMITTER_EMAIL=remote-build@localhost \
    git commit-tree -m 'Remote build snapshot' "$tree")

# One connection carries every step; it closes after a minute without use.
if ! ssh -o ControlMaster=yes -o ControlPath="$work/control" -o ControlPersist=60 -fN "$builder" \
    < /dev/null > /dev/null 2> "$work/connect.log"; then
    printf 'Remote builder %s is unreachable:\n' "$builder" >&2
    cat "$work/connect.log" >&2
    exit 1
fi
remote=(ssh -o ControlPath="$work/control" "$builder")
"${remote[@]}" "git init --quiet --bare $(quote "$root/repo.git")"
GIT_SSH_COMMAND="ssh -o ControlPath=$work/control" \
    git push --quiet --force "$builder:$root/repo.git" "$snapshot:refs/snapshots/$key"
status=0
# Heartbeats on standard input last until this script exits; the builder watches them.
"${remote[@]}" "bash -c $(quote "$remote_script") remote-run $(quote "$root" "$key" "$@")" \
    < <(
        exec < /dev/null 2> /dev/null 9>&-
        while kill -0 "$$" 2> /dev/null; do echo; sleep 5; done
    ) || status=$?
if ! "${remote[@]}" "cat $(quote "$root/$key.tar") && rm -f $(quote "$root/$key.tar")" |
    tar --extract --keep-directory-symlink --file=- --directory="$PWD"; then
    echo "Could not return the outputs from $builder" >&2
    ((status)) || status=1
fi
exit "$status"
