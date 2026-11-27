# Source the same validated settings as Python, retaining explicit exports.
_secode_operator_helper=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if ! _secode_operator_exports=$(python3 "$_secode_operator_helper/operator_paths.py" --shell); then
    unset _secode_operator_helper _secode_operator_exports
    return 1
fi
eval "$_secode_operator_exports"
unset _secode_operator_helper _secode_operator_exports
