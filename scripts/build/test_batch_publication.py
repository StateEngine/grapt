"""Clean batch metadata can replace invalid history, never forbidden content."""
import unittest

import batch_compose as compose
from test_batch_land import repository, git, commit


class BatchPublication(unittest.TestCase):
    def test_invalid_commit_metadata_squashes_the_entire_reviewed_content(self):
        prefix = 'fix(fixture): '
        trailer = compose.TRAILER
        variants = {
            'subject_boundary': (prefix + 'x' * (72 - len(prefix)), trailer, None),
            'body_length': ('fix(fixture): add content',
                            'First reason\nSecond reason\nThird reason\n\n' + trailer, None),
            'author': ('fix(fixture): add content', trailer,
                       'Alternate Fixture <456+alternate@users.noreply.github.com>'),
            'missing_trailer': ('fix(fixture): add content', 'One reason', None),
        }
        for kind, (subject, body, author) in variants.items():
            with self.subTest(kind=kind), repository() as (root, base, _, _):
                git(root, 'checkout', '-q', '-b', 'candidate')
                (root / 'first.txt').write_text('First reviewed content\n')
                commit(root)
                (root / 'second.txt').write_text('Second reviewed content\n')
                git(root, 'add', '--', 'second.txt')
                git(root, 'commit', '-q', '-m', subject + '\n\n' + body,
                    *(['--author', author] if author else []))
                candidate = git(root, 'rev-parse', 'HEAD')
                self.assertTrue(compose.scan(root, base, candidate))
                git(root, 'checkout', '-q', '--detach', base)
                landed = compose.merge(root, 11, candidate, base)
                self.assertEqual(git(root, 'show', '-s', '--format=%P', landed), base)
                self.assertEqual(git(root, 'rev-list', base + '..' + landed).split(), [landed])
                self.assertEqual((root / 'first.txt').read_text(), 'First reviewed content\n')
                self.assertEqual((root / 'second.txt').read_text(), 'Second reviewed content\n')
                self.assertEqual(git(root, 'show', '-s', '--format=%b', landed),
                                 'Lands pull request 11.\n\n'
                                 'Co-Authored-By: Claude <noreply@anthropic.com>')
                self.assertEqual(compose.scan(root, base, landed), [])

    def test_forbidden_content_is_refused_instead_of_being_sanitized_by_squash(self):
        with repository() as (root, base, _, _):
            git(root, 'checkout', '-q', '-b', 'candidate')
            forbidden = '/'.join(('', 'home', 'private-fixture', 'data'))
            (root / 'feature.txt').write_text(forbidden + '\n')
            candidate = commit(root)
            self.assertTrue(any('real-path' in problem for problem in compose.scan(root, base, candidate)))
            git(root, 'checkout', '-q', '--detach', base)
            with self.assertRaisesRegex(ValueError, 'real-path'):
                compose.merge(root, 11, candidate, base)
            self.assertEqual((root / 'feature.txt').read_text(), forbidden + '\n')


if __name__ == '__main__':
    unittest.main()
