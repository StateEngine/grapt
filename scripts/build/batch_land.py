#!/usr/bin/env python3
"""Compose reviewed pull requests in a detached, resumable landing controller."""
import argparse
from dataclasses import asdict
import os
from pathlib import Path
import re
import subprocess
import sys

import batch_compose as compose
import batch_github as github
from batch_mirror import mirror
import batch_notify
from batch_pipeline import Pipeline, StageFailure
from batch_staging import qualify
from batch_state import load, lock, save
import durable_jobs
import heavy
import operator_paths


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('name')
    parser.add_argument('--pr', type=int, action='append', default=[])
    parser.add_argument('--project', required=True, help='GitHub owner/repository')
    parser.add_argument('--repository', type=Path, required=True)
    parser.add_argument('--worktrees', type=Path, default=operator_paths.worktrees())
    parser.add_argument('--state', type=Path, required=True, help='External owned checkpoint directory')
    parser.add_argument('--mirror-host', default=os.environ.get('SECODE_WINDOWS_HOST'))
    parser.add_argument('--mirror-path', default=os.environ.get('SECODE_CANONICAL_REPOSITORY'))
    parser.add_argument('--actor', help='Registered sender for phase notifications')
    parser.add_argument('--author', action='append', default=[], help='Explicit PR=REGISTERED_AGENT mapping')
    parser.add_argument('--timeout', type=int, default=14400)
    parser.add_argument('--staging-timeout', type=int, default=7200)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--execute', action='store_true')
    mode.add_argument('--resume', action='store_true')
    mode.add_argument('--resume-mirror', action='store_true')
    options = parser.parse_args(argv)
    if (not re.fullmatch('[a-z0-9][a-z0-9-]{0,59}', options.name)
            or not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', options.project)
            or not 1 <= options.timeout <= 86400 or not 1 <= options.staging_timeout <= 14400):
        parser.error('Use a short batch name, owner/repository and bounded timeouts')
    options.repository = options.repository.resolve(strict=True)
    options.worktrees = options.worktrees.resolve(strict=True)
    options.state = options.state.resolve()
    if options.state.is_relative_to(options.repository) or options.worktrees.is_relative_to(options.repository):
        parser.error('Keep state and batch worktrees outside the controller checkout')
    if options.pr and (len(options.pr) > 8 or len(set(options.pr)) != len(options.pr) or any(n <= 0 for n in options.pr)):
        parser.error('Select one to eight distinct reviewed pull requests')
    if options.execute and (not options.pr or not options.actor):
        parser.error('--execute requires pull requests and an explicit --actor')
    if not options.resume and not options.resume_mirror and not options.pr:
        parser.error('Select at least one pull request')
    return options


def state_for(options):
    if options.resume or options.resume_mirror:
        state = load(options.state / 'batch.json')
        if options.resume_mirror and state.get('version') != 2:
            if state.get('phase') != 'mirror' or not re.fullmatch('[0-9a-f]{40}', state.get('landed', '')):
                raise ValueError('Only a confirmed main publication can resume its mirror')
            mirror(options.repository, options.mirror_host, options.mirror_path, state['landed'])
            state['phase'] = 'complete'
            save(options.state / 'batch.json', state)
            return None
        if (state.get('version') != 2 or state.get('name') != options.name
                or state.get('repository') != str(options.repository) or state.get('project') != options.project
                or options.pr and state['requested'] != options.pr):
            raise ValueError('Resume options do not match this controller checkpoint')
        if options.resume_mirror and not state.get('landed'):
            raise ValueError('Only a confirmed main publication can resume its mirror')
    else:
        state = dict(version=2, name=options.name, repository=str(options.repository), project=options.project,
                     phase='configure', base=None, excluded=[], requested=options.pr, rows=[], mail=None,
                     actor=options.actor, authors=options.author)
        save(options.state / 'batch.json', state)
    if state.get('mail') is None:
        state['mail'] = batch_notify.configure(state['actor'], state['authors'], state['requested'])
        save(options.state / 'batch.json', state)
    if state.get('base') is None:
        compose.git(options.repository, 'fetch', '--no-tags', 'github', 'main')
        state['base'] = compose.git(options.repository, 'rev-parse', 'FETCH_HEAD')
        state['phase'] = 'admission'
        save(options.state / 'batch.json', state)
    return state


def main(argv=None):
    options = arguments(argv)
    if not options.execute and not options.resume and not options.resume_mirror:
        print(__import__('json').dumps(dict(batch=options.name, pull_requests=options.pr,
              steps=['admit reviews', 'compose', 'scan', 'staging', 'CI', 'main', 'PR closure', 'mirror', 'mail'])))
        return 0
    if not options.mirror_host or not options.mirror_path:
        raise ValueError('Configure --mirror-host and --mirror-path before execution')
    if heavy.inherited() is not None:
        raise ValueError('Run the controller outside a build lease; staging schedules itself')
    if not durable_jobs.active():
        command = [sys.executable, '-B', str(Path(__file__).resolve()), *(sys.argv[1:] if argv is None else argv)]
        return durable_jobs.run(command, 'batch-landing', detach=True)
    with lock(options.state, create=options.execute):
        return execute(options)


def execute(options):
    pipeline = None
    try:
        state = state_for(options)
        if state is None:
            return 0
        pipeline = Pipeline(options, state)
        pipeline.record()
        if state.get('publication'):
            pipeline.run([])
        else:
            rows = []
            for number in state['requested']:
                if number in {row['pr'] for row in state['excluded']}:
                    continue
                try:
                    row = github.admission(options.repository, options.project, number, state['base'])
                    previous = next((r for r in state['rows'] if r['number'] == number), None)
                    if previous is not None and previous != asdict(row):
                        raise RuntimeError('An admitted PR changed; preserve this attempt and start fresh')
                    rows.append(row)
                    if previous is None:
                        state['rows'].append(asdict(row))
                        pipeline.record()
                except ValueError as error:
                    pipeline.exclude(number, error)
            if not rows:
                raise ValueError('No pull requests have a current ACCEPT review')
            pipeline.notice('admission', 'Admitted reviewed heads', [row.number for row in rows])
            pipeline.run(rows)
        return int(bool(state['excluded']) or state['phase'] != 'complete')
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        if pipeline is not None:
            pipeline.record(error=str(error) or type(error).__name__)
            pipeline.notice('controller', str(error) or type(error).__name__)
        print(str(error) or type(error).__name__, file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
