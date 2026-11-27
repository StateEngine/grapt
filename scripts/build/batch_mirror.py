"""Fast-forward the configured clean Windows mirror after canonical publication."""
import base64
import re

import batch_compose as compose


def mirror(root, host, path, head):
    if not host or host.startswith('-') or not re.fullmatch(r'[A-Za-z0-9_.-]+', host):
        raise ValueError('Configure a Windows SSH alias with --mirror-host')
    if not path or not re.match(r'^[A-Za-z]:[/\\]', path) or '\n' in path or '\r' in path:
        raise ValueError('Configure an absolute Windows repository path with --mirror-path')
    quote = lambda value: "'" + value.replace("'", "''") + "'"
    # Fetch and merge only: the live mirror is never reset or switched.
    script = f"""$ErrorActionPreference='Stop'
function G {{ $result = & git -C {quote(path)} @args; if ($LASTEXITCODE -ne 0) {{ throw 'Mirror Git command failed' }}; return $result }}
if ((G branch --show-current) -ne 'main') {{ throw 'Mirror must already be on main' }}
if (G status --porcelain) {{ throw 'Preserve dirty mirror; commit or move its work first' }}
G fetch --no-tags github main
$target = G rev-parse FETCH_HEAD
G merge-base --is-ancestor {quote(head)} $target
G merge-base --is-ancestor HEAD $target
G merge --ff-only $target
if ((G rev-parse HEAD) -ne $target) {{ throw 'Mirror fast-forward was not observed' }}
Write-Output ('MIRRORED ' + $target)
"""
    encoded = base64.b64encode(script.encode('utf-16le')).decode('ascii')
    result = compose.command(root, 'ssh', host, 'powershell.exe', '-NoProfile',
                             '-NonInteractive', '-EncodedCommand', encoded)
    if not re.search(r'^MIRRORED [0-9a-f]{40}\s*$', result.stdout, re.M):
        raise ValueError('Mirror update is unconfirmed; rerun --resume-mirror after inspection')
