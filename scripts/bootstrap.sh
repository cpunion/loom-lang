#!/usr/bin/env bash
set -euo pipefail

# A preceding Loom compiler builds current Loom. The frozen Rust compiler is
# recovered only when no installed Loom seed is supplied. Advance the pin only
# after a verified bootstrap; current source must remain accepted by its seed.
if [[ $# != 0 ]]; then
    printf 'Usage: bash scripts/bootstrap.sh\nSet LOOM_BOOTSTRAP_COMPILER to use an installed Loom seed.\n' >&2
    exit 2
fi

seed_compiler=""
if [[ -n "${LOOM_BOOTSTRAP_COMPILER:-}" ]]; then
    seed_compiler="$(command -v "$LOOM_BOOTSTRAP_COMPILER")" || {
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

printf 'Building the native LLVM bridge and runtime...\n'
cargo build --locked --workspace --target-dir "$target_root"

if [[ -z "$seed_compiler" ]]; then
    IFS= read -r seed_pin < compiler/bootstrap/seed
    bootstrap_cache="$target_root/bootstrap/$seed_pin"
    seed_source="$bootstrap_cache/source"
    seed_compiler="$bootstrap_cache/loom-stage0"
    if [[ ! -x "$seed_compiler" ]]; then
        if ! git cat-file -e "$seed_pin^{commit}" 2>/dev/null; then
            printf 'Missing pinned bootstrap commit %s.\nFetch it with: git fetch origin %s\n' "$seed_pin" "$seed_pin" >&2
            exit 1
        fi
        printf 'Recovering the frozen compiler from %s...\n' "$seed_pin"
        mkdir -p "$seed_source"
        git archive "$seed_pin" Cargo.toml Cargo.lock compiler rustfmt.toml |
            tar -xf - -C "$seed_source"
        cargo build --locked --workspace --manifest-path "$seed_source/Cargo.toml" \
            --target-dir "$bootstrap_cache/target"
        # The historical Rust compiler builds only the historical Loom seed,
        # never current source. Its runtime and std stay inside this cache.
        LOOM_STD="$seed_source/compiler/std" \
        LOOM_RUNTIME_LIBRARY="$bootstrap_cache/target/debug/libloom_seed_runtime.a" \
            "$bootstrap_cache/target/debug/loom" build "$seed_source/compiler/loom" \
            --output "$seed_compiler"
    fi
fi

# Every current stage uses one current native backend/runtime. Only the source
# compiler changes between stages; no Rust parser or checker is active here.
printf 'Using Loom seed: %s\n' "$seed_compiler"
export LOOM_RUNTIME_LIBRARY="$target_root/debug/libloom_runtime.a"
previous="$seed_compiler"
for stage in 1 2 3; do
    candidate="$target_root/loom-stage$stage"
    printf 'Building Loom stage %s...\n' "$stage"
    "$previous" build "$repo_root/compiler/loom" \
        --std "$repo_root/compiler/std" \
        --native-tool "$target_root/debug/loom-native" \
        --output "$candidate"
    previous="$candidate"
done

if ! cmp -s "$target_root/loom-stage2" "$target_root/loom-stage3"; then
    printf 'Bootstrap failed: Loom stages 2 and 3 differ.\n' >&2
    exit 1
fi
if [[ -d "$target_root/loom" ]]; then
    printf 'Cannot publish the compiler over a directory: %s\n' "$target_root/loom" >&2
    exit 1
fi
publication="$(mktemp "$target_root/loom.XXXXXX")"
trap 'if [[ -n "${publication:-}" ]]; then rm -f "$publication"; fi' EXIT
cp "$target_root/loom-stage3" "$publication"
chmod 755 "$publication"
mv -f "$publication" "$target_root/loom"
publication=""
printf 'Bootstrap verified: %s\n' "$target_root/loom"
