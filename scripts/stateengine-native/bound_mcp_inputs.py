"""Required input preflight for the real bound-MCP journey.

SECODE_STATEENGINE_PAYLOAD must name the separately supplied bundled se payload.
Validate it against the package's existing pins before executing any producer
command. This preflight alone does not establish a passing MCP journey.
"""
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/release'))
import stateengine_payload


def required_payload(environment, repository=ROOT):
    value = environment.get('SECODE_STATEENGINE_PAYLOAD', '')
    if not value:
        raise ValueError('required bound-MCP input missing: SECODE_STATEENGINE_PAYLOAD')
    payload = Path(value)
    if not payload.is_absolute() or not payload.is_dir():
        raise ValueError('SECODE_STATEENGINE_PAYLOAD must name an absolute payload directory')
    if sys.platform not in ('linux', 'win32'):
        raise ValueError('bound-MCP payload preflight supports Linux and Windows hosts')
    target = 'x86_64-pc-windows-msvc' if sys.platform == 'win32' else 'x86_64-unknown-linux-gnu'
    pins, _raw = stateengine_payload.load(repository / 'packages/stateengine/payload-pins.json')
    pin = pins.get('targets', {}).get(target)
    if pins.get('schema') != stateengine_payload.PIN_SCHEMA or not isinstance(pin, dict):
        raise ValueError('required bound-MCP producer pin missing for ' + target)
    document, _observed = stateengine_payload.verify(payload, target, pin)
    return payload, document


def main():
    try:
        _payload, document = required_payload(os.environ)
        print(json.dumps({'status': 'producer-input-verified',
                          'version': document['version'], 'source_commit': document['source_commit'],
                          'target': document['target']}))
        return 0
    except (ValueError, OSError, stateengine_payload.PayloadError) as error:
        print('required bound-MCP producer input refused: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
