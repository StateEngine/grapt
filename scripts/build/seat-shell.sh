#!/usr/bin/env bash
# Project-scoped startup: do not replace an admitted build's target environment.
if [[ ${STATEENGINE_PROJECT_ID:-} == secode && -z ${SECODE_HEAVY_LEASE:-} &&
      -z ${SECODE_BUILD_ENV_APPLIED:-} ]] && command -v rustc >/dev/null 2>&1 &&
      git rev-parse --show-toplevel >/dev/null 2>&1; then
    secode_shell_helpers=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    if source "$secode_shell_helpers/seat-env.sh" "$PWD"; then
        export SECODE_BUILD_ENV_APPLIED=1
    fi
    unset secode_shell_helpers
fi
