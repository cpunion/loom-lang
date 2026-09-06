# Source in Git Bash after entering a Visual Studio developer environment.
# Rust must find SDK import archives while building llvm-sys, before link.exe
# can use LIB. Encode each path separately so spaces remain part of one argument.
if [[ -z "${LIB:-}" ]]; then
    printf 'Windows builds need LIB from a Visual Studio developer environment.\n' >&2
    return 1
fi

loom_encoded_flags="${CARGO_ENCODED_RUSTFLAGS:-}"
if [[ -z "$loom_encoded_flags" && -n "${RUSTFLAGS:-}" ]]; then
    read -r -a loom_rust_flags <<< "$RUSTFLAGS"
    printf -v loom_encoded_flags '%s\x1f' "${loom_rust_flags[@]}"
    loom_encoded_flags="${loom_encoded_flags%$'\x1f'}"
fi
loom_required_flags=(-Ctarget-feature=+crt-static)
IFS=';' read -r -a loom_lib_dirs <<< "$LIB"
for loom_lib_dir in "${loom_lib_dirs[@]}"; do
    if [[ -n "$loom_lib_dir" ]]; then loom_required_flags+=("-Lnative=$loom_lib_dir"); fi
done
# CI and bootstrap can both source this file without changing Cargo's flags
# fingerprint and rebuilding the same dependencies twice.
for loom_flag in "${loom_required_flags[@]}"; do
    if [[ $'\x1f'"$loom_encoded_flags"$'\x1f' != *$'\x1f'"$loom_flag"$'\x1f'* ]]; then
        loom_encoded_flags+="${loom_encoded_flags:+$'\x1f'}$loom_flag"
    fi
done
export CARGO_ENCODED_RUSTFLAGS="$loom_encoded_flags"
unset loom_encoded_flags loom_rust_flags loom_required_flags loom_lib_dirs loom_lib_dir loom_flag
