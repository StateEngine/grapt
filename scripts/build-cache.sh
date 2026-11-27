#!/usr/bin/env bash
# Cargo artifacts and incremental state belong to one stable checkout.
grapt_root=$(git rev-parse --show-toplevel) || return 1
# Rename default: prefer the renamed cache, fall back to the legacy secode
# cache (the box bind-mounts it on NVMe), else create the renamed path.
cache_base=${XDG_CACHE_HOME:-$HOME/.cache}
if [[ -n ${SECODE_CACHE_DIR:-} ]]; then
    grapt_cache=$SECODE_CACHE_DIR
elif [[ -d $cache_base/grapt ]]; then
    grapt_cache=$cache_base/grapt
elif [[ -d $cache_base/secode ]]; then
    grapt_cache=$cache_base/secode
else
    grapt_cache=$cache_base/grapt
fi
# The box bind-mounts its Grapt cache on NVMe. Registry writes belong beside
# the compiler cache; explicit caller Cargo homes retain their own configuration.
export CARGO_HOME=${CARGO_HOME:-$grapt_cache/cargo-home}
export SECODE_CACHE_DIR=$grapt_cache
grapt_toolchain=$(rustc -V | tr ' /' '__') || return 1
grapt_checkout=$(printf '%s' "$grapt_root" | sha256sum | cut -d ' ' -f 1)
grapt_cached_root=$grapt_cache/target/$grapt_toolchain
export SECODE_WORKTREE_ROOT=$grapt_root
# A managed-root claim is true only when this job's outputs live under the cache.
# A caller that already chose a target elsewhere keeps it and gets no claim, so
# the cleaner is not asked to treat an outside build as its own. Scanning the
# cache while the job runs reads processes' raw layout, which the sourced script
# cannot record; keep the variable away in that case. The cache root itself keeps
# the claim so that selecting it is refused as the retired shared target, not as
# an unknown layout.
grapt_cached_resolved=$(realpath -m -- "$grapt_cached_root")
grapt_selected=
if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
    grapt_selected=$(realpath -m -- "$CARGO_TARGET_DIR")
fi
grapt_managed=
if [[ -z ${CARGO_TARGET_DIR:-} ]]; then
    grapt_managed=1
elif [[ $grapt_selected == "$grapt_cached_resolved" || $grapt_selected == "$grapt_cached_resolved"/* ]]; then
    grapt_managed=1
elif [[ ${SECODE_RAM_TARGET_RUN:-} && $grapt_selected == "$(realpath -m -- "$SECODE_RAM_TARGET_RUN")"/* ]]; then
    grapt_managed=1
fi
if [[ -n $grapt_managed ]]; then
    export SECODE_TARGET_ROOT=$grapt_cached_root
    export SECODE_WORKTREE_TARGET=$SECODE_TARGET_ROOT/worktrees/$grapt_checkout
    # Preserve persistent cross artifacts when the Linux gate borrows a RAM target.
    export SECODE_WINDOWS_TARGET_DIR=$SECODE_WORKTREE_TARGET
else
    unset SECODE_TARGET_ROOT SECODE_WORKTREE_TARGET SECODE_WINDOWS_TARGET_DIR
fi
unset grapt_cached_root grapt_cached_resolved grapt_selected grapt_managed
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$SECODE_WORKTREE_TARGET}
if [[ ! ${CARGO_BUILD_JOBS:-3} =~ ^[1-9][0-9]{0,2}$ ]]; then
    echo 'CARGO_BUILD_JOBS must be a positive bounded integer.' >&2
    return 1
fi
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
(( CARGO_BUILD_JOBS <= 3 )) || export CARGO_BUILD_JOBS=3
export CARGO_INCREMENTAL=1
# Rust dev ee6b5bb0 uses revision 9; the later Campaign Mode revision 10 is unlanded.
export SECODE_FIXTURE_COMMIT=24d77ff67b1d0ff516da3eaeace932b5e1ea665d
export SECODE_FIXTURES=${SECODE_FIXTURES:-$grapt_cache/fixtures/$SECODE_FIXTURE_COMMIT}
# Clear inherited compiler-cache wrappers for every Grapt Cargo entrypoint.
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
# Override a previous generated build.rustc-wrapper without editing active files.
export CARGO_BUILD_RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=
# One-release compatibility: in-flight older controllers still require these
# exports. Empty wrappers keep Cargo direct; no compiler-cache client is used.
export RUSTC_WRAPPER= SECODE_SCCACHE= SCCACHE_DIR= SCCACHE_CACHE_SIZE=
export SCCACHE_SERVER_PORT= SCCACHE_CLIENT_SIDE= SCCACHE_BASEDIRS=
grapt_remap="--remap-path-prefix=$HOME=/grapt"
case " ${RUSTFLAGS:-} " in
    *" $grapt_remap "*) ;;
    *) export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$grapt_remap" ;;
esac
unset grapt_remap
unset grapt_root grapt_cache cache_base grapt_toolchain grapt_checkout
