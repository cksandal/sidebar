#!/usr/bin/env bash
# Windows dev loop: cross-compile here, run and test in a Windows VM over SSH.
#
# Needs: rustup with the x86_64-pc-windows-gnu target, mingw-w64, an SSH host
# alias for the VM (OpenSSH Server, cmd.exe as its shell) and, for `shot`,
# VirtualBox. See CONTRIBUTING.md.
#
#   scripts/win.sh build            cross-compile sidebar.exe
#   scripts/win.sh deploy           build and copy it to the VM
#   scripts/win.sh run [args...]    deploy and start it on the VM's desktop
#   scripts/win.sh stop             stop it
#   scripts/win.sh log              print its output
#   scripts/win.sh test             build the test suites and run them in the VM
#   scripts/win.sh frames [args...] render headless frames in the VM, copy them back as PNG
#   scripts/win.sh shot [file.png]  screenshot of the VM screen (VirtualBox)
#   scripts/win.sh input 'CMDS'     synthetic mouse input on the VM desktop, e.g.
#                                   'drag 1560 70 800 500; click 1500 30'
#                                   (see scripts/win-input.rs for the commands)
#
# Environment: SIDEBAR_VM_HOST (default sidebar-vm), SIDEBAR_VM_NAME
# (VirtualBox name, default Windows11), PROFILE=release for release builds.

set -euo pipefail

HOST=${SIDEBAR_VM_HOST:-sidebar-vm}
VM=${SIDEBAR_VM_NAME:-Windows11}
TARGET=x86_64-pc-windows-gnu
PROFILE=${PROFILE:-debug}
REMOTE='C:\sidebar-dev'
REMOTE_SCP='C:/sidebar-dev'
TASK=sidebar-dev

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=$ROOT/target/$TARGET/$PROFILE
STAGE=$ROOT/target/win-stage
export PATH="$HOME/.cargo/bin:$PATH"

cargo_flags=(--target "$TARGET")
[[ $PROFILE == release ]] && cargo_flags+=(--release)

remote() { ssh "$HOST" "$@"; }

ensure_remote_dir() { remote "if not exist $REMOTE\\$1 mkdir $REMOTE\\$1" >/dev/null; }

# Debug builds carry ~150 MB of DWARF that is useless on Windows; strip a copy.
stage() {
    mkdir -p "$STAGE"
    local dest=$STAGE/$(basename "$1")
    x86_64-w64-mingw32-strip -o "$dest" "$1"
    echo "$dest"
}

build() { (cd "$ROOT" && cargo build "${cargo_flags[@]}" -p sidebar-app); }

deploy() {
    build
    ensure_remote_dir ""
    stop >/dev/null
    scp -q "$(stage "$OUT/sidebar.exe")" "$HOST:$REMOTE_SCP/sidebar.exe"
    echo "deployed to $HOST:$REMOTE\\sidebar.exe"
}

# Started through a scheduled task so it runs in the logged-in user's
# interactive session; programs started directly over SSH have no desktop.
# conhost --headless gives run.cmd a console that never shows on screen.
run() {
    deploy
    local cmd=$STAGE/run.cmd
    {
        printf '@echo off\r\n'
        printf 'cd /d %s\r\n' "$REMOTE"
        printf 'set RUST_BACKTRACE=1\r\n'
        printf 'set SIDEBAR_TRACE=%s\r\n' "${SIDEBAR_TRACE:-}"
        printf 'sidebar.exe %s > log.txt 2>&1\r\n' "$*"
    } >"$cmd"
    scp -q "$cmd" "$HOST:$REMOTE_SCP/run.cmd"
    remote "schtasks /create /tn $TASK /tr \"conhost.exe --headless $REMOTE\\run.cmd\" /sc once /st 00:00 /it /f >nul 2>&1 & schtasks /run /tn $TASK"
}

