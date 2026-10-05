#!/usr/bin/env bash
set -euo pipefail

# A preceding Loom compiler builds current Loom. Fresh checkouts compile the
# source-bound, portable checked stage 0 with the current native bridge.
development=false
if [[ $# == 1 && "$1" == --dev ]]; then
    development=true
elif [[ $# != 0 ]]; then
    printf 'Usage: bash scripts/bootstrap.sh [--dev]\nSet LOOM_BOOTSTRAP_COMPILER to use an installed Loom seed.\n' >&2
    exit 2
fi

exe_suffix=""
runtime_name="libloom_runtime.a"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) exe_suffix=".exe"; runtime_name="loom_runtime.lib" ;;
esac
seed_compiler=""
if [[ -n "${LOOM_BOOTSTRAP_COMPILER:-}" ]]; then
    requested_compiler="$LOOM_BOOTSTRAP_COMPILER"
    if [[ "$exe_suffix" == .exe ]]; then requested_compiler="$(cygpath -u "$requested_compiler")"; fi
    seed_compiler="$(command -v "$requested_compiler")" || {
        printf 'Cannot find LOOM_BOOTSTRAP_COMPILER: %s\n' "$LOOM_BOOTSTRAP_COMPILER" >&2
        exit 1
    }
    if [[ "$seed_compiler" != /* ]]; then
        seed_compiler="$PWD/$seed_compiler"
    fi
    if [[ ! -x "$seed_compiler" ]]; then
        printf 'Bootstrap compiler is not executable: %s\n' "$seed_compiler" >&2
        exit 1
    fi
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"
target_root="$repo_root/target"
if $development && [[ -z "$seed_compiler" && -z "${LOOM_BOOTSTRAP_INPUT:-}" && -x "$target_root/loom$exe_suffix" ]]; then
    seed_compiler="$target_root/loom$exe_suffix"
fi
if $development; then export LOOM_OPT_LEVEL="${LOOM_OPT_LEVEL:-1}"; fi

if [[ -n "$seed_compiler" && -n "${LOOM_BOOTSTRAP_INPUT:-}" ]]; then
    printf 'Choose LOOM_BOOTSTRAP_COMPILER or LOOM_BOOTSTRAP_INPUT, not both.\n' >&2
    exit 2
fi
printf 'Building the native LLVM bridge and runtime...\n'
if [[ "$exe_suffix" == .exe ]]; then source "$repo_root/scripts/windows-env.sh"; fi
cargo build --locked --workspace --target-dir "$target_root"

export LOOM_RUNTIME_LIBRARY="$target_root/debug/$runtime_name"
if [[ "$exe_suffix" == .exe ]]; then
    # Environment variables are not subject to Git Bash's argv path conversion.
    LOOM_RUNTIME_LIBRARY="$(cygpath -m "$LOOM_RUNTIME_LIBRARY")"
fi
if [[ -z "$seed_compiler" && -z "${LOOM_BOOTSTRAP_INPUT:-}" ]]; then
    # The checksum detects checkout damage. Unix CI independently reproduces
    # every checked byte from the immutable source pin, using the Loom checker.
    checked_archive="$repo_root/compiler/bootstrap/stage0.checked.gz"
    if command -v sha256sum >/dev/null; then
        (cd "$repo_root/compiler/bootstrap" && sha256sum -c stage0.checked.gz.sha256)
    else
        (cd "$repo_root/compiler/bootstrap" && shasum -a 256 -c stage0.checked.gz.sha256)
    fi
    mkdir -p "$target_root/bootstrap/stage0"
    checked_input="$target_root/bootstrap/stage0/compiler.checked"
    checked_temporary="$(mktemp "$checked_input.XXXXXX")"
    if ! gzip -dc "$checked_archive" > "$checked_temporary"; then
        rm -f "$checked_temporary"
        printf 'Cannot decompress the pinned stage 0 input.\n' >&2
        exit 1
    fi
    mv -f "$checked_temporary" "$checked_input"
    LOOM_BOOTSTRAP_INPUT="$checked_input"
    if [[ "$exe_suffix" == .exe ]]; then
        LOOM_BOOTSTRAP_INPUT="$(cygpath -m "$checked_input")"
    fi
fi
if [[ -n "${LOOM_BOOTSTRAP_INPUT:-}" ]]; then
    seed_compiler="$target_root/loom-stage0$exe_suffix"
    printf 'Building the initial native compiler from checked input...\n'
    "$target_root/debug/loom-native$exe_suffix" "$LOOM_BOOTSTRAP_INPUT" --output "$seed_compiler"
fi

# Every current stage uses one current native backend/runtime. Only the source
# compiler changes between stages; no Rust parser or checker is active here.
printf 'Using Loom seed: %s\n' "$seed_compiler"
previous="$seed_compiler"
stages=(1 2 3)
if $development; then stages=(1); fi
for stage in "${stages[@]}"; do
    candidate="$target_root/loom-stage$stage$exe_suffix"
    printf 'Building Loom stage %s...\n' "$stage"
    "$previous" build "$repo_root/compiler/loom" \
        --std "$repo_root/compiler/std" \
        --native-tool "$target_root/debug/loom-native$exe_suffix" \
        --output "$candidate"
    previous="$candidate"
done

if ! $development && ! cmp -s "$target_root/loom-stage2$exe_suffix" "$target_root/loom-stage3$exe_suffix"; then
    printf 'Bootstrap failed: Loom stages 2 and 3 differ.\n' >&2
    exit 1
fi
if [[ -d "$target_root/loom$exe_suffix" ]]; then
    printf 'Cannot publish the compiler over a directory: %s\n' "$target_root/loom$exe_suffix" >&2
    exit 1
fi
publication="$(mktemp "$target_root/loom.XXXXXX")"
trap 'if [[ -n "${publication:-}" ]]; then rm -f "$publication"; fi' EXIT
cp "$previous" "$publication"
chmod 755 "$publication"
mv -f "$publication" "$target_root/loom$exe_suffix"
publication=""
if $development; then
    printf 'Development compiler built (one stage; not bootstrap-verified): %s\n' "$target_root/loom$exe_suffix"
else
    printf 'Bootstrap verified: %s\n' "$target_root/loom$exe_suffix"
fi
