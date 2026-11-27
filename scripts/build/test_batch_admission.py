"""A blocking review must prevent automatic publication of a reviewed head."""
import unittest
from unittest.mock import patch

import batch_github as github
from test_batch_land import git, repository


class AdmissionTests(unittest.TestCase):
    def refuse(self, bodies):
        with repository() as (root, head, _, _):
            git(root, 'remote', 'add', 'github', str(root))
            git(root, 'update-ref', 'refs/pull/1/head', head)
            pull = dict(state='open', draft=False, base={'ref': 'main'},
                        head=dict(sha=head, repo={'full_name': 'example/project'}))
            reviews = [dict(id=index, submitted_at=f'2026-01-01T00:00:{index:02}Z',
                            state='COMMENTED', body=body, commit_id=head)
                       for index, body in enumerate(bodies, 1)]
            with patch.object(github, 'api', side_effect=lambda _, __, suffix:
                              reviews if suffix.endswith('reviews?per_page=100') else pull):
                with self.assertRaises(ValueError):
                    github.admission(root, 'example/project', 1, head)

    def test_a_later_comment_requesting_changes_revokes_acceptance(self):
        self.refuse(['0 Important. ACCEPT.', '1 Important. REQUEST CHANGES.'])

    def test_quoting_an_older_accept_does_not_override_a_change_request(self):
        self.refuse(['1 Important. REQUEST CHANGES. The earlier ACCEPT is withdrawn.'])


if __name__ == '__main__':
    unittest.main()