# Asks it to close first (so it can release its reserved screen space and
# save state), then forces it. The polite taskkill has to run on the user's
# desktop: from the SSH session it cannot reach the sidebar's windows.
stop() {
    remote 'tasklist /fi "imagename eq sidebar.exe" /fo csv /nh' | grep -q sidebar.exe || return 0
    remote "schtasks /create /tn $TASK-stop /tr \"taskkill /im sidebar.exe\" /sc once /st 00:00 /it /f >nul 2>&1 & schtasks /run /tn $TASK-stop >nul"
    for _ in 1 2 3 4 5 6; do
        sleep 0.5
        remote 'tasklist /fi "imagename eq sidebar.exe" /fo csv /nh' | grep -q sidebar.exe || { echo stopped; return 0; }
    done
    remote "taskkill /im sidebar.exe /f & exit 0"
}

log() { remote "type $REMOTE\\log.txt"; }

test_() {
    local json exes
    json=$(cd "$ROOT" && cargo test --workspace --no-run "${cargo_flags[@]}" --message-format=json)
    mapfile -t exes < <(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable != null) | .executable' <<<"$json")
    ensure_remote_dir tests
    local staged=()
    for exe in "${exes[@]}"; do staged+=("$(stage "$exe")"); done
    scp -q "${staged[@]}" "$HOST:$REMOTE_SCP/tests/"
    local failed=()
    for exe in "${staged[@]}"; do
        local name=$(basename "$exe")
        echo "== $name"
        if ! remote "cd /d $REMOTE\\tests && $name -q"; then failed+=("$name"); fi
    done
    if ((${#failed[@]})); then
        echo "FAILED in the VM: ${failed[*]}" >&2
        return 1
    fi
    echo "all ${#staged[@]} test executables passed in the VM"
}

frames() {
    deploy
    local local_dir=$ROOT/target/win-frames
    remote "(if exist $REMOTE\\frames rmdir /s /q $REMOTE\\frames) & mkdir $REMOTE\\frames"
    remote "cd /d $REMOTE && sidebar.exe --backend headless --scale 2 --ticks 12 --dump $REMOTE\\frames $*"
    rm -rf "$local_dir" && mkdir -p "$local_dir"
    scp -q "$HOST:$REMOTE_SCP/frames/*" "$local_dir/"
    for f in "$local_dir"/*.ppm; do
        pnmtopng "$f" >"${f%.ppm}.png" && rm "$f"
    done
    ls "$local_dir"/*.png
}

input() {
    mkdir -p "$STAGE"
    local exe=$STAGE/win-input.exe
    if [[ ! -e $exe || $ROOT/scripts/win-input.rs -nt $exe ]]; then
        rustc --edition 2024 -O --target "$TARGET" -C linker=x86_64-w64-mingw32-gcc -o "$exe" "$ROOT/scripts/win-input.rs"
        scp -q "$exe" "$HOST:$REMOTE_SCP/win-input.exe"
    fi
    printf '%s\r\n' "$*" >"$STAGE/input.txt"
    scp -q "$STAGE/input.txt" "$HOST:$REMOTE_SCP/input.txt"
    remote "del $REMOTE\\win-input.log 2>nul & schtasks /create /tn $TASK-input /tr $REMOTE\\win-input.exe /sc once /st 00:00 /it /rl highest /f >nul 2>&1 & schtasks /run /tn $TASK-input >nul"
    # Wait for the task to finish, then show what it logged.
    for _ in $(seq 60); do
        sleep 0.5
        if remote "type $REMOTE\\win-input.log 2>nul" | grep -q .; then
            remote "type $REMOTE\\win-input.log" | grep -v '^done' || true
            return 0
        fi
    done
    echo "input: no result after 30 s" >&2
    return 1
}

shot() {
    local file=${1:-$ROOT/target/vm-screen.png}
    VBoxManage controlvm "$VM" screenshotpng "$(realpath -m "$file")"
    echo "$file"
}

cmd=${1:-}
shift || true
case $cmd in
    build) build ;;
    deploy) deploy ;;
    run) run "$@" ;;
    stop) stop ;;
    log) log ;;
    test) test_ ;;
    frames) frames "$@" ;;
    shot) shot "$@" ;;
    input) input "$@" ;;
    *) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
