"""Documentation landings retain source checks without acquiring build capacity."""
import json
import os
import unittest
from unittest.mock import patch

import landing
import landing_docs
from test_landing import command, commit, fixture, invoke


class DocumentationLandingTests(unittest.TestCase):
    def test_docs_rebase_and_verification_use_git_without_a_build_reservation(self):
        with fixture() as (outer, canonical, root, base, env):
            docs = root/'docs'; docs.mkdir()
            (docs/'guide.md').write_text('First line  \nSecond line\n')
            (root/'README.md').unlink()
            accepted = commit(root, ['docs/guide.md', 'README.md'])
            (canonical/'a/src/lib.rs').write_text('pub fn value() -> u32 { 2 }\n')
            advanced = commit(canonical, ['a/src/lib.rs'])
            request = dict(accepted=accepted, old_base=base, branch='candidate',
                           canonical=str(canonical), gate_log=str(outer/'unused.log'),
                           receipt=str(outer/'docs.json'), full=False, run_gate=True)
            tools = outer/'tools'; tools.mkdir()
            cargo = tools/'cargo'
            cargo.write_text('#!/bin/sh\necho "Documentation invoked build tooling" >&2\nexit 99\n')
            cargo.chmod(0o755)
            env['PATH'] = str(tools) + os.pathsep + env['PATH']
            result = invoke(root, request, env, mode='docs')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            record = json.loads((outer/'docs.json').read_text())
            request.update(candidate=record['candidate'], new_base=advanced)
            result = invoke(root, request, env, mode='verify-docs')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse((outer/'heavy-queue').exists(), 'Documentation reserved build capacity')
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), accepted)
            self.assertEqual(command(root, 'git', 'status', '--porcelain'), '')
            self.assertFalse((outer/'unused.log').exists())

    def test_code_permissions_prompts_and_symlinks_cannot_enter_the_docs_path(self):
        for mode in ('code', 'permission', 'prompt', 'symlink', 'full',
                     'AGENTS.md', 'CLAUDE.md', 'SKILL.md', 'aGeNtS.Md', 'cLaUdE.Md', 'sKiLl.Md'):
            with self.subTest(mode=mode), fixture() as (outer, canonical, root, base, env):
                docs = root/'docs'; docs.mkdir()
                guide = docs/'guide.md'
                guide.write_text('Documentation\n')
                changed = ['docs/guide.md']
                if mode == 'code':
                    (root/'a/src/lib.rs').write_text('pub fn value() -> u32 { 2 }\n')
                    changed.append('a/src/lib.rs')
                elif mode == 'permission':
                    guide.chmod(0o755)
                elif mode == 'prompt':
                    path = root/'AGENTS.md'; path.write_text('Instructions\n')
                    changed.append('AGENTS.md')
                elif mode == 'symlink':
                    guide.unlink(); guide.symlink_to('../README.md')
                elif mode.casefold() in {'agents.md', 'claude.md', 'skill.md'}:
                    nested = docs/'nested'; nested.mkdir()
                    (nested/mode).write_text('Instructions\n')
                    changed.append('docs/nested/' + mode)
                accepted = commit(root, changed)
                request = dict(accepted=accepted, old_base=base, branch='candidate',
                    full=mode == 'full', canonical=str(canonical), receipt=str(outer/'docs.json'))
                with patch.object(landing.heavy, 'slot', side_effect=AssertionError('Reserved build capacity')):
                    with self.assertRaisesRegex(ValueError, 'Documentation fast path'):
                        landing_docs.compose(root, request)
                self.assertFalse((outer/'docs.json').exists())
