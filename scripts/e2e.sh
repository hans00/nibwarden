#!/usr/bin/env bash
# End-to-end test: nibwardend + nibwarden against a throwaway Vaultwarden in docker
# (phase1.md §11). Exits non-zero on the first failed check.
#
#   scripts/e2e.sh                      # starts vaultwarden/server:$VW_VERSION
#   NIBWARDEN_E2E_SERVER=http://127.0.0.1:8222 scripts/e2e.sh   # reuse a server
#
# The server needs open sign-ups without email verification. Local tools used: docker,
# jq, python3, script, setsid, OpenSSH (ssh, sshd, ssh-add, ssh-keygen) and git.
# scripts/e2e/fake-gui stands in for nibwarden-gui.
set -euo pipefail

cd "$(dirname "$0")/.."
VW_IMAGE=${NIBWARDEN_E2E_IMAGE:-vaultwarden/server:1.37.4}

cargo build -q --bins
BIN=$PWD/target/debug
N=$BIN/nibwarden

# Short base path: Unix socket paths are limited to ~108 bytes.
TMP=$(mktemp -d /tmp/nw-e2e.XXXXXX)
CONTAINER=
DAEMON=
SSHD=
BUS=
LOGIND=
cleanup() {
    [[ -n $SSHD ]] && kill "$SSHD" 2>/dev/null
    [[ -n $LOGIND ]] && kill "$LOGIND" 2>/dev/null
    [[ -n $BUS ]] && kill "$BUS" 2>/dev/null
    [[ -n $DAEMON ]] && kill "$DAEMON" 2>/dev/null && wait "$DAEMON" 2>/dev/null
    [[ -n $CONTAINER ]] && docker rm -f "$CONTAINER" >/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT

PASSED=0
fail() { echo "FAIL: $*" >&2; [[ -f $TMP/daemon.log ]] && tail -20 "$TMP/daemon.log" >&2; exit 1; }
ok() { PASSED=$((PASSED + 1)); echo "ok   $*"; }
expect_eq() { [[ $2 == "$3" ]] && ok "$1" || fail "$1: expected [$3], got [$2]"; }
# expect_fail <name> <exit code> <stderr substring> -- cmd...
expect_fail() {
    local name=$1 code=$2 needle=$3; shift 4
    local out rc=0
    out=$("$@" 2>&1 >/dev/null) || rc=$?
    [[ $rc == "$code" ]] || fail "$name: expected exit $code, got $rc ($out)"
    [[ $out == *"$needle"* ]] || fail "$name: expected stderr to mention [$needle], got [$out]"
    ok "$name"
}
random() { head -c 18 /dev/urandom | base64 | tr -d '/+='; }

if [[ -n ${NIBWARDEN_E2E_SERVER:-} ]]; then
    SERVER=$NIBWARDEN_E2E_SERVER
else
    CONTAINER=$(docker run -d -e SIGNUPS_ALLOWED=true -e SIGNUPS_VERIFY=false \
        -e I_REALLY_WANT_VOLATILE_STORAGE=true \
        -e DOMAIN=http://127.0.0.1 -e ROCKET_PORT=80 -p 127.0.0.1::80 "$VW_IMAGE")
    SERVER=http://$(docker port "$CONTAINER" 80/tcp | head -1)
    for _ in $(seq 50); do curl -fs "$SERVER/alive" >/dev/null && break; sleep 0.2; done
    curl -fs "$SERVER/alive" >/dev/null || fail "Vaultwarden did not come up"
fi
echo "server: $SERVER"

EMAIL=e2e-$(random | tr 'A-Z' 'a-z' | head -c 10)@example.test
MASTER=$(random)
SECRET=$(random)
API_KEY=$(random)
TOTP_SECRET=JBSWY3DPEHPK3PXP
export NIBWARDEN_PASSWORD=$MASTER
P="$BIN/nibwarden-probe --data-dir $TMP/probe"

# --- test data, created through the probe (a second device) ---
$P register --server "$SERVER" --email "$EMAIL" >/dev/null
$P login --server "$SERVER" --email "$EMAIL" >/dev/null
FOLDER=$($P add-folder --name 'Work 工作')
$P add-login --name GitHub --username alice --secret "$SECRET" --totp "$TOTP_SECRET" \
    --uri https://github.com --field "API key=$API_KEY" --folder-id "$FOLDER" >/dev/null
$P add-login --name Dup --username a --secret "dup-$(random)" >/dev/null
$P add-login --name Dup --username b --secret "dup-$(random)" >/dev/null
# Asks for the master password again before use (reprompt).
BANK=$(random)
$P add-login --name Bank --username me --secret "$BANK" --reprompt >/dev/null
# SSH keys made by OpenSSH itself, so the agent is checked against real-world key files.
mkdir -m 700 "$TMP/keys"
SSH_KEYS=(ed25519 rsa ecdsa)
ssh-keygen -q -t ed25519 -N '' -C '' -f "$TMP/keys/ed25519"
ssh-keygen -q -t rsa -b 3072 -N '' -C '' -f "$TMP/keys/rsa"
ssh-keygen -q -t ecdsa -b 521 -N '' -C '' -f "$TMP/keys/ecdsa"
# The third line of each PEM is private key material, kept for the leak check. The files
# are then deleted: ssh and ssh-keygen fall back to a private key next to the .pub, and
# every signature below must come from the agent.
PRIVATE_LINES=()
for k in "${SSH_KEYS[@]}"; do
    $P add-ssh-key --name "ssh $k" --private-key-file "$TMP/keys/$k" >/dev/null
    PRIVATE_LINES+=("$(sed -n 3p "$TMP/keys/$k")")
    rm "$TMP/keys/$k"
done
unset NIBWARDEN_PASSWORD

# --- daemon at the most verbose log level, to prove secrets never reach the log ---
export NIBWARDEN_SOCKET=$TMP/run/d.sock
AGENT=$TMP/run/ssh-agent.sock
# A fake notify-send records "locked" notices instead of showing them on the desktop.
mkdir "$TMP/bin"
printf '#!/bin/sh\nprintf "%%s|" "$@" >>"%s/notices"; echo >>"%s/notices"\n' "$TMP" "$TMP" >"$TMP/bin/notify-send"
chmod +x "$TMP/bin/notify-send"
# A private bus with a mock logind stands in for the system bus (screen lock and sleep
# events), so the test neither sees nor causes the real ones.
dbus-daemon --session --nofork --address="unix:path=$TMP/bus" &
BUS=$!
for _ in $(seq 50); do [[ -S $TMP/bus ]] && break; sleep 0.1; done
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=$TMP/bus
scripts/e2e/mock-logind &
LOGIND=$!
logind_ready() {
    gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" --dest org.freedesktop.DBus \
        --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner \
        org.freedesktop.login1 2>/dev/null | grep -q true
}
for _ in $(seq 50); do logind_ready && break; sleep 0.1; done
logind_emit() {
    gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" --dest org.freedesktop.login1 \
        --object-path /org/freedesktop/login1 --method test.Control.Emit "$1" >/dev/null
}

# Settings (config.toml) and git's files live in the test directory, not the user's.
export GIT_CONFIG_GLOBAL=$TMP/gitconfig GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME=$TMP/config

# start_daemon [gui]: without "gui" there is no display, as on a headless machine
# (phase 1 behaviour); with it, a fake GUI answers unlock windows from a script and a fixed
# display name makes the daemon believe it runs in a graphical session.
mkdir "$TMP/gui"
start_daemon() {
    local display=() gui=()
    if [[ ${1:-} == gui ]]; then
        display=(WAYLAND_DISPLAY=nw-e2e NW_FAKE_GUI_DIR=$TMP/gui)
        gui=(--gui "$PWD/scripts/e2e/fake-gui")
    fi
    env PATH="$TMP/bin:$PATH" RUST_LOG=trace WAYLAND_DISPLAY= DISPLAY= "${display[@]}" \
        "$BIN/nibwardend" --data-dir "$TMP/data" --socket "$NIBWARDEN_SOCKET" \
        "${gui[@]}" --sync-interval 0 >>"$TMP/daemon.log" 2>&1 &
    DAEMON=$!
    for _ in $(seq 50); do [[ -S $NIBWARDEN_SOCKET && -S $AGENT ]] && break; sleep 0.1; done
}
stop_daemon() {
    kill "$DAEMON"
    wait "$DAEMON" 2>/dev/null || true
    DAEMON=
}
start_daemon

# Every CLI call runs without a controlling terminal, like a script would.
nw() { setsid -w "$N" "$@" </dev/null; }

expect_eq "status logged out" "$(nw status --json | jq -r .state)" logged_out
printf '%s\n' "$MASTER" | setsid -w "$N" login --server "$SERVER" --email "$EMAIL" --password-stdin 2>/dev/null
expect_eq "login" "$(nw status --json | jq -r .state)" unlocked

# The server knows the login as nibwarden's desktop app on this device, not as the SDK's
# placeholder browser: list the account's devices with a token of our own.
ITERATIONS=$(curl -fs "$SERVER/identity/accounts/prelogin" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\"}" | jq -r '.kdfIterations // .KdfIterations')
HASH=$(python3 -I -c 'import base64, hashlib, sys
k = hashlib.pbkdf2_hmac("sha256", sys.argv[1].encode(), sys.argv[2].encode(), int(sys.argv[3]))
print(base64.b64encode(hashlib.pbkdf2_hmac("sha256", k, sys.argv[1].encode(), 1)).decode())' \
    "$MASTER" "$EMAIL" "$ITERATIONS")
TOKEN=$(curl -fs "$SERVER/identity/connect/token" --data-urlencode grant_type=password \
    --data-urlencode "username=$EMAIL" --data-urlencode "password=$HASH" \
    -d 'scope=api offline_access&client_id=web&deviceType=9&deviceName=e2e' \
    -d 'deviceIdentifier=00000000-0000-4000-8000-0000000000e2' | jq -r .access_token)
DEVICE=$(curl -fs "$SERVER/api/devices" -H "Authorization: Bearer $TOKEN" \
    | jq -c --arg id "$(cat "$TMP/data/device-id")" \
        '[.data[] | select(.identifier == $id) | {name, type}]')
expect_eq "login names the device" "$DEVICE" '[{"name":"nibwarden","type":8}]'

# --- list ---
expect_eq "list count" "$(nw list --json | jq length)" 7
expect_eq "list search" "$(nw list git --json | jq -r '.[].name')" GitHub
expect_eq "list folder" "$(nw list --folder 'work 工作' --json | jq -r '.[].name')" GitHub
expect_eq "list shows uri" "$(nw list --json | jq -r '.[] | select(.name=="GitHub") | .uris[0]')" https://github.com
[[ $(nw list --json) != *"$SECRET"* && $(nw list --json) != *"$TOTP_SECRET"* ]] \
    && ok "list carries no secrets" || fail "list leaked a secret"

# --- get / read ---
expect_eq "get password" "$(nw get GitHub)" "$SECRET"
expect_eq "get username" "$(nw get GitHub -f username)" alice
expect_eq "get custom field" "$(nw get GitHub -f 'api key')" "$API_KEY"
expect_eq "get by folder" "$(nw get GitHub --folder 'Work 工作' -f uri)" https://github.com
expect_eq "get -n has no newline" "$(nw get GitHub -n | wc -c)" "${#SECRET}"
expect_eq "read 3 segments" "$(nw read 'nw://Work 工作/GitHub/password')" "$SECRET"
expect_eq "read escaped" "$(nw read 'nw://*/GitHub/API%20key')" "$API_KEY"
expect_eq "get with ref" "$(nw get nw://GitHub/username)" alice
expect_fail "ambiguous name" 1 "2 items match" -- nw get Dup
expect_fail "unknown item" 1 "no item named" -- nw get Nope
expect_fail "unknown field" 1 "no field" -- nw get GitHub -f nope
expect_fail "bad reference" 1 "invalid reference" -- nw read nw://only

# --- totp: must equal an independent RFC 6238 computation (this or the previous step) ---
totp_at() {
    python3 -I - "$TOTP_SECRET" "$1" <<'PY'
import base64, hmac, struct, sys, time
key = base64.b32decode(sys.argv[1])
step = int(time.time()) // 30 + int(sys.argv[2])
h = hmac.new(key, struct.pack(">Q", step), "sha1").digest()
o = h[-1] & 15
print("%06d" % ((struct.unpack(">I", h[o:o + 4])[0] & 0x7FFFFFFF) % 10**6))
PY
}
CODE=$(nw totp GitHub)
[[ $CODE == "$(totp_at 0)" || $CODE == "$(totp_at -1)" ]] && ok "totp" || fail "totp: got $CODE"
expect_eq "totp field" "$(nw get GitHub -f totp | wc -c)" 7

# --- run ---
expect_eq "run inherited env" \
    "$(GH=nw://GitHub/password PLAIN=x nw run -- sh -c 'printf %s:%s "$GH" "$PLAIN"')" "$SECRET:x"
cat >"$TMP/env.tpl" <<EOF
# template
export GH_USER=nw://GitHub/username
KEY="nw://Work 工作/GitHub/API key"
EOF
expect_eq "run env file" \
    "$(nw run --env-file "$TMP/env.tpl" -- sh -c 'printf %s:%s "$GH_USER" "$KEY"')" "alice:$API_KEY"
expect_fail "run exit code" 7 "" -- nw run -- sh -c 'exit 7'
expect_fail "run bad ref" 1 "resolving \$BAD" -- env BAD=nw://Nope/password "$N" run -- true

# --- generate ---
expect_eq "generate length" "$(nw generate -l 32 | tr -d '\n' | wc -c)" 32
expect_eq "generate classes" "$(nw generate -l 40 --no-uppercase --no-numbers | tr -d 'a-z\n' | wc -c)" 0
expect_eq "passphrase words" "$(nw generate --passphrase --words 6 --separator . | tr '.' '\n' | wc -l)" 6
GENERATED=$(nw generate -l 24)

# --- ssh agent ---
keyof() { cut -d' ' -f1,2 "$@"; }   # "type base64", without the comment
expect_eq "ssh list" "$(nw ssh list --json | jq -r '.[].name' | sort | tr '\n' ,)" "ssh ecdsa,ssh ed25519,ssh rsa,"
expect_eq "status shows agent socket" "$(nw status --json | jq -r .ssh_agent_socket)" "$AGENT"
expect_eq "ssh-add -L" "$(SSH_AUTH_SOCK=$AGENT ssh-add -L | keyof | sort)" \
    "$(for k in "${SSH_KEYS[@]}"; do keyof "$TMP/keys/$k.pub"; done | sort)"
expect_eq "ssh-add -l comments" "$(SSH_AUTH_SOCK=$AGENT ssh-add -l | grep -c ' ssh ')" 3
echo "signed payload" >"$TMP/payload"
for k in "${SSH_KEYS[@]}"; do
    echo "e2e@example.test $(keyof "$TMP/keys/$k.pub")" >>"$TMP/allowed_signers"
done
# Signatures always need approval; with no window to ask in, nothing is signed until the
# master password authorizes this session (`ssh authorize`, as one would over SSH).
expect_fail "signature without approval is refused" 255 "" -- \
    env SSH_AUTH_SOCK="$AGENT" ssh-keygen -Y sign -n git -f "$TMP/keys/ed25519.pub" "$TMP/payload" </dev/null
printf '%s\n' "$MASTER" | setsid -w "$N" ssh authorize --password-stdin 2>/dev/null \
    && ok "ssh authorize" || fail "ssh authorize"
expect_fail "ssh authorize with a wrong password" 1 "wrong master password" -- \
    sh -c "echo wrong | '$N' ssh authorize --password-stdin"
rm -f "$TMP/payload.sig"
for k in "${SSH_KEYS[@]}"; do
    rm -f "$TMP/payload.sig"
    # -f with a public key makes ssh-keygen ask the agent for the private half.
    SSH_AUTH_SOCK=$AGENT ssh-keygen -q -Y sign -n git -f "$TMP/keys/$k.pub" "$TMP/payload" </dev/null 2>/dev/null \
        || fail "ssh-keygen -Y sign with $k"
    ssh-keygen -q -Y verify -f "$TMP/allowed_signers" -I e2e@example.test -n git \
        -s "$TMP/payload.sig" <"$TMP/payload" >/dev/null && ok "sshsig $k sign + verify" \
        || fail "sshsig $k verify"
done

# A real ssh login: user-mode sshd that only accepts the vault's keys.
mkdir -m 700 "$TMP/sshd"
ssh-keygen -q -t ed25519 -N '' -f "$TMP/sshd/host_key"
cat "$TMP"/keys/*.pub >"$TMP/sshd/authorized_keys"
SSHD_PORT=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
cat >"$TMP/sshd/config" <<CONF
ListenAddress 127.0.0.1:$SSHD_PORT
HostKey $TMP/sshd/host_key
AuthorizedKeysFile $TMP/sshd/authorized_keys
PidFile $TMP/sshd/pid
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
CONF
"$(command -v sshd || echo /usr/sbin/sshd)" -f "$TMP/sshd/config" -E "$TMP/sshd/log"
for _ in $(seq 50); do [[ -s $TMP/sshd/pid ]] && break; sleep 0.1; done
SSHD=$(cat "$TMP/sshd/pid")
nw_ssh() {
    SSH_AUTH_SOCK= ssh -F /dev/null -p "$SSHD_PORT" -o IdentityAgent="$AGENT" -o BatchMode=yes \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$@" \
        127.0.0.1 echo logged-in-as-\$USER </dev/null
}
for k in "${SSH_KEYS[@]}"; do
    # IdentityFile + IdentitiesOnly restricts ssh to one agent key; for RSA this exercises
    # the rsa-sha2-512 flag.
    expect_eq "ssh login with $k" \
        "$(nw_ssh -o IdentitiesOnly=yes -o IdentityFile="$TMP/keys/$k.pub" 2>&1)" "logged-in-as-$USER"
done

# --- git signing set up by `ssh git-setup` ---
# Private git config and XDG dir, so the user's own ~/.gitconfig and allowed_signers are
# neither read nor written.
git config --global user.name e2e
git config --global user.email e2e@example.test
SIGNERS=$TMP/config/git/allowed_signers
git init -q "$TMP/repo"
git init -q "$TMP/repo2"
in_repo() { local dir=$1; shift; (cd "$dir" && "$@"); }
expect_fail "git-setup asks which key" 1 "pick one with --key" -- in_repo "$TMP/repo" nw ssh git-setup
expect_fail "git-setup outside a repo" 1 "use --global" -- in_repo "$TMP" nw ssh git-setup --key 'ssh rsa'
in_repo "$TMP/repo" nw ssh git-setup --key 'SSH ED25519' 2>/dev/null || fail "git-setup"
in_repo "$TMP/repo" nw ssh git-setup --key 'ssh ed25519' 2>/dev/null || fail "git-setup again"
expect_eq "git-setup signing key" "$(git -C "$TMP/repo" config --local user.signingkey)" \
    "key::$(keyof "$TMP/keys/ed25519.pub") nibwarden:ssh ed25519"
expect_eq "git-setup signing program" "$(git -C "$TMP/repo" config --local gpg.ssh.program)" \
    "$BIN/nibwarden-ssh-sign"
expect_eq "allowed signer written once" "$(cat "$SIGNERS")" \
    "e2e@example.test namespaces=\"git\" $(keyof "$TMP/keys/ed25519.pub")"
# commit.gpgsign is on, and signing must not depend on the shell's SSH_AUTH_SOCK.
SSH_AUTH_SOCK=/nonexistent git -C "$TMP/repo" commit -q --allow-empty -m signed </dev/null \
    || fail "git commit (signed by git-setup config)"
git -C "$TMP/repo" verify-commit HEAD 2>/dev/null && ok "git commit + verify-commit" || fail "verify-commit"
expect_eq "git log sees a good signature" "$(git -C "$TMP/repo" log -1 --format=%G?)" G
# --global covers repositories without their own setting; RSA this time.
in_repo "$TMP" nw ssh git-setup --global --key 'ssh rsa' 2>/dev/null || fail "git-setup --global"
expect_eq "global allowed signers" "$(wc -l <"$SIGNERS")" 2
SSH_AUTH_SOCK= git -C "$TMP/repo2" commit -q --allow-empty -m signed </dev/null || fail "git commit (global)"
expect_eq "global setup signs with rsa" "$(git -C "$TMP/repo2" log -1 --format='%G? %GF')" \
    "G $(ssh-keygen -lf "$TMP/keys/rsa.pub" | cut -d' ' -f2)"

# --- lock on screen lock and sleep (mock logind) ---
nw_unlock() { printf '%s\n' "$MASTER" | setsid -w "$N" unlock --password-stdin 2>/dev/null; }
state_after_event() {
    logind_emit "$1"
    for _ in $(seq 20); do [[ $(nw status --json | jq -r .state) == locked ]] && break; sleep 0.1; done
    nw status --json | jq -r .state
}
expect_eq "another user's screen lock is ignored" "$(state_after_event foreign-lock)" unlocked
expect_eq "locks when the session locks" "$(state_after_event lock)" locked
nw_unlock
expect_eq "locks when the lock screen comes up" "$(state_after_event hint)" locked
nw_unlock
expect_eq "locks before sleep" "$(state_after_event sleep)" locked
nw_unlock

# --- lock ---
nw lock 2>/dev/null
# Public keys stay on offer while locked; the master password is needed when one is used.
expect_eq "locked agent still lists the public keys" "$(SSH_AUTH_SOCK=$AGENT ssh-add -L | wc -l)" 3
expect_fail "ssh-keygen sign while locked" 255 "" -- \
    env SSH_AUTH_SOCK="$AGENT" ssh-keygen -Y sign -n git -f "$TMP/keys/ed25519.pub" "$TMP/payload" </dev/null
expect_fail "ssh login while locked" 255 "Permission denied" -- \
    nw_ssh -o IdentitiesOnly=yes -o IdentityFile="$TMP/keys/ed25519.pub"
for _ in $(seq 20); do [[ -s $TMP/notices ]] && break; sleep 0.1; done
# Three locked requests within seconds: exactly one notice (rate limit).
expect_eq "one locked notice" "$(wc -l <"$TMP/notices" 2>/dev/null)" 1
[[ $(cat "$TMP/notices") == *"nibwarden is locked"*"ssh-keygen"*"nibwarden unlock"* ]] \
    && ok "notice names the requester" || fail "notice: $(cat "$TMP/notices")"
expect_fail "git commit while locked, no tty" 128 "neither a window nor a terminal" -- \
    setsid -w git -C "$TMP/repo" commit -q --allow-empty -m locked
expect_fail "get while locked, no tty" 3 "nibwarden unlock" -- nw get GitHub
expect_fail "list while locked" 3 "locked" -- nw list
GENERATED_LOCKED=$(nw generate) && ok "generate while locked" || fail "generate while locked"
# With a terminal, a locked vault prompts for the master password and retries.
OUT=$(printf '%s\n' "$MASTER" | script -qec "$N get GitHub -f username" /dev/null | tr -d '\r')
[[ $OUT == *"Master password:"*alice* ]] && ok "locked get prompts on the tty" || fail "tty unlock: [$OUT]"
expect_eq "unlocked by the prompt" "$(nw status --json | jq -r .state)" unlocked
expect_eq "agent serves keys again" "$(SSH_AUTH_SOCK=$AGENT ssh-add -L | wc -l)" 3
# git commit on a locked vault asks for the master password on the terminal, then signs.
nw lock 2>/dev/null
OUT=$(printf '%s\n' "$MASTER" | script -qec "git -C $TMP/repo commit -q --allow-empty -m after-unlock" \
    /dev/null | tr -d '\r')
[[ $OUT == *"master password to allow signing"* ]] && git -C "$TMP/repo" verify-commit HEAD 2>/dev/null \
    && ok "locked git commit prompts on the tty and signs" || fail "git tty unlock: [$OUT]"
# A reprompt item needs the master password again; without a window it cannot be asked.
expect_fail "reprompt item without a window" 1 "master password again" -- nw get Bank
# Unless the master password was just typed for this very request.
nw lock 2>/dev/null
OUT=$(printf '%s\n' "$MASTER" | script -qec "$N get Bank" /dev/null | tr -d '\r')
[[ $OUT == *"Master password:"*"$BANK"* ]] && ok "reprompt item after a terminal unlock" \
    || fail "reprompt after tty unlock: [$OUT]"

# --- unlock window (fake GUI) ---
# From here on the daemon has a (fake) desktop. It restarts locked, without having seen
# any SSH keys.
stop_daemon
start_daemon gui
expect_eq "restarted locked" "$(nw status --json | jq -r .state)" locked
gui_answers() { printf '%s\n' "$@" >"$TMP/gui/answers"; : >"$TMP/gui/log"; }
gui_log() { jq -sc "$1" "$TMP/gui/log"; }
gui_answers "password $MASTER"
expect_eq "first key listing after a restart unlocks" "$(SSH_AUTH_SOCK=$AGENT ssh-add -L | wc -l)" 3
expect_eq "... in an unlock-only window" \
    "$(gui_log '[length, .[0].kind, .[0].waiting[0].name, (.[0] | has("access"))]')" '[1,"unlock","ssh-add",false]'

nw lock 2>/dev/null
gui_answers "password wrong-$(random)" "password $MASTER"
nw unlock --gui 2>/dev/null || fail "unlock --gui"
expect_eq "unlock window unlocks" "$(nw status --json | jq -r .state)" unlocked
expect_eq "unlock window: who is waiting" \
    "$(gui_log '[.[0].kind, .[0].email, .[0].waiting[0].name, (.[0].waiting[0].pid|type)]')" \
    "[\"unlock\",\"$EMAIL\",\"nibwarden\",\"number\"]"
expect_eq "unlock window: wrong password is retried" "$(gui_log '.[1]')" '{"kind":"retry","reason":"wrong_password"}'
nw lock 2>/dev/null
gui_answers cancel
expect_fail "unlock window cancelled" 1 "cancelled" -- nw unlock --gui
expect_eq "still locked after cancel" "$(nw status --json | jq -r .state)" locked
gui_answers "password $MASTER"
nw unlock --gui 2>/dev/null && ok "unlock window again" || fail "unlock --gui after cancel"

# --- confirmation before use (fake GUI) ---
gui_answers cancel
expect_fail "read denied in the window" 1 "denied" -- nw get GitHub
gui_answers allow
expect_eq "read allowed in the window" "$(nw get GitHub)" "$SECRET"
expect_eq "window shows the request and the application" \
    "$(gui_log '[length, .[0].kind, .[0].access.requester.name, .[0].access.action.type, .[0].access.action.item, .[0].access.action.field, .[0].scope, (.[0].application.name|type)]')" \
    '[1,"approve","nibwarden","read_secret","GitHub","password","application","string"]'
# Approved for this application until the vault locks: no window the second time.
gui_answers
expect_eq "read remembered for the application" "$(nw get GitHub)" "$SECRET"
expect_eq "... no window" "$(gui_log length)" 0
gui_answers "password wrong-$(random)" "password $MASTER"
expect_eq "reprompt item takes the master password" "$(nw get Bank)" "$BANK"
expect_eq "reprompt retries a wrong password" "$(gui_log 'map(.kind)')" '["reprompt","retry"]'
git_commit() { setsid -w git -C "$TMP/repo" commit -q --allow-empty -m "$1" </dev/null && git -C "$TMP/repo" verify-commit HEAD 2>/dev/null; }
gui_answers allow
git_commit approved && ok "git signature allowed in the window" || fail "git commit after allow"
expect_eq "signature window shows git signing" \
    "$(gui_log '[.[0].kind, .[0].access.requester.name, .[0].access.action.type, .[0].access.action.purpose.kind, .[0].scope]')" \
    '["approve","ssh-keygen","ssh_sign","git","application"]'
gui_answers
git_commit remembered && ok "remembered: signs without asking" || fail "remembered signature"
expect_eq "remembered: no window" "$(gui_log length)" 0
# Locking forgets it; locked, one window unlocks and allows.
nw lock 2>/dev/null
gui_answers "password $MASTER"
git_commit locked-signature && ok "locked git signature: unlock and allow" || fail "git commit while locked"
expect_eq "... in one window" "$(gui_log '[length, .[0].kind, .[0].access.action.type, .[0].scope]')" \
    '[1,"unlock","ssh_sign","application"]'
gui_answers
git_commit remembered-again && ok "remembered from the unlock window" || fail "remembered after unlock"
nw lock 2>/dev/null
gui_answers "password $MASTER"
expect_eq "locked get (no terminal): unlock and allow" "$(nw get GitHub -f username)" alice
expect_eq "... in one window" "$(gui_log '[length, .[0].kind, .[0].access.action.field]')" '[1,"unlock","username"]'

# --- declining and closing windows ---
nw lock 2>/dev/null
gui_answers cancel
NOTICES=$(wc -l <"$TMP/notices")
expect_fail "cancelled window: no signature" 255 "" -- \
    env SSH_AUTH_SOCK="$AGENT" ssh-keygen -Y sign -n git -f "$TMP/keys/ed25519.pub" "$TMP/payload" </dev/null
expect_eq "cancelled window: no notification" "$(wc -l <"$TMP/notices")" "$NOTICES"
gui_answers
expect_fail "closed window: get fails" 1 "cancelled" -- nw get GitHub
expect_eq "still locked after declined windows" "$(nw status --json | jq -r .state)" locked
# Two programs that only need the vault unlocked share one window, which lists both.
gui_answers wait "password $MASTER"
nw list --json >"$TMP/shared-list" &
SHARED=$!
for _ in $(seq 50); do [[ -s $TMP/gui/log ]] && break; sleep 0.1; done
nw unlock --gui 2>/dev/null || fail "second request"
wait "$SHARED"
expect_eq "second request joins the open window" "$(jq length "$TMP/shared-list")" 7
expect_eq "one window lists both" "$(gui_log '[map(.kind), (.[1].waiting | map(.name))]')" \
    '[["unlock","waiting"],["nibwarden","nibwarden"]]'
rm "$TMP/gui/answers"

# --- Bitwarden browser extension (phase2.md §5.8) ---
# Registration, in a home of its own: two browsers, one of them set up for Bitwarden's app.
BH=$TMP/home
mkdir -p "$BH/.mozilla" "$BH/.config/chromium" "$BH/.config/google-chrome/NativeMessagingHosts"
OFFICIAL='{"name": "com.8bit.bitwarden", "path": "/opt/Bitwarden/desktop_proxy"}'
printf '%s\n' "$OFFICIAL" >"$BH/.config/google-chrome/NativeMessagingHosts/com.8bit.bitwarden.json"
expect_eq "browser enable" "$(HOME=$BH "$N" browser enable 2>/dev/null | sed 's/  */ /g' | paste -sd '|')" \
    "Firefox nibwarden|Google Chrome another program: /opt/Bitwarden/desktop_proxy|Chromium nibwarden"
expect_eq "... Firefox manifest" \
    "$(jq -c '[.name, .path, .allowed_extensions]' "$BH/.mozilla/native-messaging-hosts/com.8bit.bitwarden.json")" \
    "[\"com.8bit.bitwarden\",\"$BIN/nibwarden-browser-host\",[\"{446900e4-71c2-419f-a6a7-df9c091e268b}\"]]"
expect_eq "... Chromium manifest" \
    "$(jq -r '.allowed_origins[0]' "$BH/.config/chromium/NativeMessagingHosts/com.8bit.bitwarden.json")" \
    "chrome-extension://nngceckbapebfimnlniiiahkandclblb/"
expect_eq "... Bitwarden's own registration left alone" \
    "$(cat "$BH/.config/google-chrome/NativeMessagingHosts/com.8bit.bitwarden.json")" "$OFFICIAL"
HOME=$BH "$N" browser disable >/dev/null 2>&1
[[ ! -e $BH/.mozilla/native-messaging-hosts/com.8bit.bitwarden.json && \
    -e $BH/.config/google-chrome/NativeMessagingHosts/com.8bit.bitwarden.json ]] \
    && ok "browser disable removes only nibwarden's" || fail "browser disable"
# The protocol, with nibwarden-probe as the extension.
extension() { NIBWARDEN_PASSWORD=$MASTER $P browser-extension --host "$BIN/nibwarden-browser-host" "$@" 2>&1 | tr '\n' ' '; }
gui_answers allow
expect_eq "extension unlocked after approval" "$(extension)" \
    "connected setup: ok status: 0 unlock: true key-matches "
expect_eq "... asked in a window, every time" "$(gui_log '[length, .[0].kind, .[0].access.action, .[0].scope]')" \
    '[1,"approve",{"type":"browser","what":"unlock"},"request"]'
gui_answers cancel
expect_eq "extension refused" "$(extension)" "connected setup: ok status: 0 unlock: false "
nw lock 2>/dev/null
gui_answers "password $MASTER"
expect_eq "locked: unlocking in the window unlocks the extension too" "$(extension)" \
    "connected setup: ok status: 0 unlock: true key-matches "
expect_eq "... in one unlock window" "$(gui_log '[length, .[0].kind, .[0].access.action.what]')" '[1,"unlock","unlock"]'
gui_answers
expect_eq "extension of another account" "$(extension --user-id 00000000-0000-4000-8000-000000000001)" \
    "connected setup: wrongUserId "
expect_eq "... no window" "$(gui_log length)" 0
rm "$TMP/gui/answers"

# --- changing items (P2-M10): any program but nibwarden's own window is asked every time ---
I=scripts/e2e/ipc
item() { nw list --json | jq -c ".[] | select(.name == \"$1\") | $2"; }
MADE_PW=made-$(random)
gui_answers cancel
expect_fail "creating an item is confirmed (declined)" 1 "denied" -- \
    "$I" item.save '{"item": {"kind": "login", "name": "Made"}}'
expect_eq "... in a window that says what changes" "$(gui_log '.[0].access.action')" \
    '{"type":"change_item","item":"Made","change":"create"}'
gui_answers allow
MADE=$("$I" item.save "$(jq -nc --arg pw "$MADE_PW" '{item: {kind: "login", name: "Made",
    values: [["username", "maker"], ["password", $pw]], uris: ["https://made.test"],
    fields: [{name: "pin", value: "1234", kind: "hidden"}]}}')" | jq -r .id)
expect_eq "create a login" "$(item Made .subtitle)" '"maker"'
gui_answers allow allow
CONTENT=$("$I" item.content "{\"id\": \"$MADE\"}")
expect_eq "item.content has the secrets" "$(jq -r '.values | map(select(.[0] == "password"))[0][1]' <<<"$CONTENT")" "$MADE_PW"
"$I" item.save "$(jq -c '{item: (.name = "Made 2" | .values = [["username", "maker2"]])}' <<<"$CONTENT")" >/dev/null \
    || fail "edit"
expect_eq "edit renames and changes the username" "$(item 'Made 2' .subtitle)" '"maker2"'
gui_answers allow
CONTENT=$("$I" item.content "{\"id\": \"$MADE\"}")
expect_eq "... and keeps what the edit did not cover" \
    "$(jq -c '[.uris, (.fields | map(.name)), (.values | map(.[0]))]' <<<"$CONTENT")" \
    '[["https://made.test"],["pin"],["username"]]'
VIEW=$("$I" item.view "{\"item\": \"$MADE\"}")
expect_eq "item history: dates, and the replaced password's" \
    "$(jq -c '[(.created | length > 0), (.revised >= .created), (.password_history | length)]' <<<"$VIEW")" \
    '[true,true,1]'
gui_answers allow
expect_eq "... which reads back as the old password" \
    "$("$I" item.get "{\"item\": \"$MADE\", \"field\": \"password_history_0\"}" | jq -r .value)" "$MADE_PW"
gui_answers allow
"$I" item.favorite "{\"id\": \"$MADE\", \"favorite\": true}" >/dev/null || fail "favorite"
expect_eq "favorite" "$(item 'Made 2' .favorite)" true
gui_answers allow
expect_fail "a live item cannot be deleted for good" 1 "not in the trash" -- \
    "$I" item.delete "{\"id\": \"$MADE\"}"
gui_answers allow
"$I" item.trash "{\"id\": \"$MADE\"}" >/dev/null || fail "trash"
expect_eq "trash: gone from the list" "$(item 'Made 2' .name)" ""
expect_eq "... and in the trash" \
    "$("$I" item.list '{"trash": true}' | jq -r '.items[] | select(.id == "'"$MADE"'") | .trashed')" true
gui_answers allow
"$I" item.restore "{\"id\": \"$MADE\"}" >/dev/null || fail "restore"
expect_eq "restore" "$(item 'Made 2' .name)" '"Made 2"'
gui_answers allow allow
"$I" item.trash "{\"id\": \"$MADE\"}" >/dev/null && "$I" item.delete "{\"id\": \"$MADE\"}" >/dev/null \
    || fail "delete"
expect_eq "delete for good" "$("$I" item.list '{"trash": true}' | jq "[.items[] | select(.id == \"$MADE\")] | length")" 0
gui_answers allow
"$I" item.save '{"item": {"kind": "ssh_key", "name": "Made key"}}' >/dev/null || fail "new SSH key"
expect_eq "a new SSH key item gets a generated key" \
    "$(nw ssh list --json | jq -r '.[] | select(.name == "Made key") | .public_key' | cut -d' ' -f1)" ssh-ed25519
gui_answers allow
CARD=$("$I" item.save '{"item": {"kind": "card", "name": "Made card", "values": [["cardholder_name", "A B"], ["number", "4111111111111111"]]}}' | jq -r .id)
expect_eq "card details: plain values shown, secret ones only named" \
    "$("$I" item.view "{\"item\": \"$CARD\"}" | jq -c .values)" '[["cardholder_name","A B"],["number",null]]'
rm "$TMP/gui/answers"

# --- Sends (P2-M12): every create, read and delete by a program is asked about ---
SEND_TEXT=send-$(random)
send_cli() { setsid -w "$N" send "$@"; }
gui_answers cancel
expect_fail "creating a Send is confirmed (declined)" 1 "denied" -- \
    bash -c 'printf %s "$1" | setsid -w "$2" send create Note' _ "$SEND_TEXT" "$N"
expect_eq "... in a window that names the Send" "$(gui_log '.[0].access.action')" \
    '{"type":"send","send":"Note","what":"create"}'
gui_answers allow allow
LINK=$(printf '%s\n' "$SEND_TEXT" | send_cli create Note --days 2 --hidden 2>/dev/null) || fail "send create"
[[ $LINK =~ ^$SERVER/#/send/[A-Za-z0-9_-]+/[A-Za-z0-9_-]+$ ]] && ok "create a text Send: prints its link" \
    || fail "unexpected Send link [$LINK]"
SEND_LIST=$(send_cli list --json </dev/null)
expect_eq "send list: name, kind, deletion in two days, nothing secret" \
    "$(jq -c --arg t "$SEND_TEXT" '[.[0].name, .[0].file_name, (.[0].deletion_date[:10]), (tostring | contains($t))]' <<<"$SEND_LIST")" \
    "[\"Note\",null,\"$(date -u -d '+2 days' +%F)\",false]"
expect_eq "the link opens for a recipient, with the text" \
    "$($P open-send "$LINK" | jq -c '[.name, .text]')" "[\"Note\",\"$SEND_TEXT\"]"
gui_answers allow
expect_eq "send get --text" "$(send_cli get note --text </dev/null 2>/dev/null)" "$SEND_TEXT"
head -c 300000 /dev/urandom >"$TMP/send.bin"
gui_answers allow allow
FILE_LINK=$(send_cli create Blob --file "$TMP/send.bin" </dev/null 2>/dev/null) || fail "file send"
expect_eq "create a file Send" "$(send_cli list --json </dev/null | jq -r '.[] | select(.name == "Blob") | .file_name')" send.bin
$P open-send "$FILE_LINK" --out "$TMP/received.bin" >/dev/null || fail "download the file Send"
cmp -s "$TMP/send.bin" "$TMP/received.bin" && ok "... which a recipient downloads intact" || fail "file Send differs"
head -c $((10 * 1024 * 1024 + 1)) /dev/zero >"$TMP/big.bin"
expect_fail "a file over 10 MiB is refused" 1 "larger than 10 MiB" -- send_cli create Big --file "$TMP/big.bin"
gui_answers allow
"$I" send.create '{"send": {"name": "Locked note", "body": {"type": "text", "text": "x"}, "deletion_days": 1,
    "password": "open sesame", "max_access_count": 3}}' >/dev/null || fail "send with a password"
expect_eq "a Send with a password and an access limit" \
    "$(send_cli list --json </dev/null | jq -c '.[] | select(.name == "Locked note") | [.has_password, .max_access_count]')" '[true,3]'
gui_answers cancel
expect_fail "deleting a Send is confirmed (declined)" 1 "denied" -- send_cli delete Note
gui_answers allow
send_cli delete Note </dev/null 2>/dev/null || fail "send delete"
expect_eq "delete a Send" "$(send_cli list --json </dev/null | jq -c 'map(.name) | sort')" '["Blob","Locked note"]'
expect_fail "... and its link stops working" 1 "send access token" -- $P open-send "$LINK"
rm "$TMP/gui/answers"

# --- export and import (P2-M13): the master password again, then a confirmation ---
# with_stdin TEXT CMD...: run nibwarden with TEXT (printf escapes) on stdin.
with_stdin() { local text=$1; shift; printf "$text" | setsid -w "$N" "$@"; }
BEFORE=$(nw list --json | jq length)
FOLDERS=$("$I" folder.list | jq '.folders | length')
expect_fail "export asks for the master password again" 1 "wrong master password" -- \
    with_stdin 'wrong\n' export --format json -o "$TMP/export.json" --password-stdin
[[ ! -e $TMP/export.json ]] && ok "... and writes nothing without it" || fail "export written after a wrong password"
gui_answers cancel
expect_fail "exporting is confirmed (declined)" 1 "denied" -- \
    with_stdin "$MASTER\n" export --format json -o "$TMP/export.json" --password-stdin
expect_eq "... in a window that names the format" "$(gui_log '.[0].access.action')" '{"type":"export","format":"json"}'
gui_answers allow
with_stdin "$MASTER\n" export --format json -o "$TMP/export.json" --password-stdin 2>/dev/null || fail "export json"
expect_eq "export JSON: every item, in a file only you can read" \
    "$(jq '.items | length' "$TMP/export.json") $(stat -c %a "$TMP/export.json")" "$BEFORE 600"
expect_fail "an existing file is not replaced" 1 "already exists" -- \
    with_stdin "$MASTER\n" export --format json -o "$TMP/export.json" --password-stdin
gui_answers allow
with_stdin "$MASTER\n" export --format csv -o "$TMP/export.csv" --password-stdin 2>/dev/null || fail "export csv"
expect_eq "export CSV: Bitwarden's columns" "$(head -1 "$TMP/export.csv")" \
    folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp
FILE_PW=file-$(random)
gui_answers allow
with_stdin "$MASTER\n$FILE_PW\n" export --format encrypted-json -o "$TMP/export.enc.json" --password-stdin 2>/dev/null \
    || fail "export encrypted json"
expect_eq "export password-protected JSON: nothing readable" \
    "$(jq -c '[.encrypted, .passwordProtected]' "$TMP/export.enc.json") $(grep -cF -- "$SECRET" "$TMP/export.enc.json")" \
    '[true,true] 0'
name_count() { nw list --json | jq "[.[] | select(.name == \"$1\")] | length"; }
gui_answers cancel
expect_fail "importing is confirmed (declined)" 1 "denied" -- nw import "$TMP/export.json"
expect_eq "... in a window that names the file and counts its items" "$(gui_log '.[0].access.action')" \
    "{\"type\":\"import\",\"source\":\"export.json\",\"items\":$BEFORE}"
gui_answers allow
nw import "$TMP/export.json" 2>/dev/null || fail "import json"
expect_eq "import JSON: every item again, into the existing folders" \
    "$(nw list --json | jq length) $(name_count GitHub) $("$I" folder.list | jq '.folders | length')" \
    "$((BEFORE * 2)) 2 $FOLDERS"
mapfile -t COPIES < <(nw list --json | jq -r '.[] | select(.name == "GitHub") | .id')
gui_answers allow allow
expect_eq "... with the same contents" \
    "$("$I" item.content "{\"id\": \"${COPIES[0]}\"}" | jq -c '[.values, .uris, .fields, .folder_id]')" \
    "$("$I" item.content "{\"id\": \"${COPIES[1]}\"}" | jq -c '[.values, .uris, .fields, .folder_id]')"
expect_fail "a password-protected export needs its password" 1 "wrong file password" -- \
    with_stdin 'nope\n' import "$TMP/export.enc.json" --password-stdin
gui_answers allow
with_stdin "$FILE_PW\n" import "$TMP/export.enc.json" --password-stdin 2>/dev/null || fail "import encrypted json"
expect_eq "import password-protected JSON" "$(name_count GitHub)" 3
CSV_ITEMS=$(jq '[.items[] | select(.type <= 2)] | length' "$TMP/export.json")
gui_answers allow
nw import "$TMP/export.csv" 2>/dev/null || fail "import csv"
expect_eq "import CSV: its logins and notes" "$(nw list --json | jq length)" "$((BEFORE * 3 + CSV_ITEMS))"
KDBX_PW=kdbx-$(random)
KEEPASS_SECRET=keepass-$(random)
$P make-kdbx --out "$TMP/vault.kdbx" --password "$KDBX_PW" --title "From KeePass" --secret "$KEEPASS_SECRET"
gui_answers allow
expect_fail "a KeePass database needs its password" 1 "wrong KeePass password" -- \
    with_stdin 'nope\n' import "$TMP/vault.kdbx" --password-stdin
gui_answers allow
with_stdin "$KDBX_PW\n" import "$TMP/vault.kdbx" --password-stdin 2>/dev/null || fail "import kdbx"
expect_eq "import a KeePass database" \
    "$(nw list --json | jq -c '.[] | select(.name == "From KeePass") | [.subtitle, .folder]')" '["keeper","KeePass"]'
rm "$TMP/gui/answers"

# --- live sync: another device's change arrives without `sync` (periodic sync is off) ---
NIBWARDEN_PASSWORD=$MASTER $P add-login --name Pushed --username push --secret "push-$(random)" >/dev/null
for _ in $(seq 150); do
    nw list --json 2>/dev/null | jq -e 'any(.name == "Pushed")' >/dev/null && break
    sleep 0.1
done
expect_eq "change on another device arrives by itself" \
    "$(nw list --json | jq -r '.[] | select(.name == "Pushed") | .subtitle')" push
grep -q "synced after a change on another device" "$TMP/daemon.log" \
    && ok "... through the notifications hub" || fail "no notification-driven sync in the log"

# --- nothing secret on disk, in the daemon's argv/env, or in its trace log ---
DPID=$DAEMON
for secret in "$MASTER" "$SECRET" "$API_KEY" "$TOTP_SECRET" "$GENERATED" "$GENERATED_LOCKED" "$BANK" "$MADE_PW" "$SEND_TEXT" "$FILE_PW" "$KDBX_PW" "$KEEPASS_SECRET" \
    "${PRIVATE_LINES[@]}"; do
    ! grep -rqF -- "$secret" "$TMP/data" || fail "plaintext secret in the data directory"
    ! grep -qF -- "$secret" "$TMP/daemon.log" || fail "secret in the daemon log"
    ! tr '\0' '\n' </proc/$DPID/cmdline | grep -qF -- "$secret" || fail "secret in daemon argv"
done
ok "no secrets on disk, in the trace log, or in daemon argv"
[[ $(wc -l <"$TMP/daemon.log") -gt 100 ]] && ok "trace log is actually verbose" || fail "trace log suspiciously short"

nw logout 2>/dev/null
expect_eq "logout" "$(nw status --json | jq -r .state)" logged_out

# --- a new account from the main window's sign-up (phase2.md §5.7), over the same requests ---
I=scripts/e2e/ipc
NEW=new-$(random | tr 'A-Z' 'a-z' | head -c 10)
NEW_PW=$(random)
# Mixed case: the window salts the master key with the address as the official clients spell it.
TOKEN=$("$I" auth.register_start "$(jq -nc --arg s "$SERVER" --arg e "$NEW@Example.TEST" \
    '{server: $s, email: $e, name: "New"}')" | jq -r .token)
[[ -n $TOKEN && $TOKEN != null ]] && ok "sign-up without address verification hands back the token" \
    || fail "no sign-up token: $TOKEN"
"$I" auth.register_finish "$(jq -nc --arg s "$SERVER" --arg e "$NEW@Example.TEST" --arg t "$TOKEN" \
    --arg p "$NEW_PW" '{server: $s, email: $e, token: $t, password: $p, hint: "a hint"}')" >/dev/null \
    || fail "register_finish"
printf '%s\n' "$NEW_PW" | setsid -w "$N" login --server "$SERVER" --email "$NEW@example.test" --password-stdin 2>/dev/null
expect_eq "the new account logs in, spelled in lower case" "$(nw status --json | jq -r .email)" "$NEW@example.test"
expect_eq "... with an empty vault" "$(nw list --json | jq length)" 0
nw logout 2>/dev/null
# The first step does not tell whether an address has an account; finishing does.
TOKEN=$("$I" auth.register_start "$(jq -nc --arg s "$SERVER" --arg e "$NEW@example.test" \
    '{server: $s, email: $e}')" | jq -r .token)
expect_fail "signing up again with the same address" 1 "exists" -- \
    "$I" auth.register_finish "$(jq -nc --arg s "$SERVER" --arg e "$NEW@example.test" --arg t "$TOKEN" \
    --arg p "$NEW_PW" '{server: $s, email: $e, token: $t, password: $p}')"
! grep -qF -- "$NEW_PW" "$TMP/daemon.log" && ok "the new master password is not in the trace log" \
    || fail "new master password in the daemon log"
echo "all $PASSED checks passed"
