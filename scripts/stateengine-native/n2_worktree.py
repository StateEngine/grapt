"""Native child placement, writes and operator cleanup of owned Windows worktrees."""
import json
from pathlib import Path
import subprocess
import shutil
import sys

from acceptance_peer import Server
from headless_acceptance import SESSION, require, wait, write
from n2_worktree_provider import WorktreeModel
from n2_composer import hard_kill
from ordinary_profile import OrdinaryProfile
from r_windows_reliability import observe
from r_windows_terminal import WindowsTerminal


def command(report, argv, environment, *, cwd=None, check=True, text=None):
    report.record('command', dict(argv=[str(value) for value in argv], cwd=str(cwd) if cwd else None))
    result = subprocess.run([str(value) for value in argv], cwd=cwd, env=environment,
        input=text, text=True, capture_output=True, timeout=30)
    require(len(result.stdout) + len(result.stderr) <= 1024 * 1024, 'N2 command output bound')
    if check:
        require(result.returncode == 0, 'N2 command failed: ' + result.stderr[-3000:])
    return result


def run(args, report):
    model = WorktreeModel()
    journey = terminal = holder = None
    release = args.scratch / 'release-lock'
    with Server(0, model.dispatch) as provider:
        try:
            journey = OrdinaryProfile(args, provider.endpoint)
            observe(args, journey)
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text())
            config.pop('mcp', None)
            config['compaction']['auto'] = False
            config['permission'].update(shell='allow', edit='allow', patch='allow', subagent='allow')
            write(config_path, config)
            source = args.scratch / 'workspace with spaces Ω'
            source.mkdir()
            env = journey.environment

            def git(*arguments, check=True, text=None):
                return command(report, ['git', '-C', source, *arguments], env, check=check, text=text)

            git('init', '--quiet')
            git('config', 'core.autocrlf', 'true')
            git('config', 'core.longpaths', 'true')
            nested = Path(*['nested-' + str(i) + '-' + 'x' * 24 for i in range(4)])
            directory = source / nested
            directory.mkdir(parents=True)
            (directory / 'parent.txt').write_text('committed parent\n')
            (directory / 'line endings.txt').write_bytes(b'one\r\ntwo\r\n')
            files = [str(nested / name) for name in ('parent.txt', 'line endings.txt')]
            git('add', '--', *files)
            git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@invalid',
                'commit', '--quiet', '-F', '-', '--', *files, text='N2 initial files\n')
            (directory / 'parent.txt').write_text('dirty parent preserved\n')
            journey.start(0)
            created = journey.api('POST', '/api/session', dict(id=SESSION, agent='agt_live_headless1',
                model=dict(providerID='openai', id='gpt-live'), location=dict(directory=str(directory))))
            require(created['data']['id'] == SESSION, 'wrong N2 parent Session')
            terminal = WindowsTerminal(args, journey)
            terminal.start(0, directory=directory)
            terminal.send('N2_SPAWN: delegate work in an isolated Git checkout.')
            terminal.until(lambda _: model.placement is not None, 'spawn_agent placement')
            child = model.placement['agent_id']
            child_directory = Path(model.placement['directory'])
            checkout = child_directory
            for _ in nested.parts:
                checkout = checkout.parent
            require(child_directory.resolve().is_relative_to((journey.profile / 'worktrees').resolve()),
                    'spawned directory escapes this private profile')
            require(child_directory.parts[-len(nested.parts):] == nested.parts,
                    'spawn_agent lost the nested working directory')
            require((checkout / '.git').is_file(), 'spawn_agent did not create a linked worktree')
            require(command(report, ['git', '-C', checkout, 'rev-parse', '--abbrev-ref', 'HEAD'], env)
                    .stdout.strip() == 'HEAD', 'child checkout is not detached')
            require((child_directory / 'parent.txt').read_text() == 'committed parent\n'
                    and (directory / 'parent.txt').read_text() == 'dirty parent preserved\n',
                    'child imported dirty parent files or modified the source')
            longest = len(str(child_directory / 'line endings.txt'))
            require(longest > 260, 'N2 long-file-path control needs a longer private scratch name')
            report.record('placement', dict(child=child, directory=str(child_directory),
                path_characters=len(str(child_directory)), longest_file_characters=longest,
                core_longpaths=True))
            report.passed('spawn_agent creates detached nested Windows worktree from committed files')
            terminal.until(lambda _: model.handle is not None, 'child background process handle')
            terminal.until(lambda _: not {SESSION, child}.intersection(journey.api('GET', '/api/session/active')['data']),
                           'child writes and completes')
            history = journey.api('GET', f'/api/session/{child}/message?order=asc&limit=100')
            require('N2_CHILD_VERIFIED' in json.dumps(history) and not provider.errors,
                    'child did not complete its actual background command')
            require((child_directory / 'child-written.txt').read_text() == 'N2 child effect\n'
                    and not (directory / 'child-written.txt').exists(), 'child write crossed placement')
            for identity in (SESSION, child):
                saved = journey.api('GET', f'/api/session/{identity}/message?order=asc&limit=100')
                require(all(not row.get('metadata', {}).get('executionFailure') for row in saved['data']),
                        'N2 worktree Session recorded an execution failure')
            report.passed('background child writes only its worktree and retains a real terminal handle')

            original_pid, provider_calls = terminal.session, model.count()
            hard_kill(journey, report)
            terminal.until(lambda screen: 'Service' in screen and 'Connected' not in screen,
                           'worktree parent offline after hard kill')
            journey.start(1)
            terminal.until(lambda screen: 'Connected' in screen, 'worktree parent reattach')
            require(terminal.session == original_pid, 'worktree restart replaced the R process')
            restored = journey.api('GET', f'/api/session/{child}')['data']
            require(Path(restored['location']['directory']).resolve() == child_directory.resolve()
                    and restored['parentID'] == SESSION, 'restart lost child placement or lineage')
            require(journey.api('GET', f'/api/session/{child}/message?order=asc&limit=100') == history
                    and model.count() == provider_calls, 'restart replayed completed child work')
            require((child_directory / 'child-written.txt').read_text() == 'N2 child effect\n',
                    'restart lost the child file effect')
            terminal.capture('worktree-child-after-hard-kill')
            report.passed('child output and worktree placement survive a hard-kill service restart')

            # Removal is an explicit operator operation. Product worktrees are retained.
            # Archive the child's result and commit only its owned output, so a clean
            # removal really reaches the open-file refusal rather than a dirty check.
            command(report, ['git', '-C', checkout, 'add', '--', nested / 'child-written.txt'], env)
            command(report, ['git', '-C', checkout, '-c', 'user.name=Fixture', '-c',
                'user.email=fixture@invalid', 'commit', '--quiet', '-F', '-', '--',
                nested / 'child-written.txt'], env, text='N2 child output\n')
            outside = args.scratch / 'junction-target'
            outside.mkdir()
            sentinel = outside / 'keep.txt'
            sentinel.write_text('external target must survive\n')
            command(report, ['cmd.exe', '/d', '/c', 'mklink', '/J',
                checkout / 'owned-junction', outside], env)
            locked = args.scratch / 'handle-locked'
            code = ("from pathlib import Path; import time; "
                "f=open(__import__('sys').argv[1],'rb'); "
                "Path(__import__('sys').argv[2]).write_text('locked'); "
                "release=Path(__import__('sys').argv[3]); end=time.monotonic()+60; "
                "exec('while not release.exists() and time.monotonic()<end: time.sleep(.05)'); "
                "f.close(); assert release.exists()")
            argv = [sys.executable, '-u', '-c', code, str(child_directory / 'line endings.txt'),
                    str(locked), str(release)]
            report.record('file holder', dict(argv=argv))
            holder = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait(lambda: locked.exists() if holder.poll() is None else False, 'owned open-file handle')
            command(report, ['cmd.exe', '/d', '/c', 'dir', '/a:l', checkout], env)
            listings = git('worktree', 'list', '--porcelain').stdout
            require(str(checkout).replace('\\', '/') in listings.replace('\\', '/'),
                    'fixture worktree is not registered before removal')
            removed = git('worktree', 'remove', '--force', '--', checkout, check=False)
            require(removed.returncode != 0 and holder.poll() is None,
                    'Windows removal did not encounter the independently held file')
            require((child_directory / 'line endings.txt').read_bytes() == b'one\r\ntwo\r\n'
                    and sentinel.read_text() == 'external target must survive\n',
                    'failed cleanup lost the held file or followed the junction')
            report.record('cleanup refusal', dict(exit=removed.returncode, stderr=removed.stderr))
            report.passed('open-file cleanup refuses and preserves the external junction target')
            release.write_text('release')
            require(holder.wait(timeout=10) == 0, 'file holder did not release cleanly')
            holder = None
            listing = git('worktree', 'list', '--porcelain').stdout.replace('\\', '/')
            if str(checkout).replace('\\', '/') in listing:
                command(report, ['cmd.exe', '/d', '/c', 'dir', '/a:l', checkout], env, check=False)
                git('worktree', 'remove', '--force', '--', checkout)
            else:
                # Git for Windows may unregister a partially removed checkout
                # even when a held file made removal fail. Only this owned,
                # unregistered residual is removed after its handle closes.
                require(checkout.resolve().is_relative_to((journey.profile / 'worktrees').resolve()),
                        'cleanup residual escapes the private worktree area')
                junction = checkout / 'owned-junction'
                if junction.exists():
                    require(junction.is_junction(), 'cleanup link changed type')
                    junction.rmdir()
                report.record('owned residual cleanup', dict(directory=str(checkout),
                    registered=False, held_process_joined=True))
                shutil.rmtree(checkout)
            require(not checkout.exists() and sentinel.read_text() == 'external target must survive\n'
                    and (directory / 'parent.txt').read_text() == 'dirty parent preserved\n',
                    'closed-handle cleanup removed source data or left the checkout')
            require(str(checkout).replace('\\', '/') not in
                    git('worktree', 'list', '--porcelain').stdout.replace('\\', '/'),
                    'closed-handle cleanup left a stale worktree registration')
            terminal.close()
            journey.stop()
            report.passed('closed-handle cleanup succeeds without deleting source or junction target')
        finally:
            release.write_text('release')
            if holder is not None:
                try:
                    holder.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    holder.kill()
                    holder.wait(timeout=5)
            if terminal:
                terminal.cleanup()
            if journey:
                journey.cleanup()
