#!/usr/bin/env python3
"""Operator-only real hosted search through a supplied, signed-in service.

Use the current row A package and its running service profile. This creates one
new inspect Session; it never opens credential files or reuses a producer Session.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

PROMPT = ("Use native hosted web_search exactly once to find IANA's Example Domains "
          "documentation. Return only its title and URL. Do not use shell, web_fetch "
          "or MCP tools. If native search is unavailable, answer UNAVAILABLE.")


def observed_search(records, returncode):
    """A prose answer or offered tool is insufficient: require its settled call."""
    receipt, searches = None, {}
    for row in records:
        if row.get('schema') == 'grapt.cli.run-receipt.v1':
            receipt = row
        messages = row.get('data', [])
        if not isinstance(messages, list):
            continue
        for message in messages:
            for part in message.get('content', []):
                if part.get('type') == 'tool' and part.get('name') == 'web_search':
                    searches[part['id']] = part.get('state', {}).get('status')
    if (returncode != 0 or not receipt or receipt.get('exit') != 'success'
            or receipt.get('prompted') is not True or receipt.get('cancelled') is not None):
        raise ValueError('CLI did not finish the new search Session successfully')
    if len(searches) != 1 or list(searches.values()) != ['completed']:
        raise ValueError('exactly one completed native web_search call was not observed')
    return receipt['session']


def package_cli(package):
    manifest = package / 'release-manifest.json'
    if not manifest.is_file() or manifest.stat().st_size > 1024 * 1024:
        raise ValueError('bounded release manifest required')
    document = json.loads(manifest.read_text(encoding='utf-8'))
    if document.get('target') != 'x86_64-pc-windows-msvc':
        raise ValueError('Windows row A package required')
    for name in ('grapt.exe', 'grapt-server.exe'):
        file = package / name
        row = document['binaries'][name]
        if not file.is_file() or file.stat().st_size != row['bytes']:
            raise ValueError('packaged binary is absent or differs: ' + name)
        with file.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if digest != row['sha256']:
            raise ValueError('packaged binary differs: ' + name)
    return package / 'grapt.exe'


def run(args):
    if os.name != 'nt' or not args.service.is_absolute() or not args.service.is_dir():
        raise ValueError('native Windows and an explicit running service profile are required')
    cli = package_cli(args.package.resolve())
    scratch = Path(tempfile.mkdtemp(prefix='grapt-hosted-search-'))
    stdout, stderr = scratch / 'stdout.jsonl', scratch / 'stderr.txt'
    with stdout.open('xb') as out, stderr.open('xb') as err:
        try:
            result = subprocess.run([str(cli), 'run', '--service', str(args.service),
                '--new', '--inspect', '--jsonl', PROMPT], cwd=scratch, stdin=subprocess.DEVNULL,
                stdout=out, stderr=err, timeout=180, creationflags=subprocess.CREATE_NO_WINDOW)
        except subprocess.TimeoutExpired as error:
            raise ValueError('CLI timed out; outcome unknown; inspect ' + str(scratch)
                             + ' before retrying') from error
    # These are the CLI's existing hard output limits, not an unbounded capture.
    if stdout.stat().st_size > 16 * 1024 * 1024 or stderr.stat().st_size > 1024 * 1024:
        raise ValueError('CLI output exceeded its bound; inspect ' + str(scratch))
    records = [json.loads(line) for line in stdout.read_text(encoding='utf-8').splitlines() if line]
    try:
        return observed_search(records, result.returncode)
    except ValueError as error:
        raise ValueError(str(error) + '; inspect ' + str(scratch)) from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, required=True)
    parser.add_argument('--service', type=Path, required=True)
    args = parser.parse_args()
    try:
        run(args)
    except (OSError, ValueError, KeyError) as error:
        print('FAIL hosted web search: ' + str(error)[:1024])
        return 1
    print('PASS hosted web search')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
