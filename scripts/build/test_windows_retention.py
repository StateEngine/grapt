"""PC cleanup expires only closed, old temporary runs and preserves held inputs."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

import windows_retention


class RetentionTests(unittest.TestCase):
    def test_linked_namespace_cannot_rotate_or_write_outside_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); outside=root/'outside';outside.mkdir()
            original='preserve foreign log'*(1024**2//20+1)
            log=outside/'retention.log';log.write_text(original)
            linked=root/'native'
            if os.name=='nt':
                subprocess.run(['cmd','/c','mklink','/J',str(linked),str(outside)],check=True,capture_output=True,timeout=5)
            else:
                linked.symlink_to(outside,target_is_directory=True)
            with patch.object(windows_retention,'ROOT',linked/'runs'), \
                    patch.object(windows_retention,'main',side_effect=ValueError('Native retention root must be a real directory')):
                with self.assertRaisesRegex(ValueError,'real directory'):
                    windows_retention.logged_main()
            self.assertEqual(log.read_text(),original)
            self.assertFalse((outside/'retention.previous.log').exists())

    def test_only_completed_old_real_runs_are_removed(self):
        now=time.time(); old=now-windows_retention.RETENTION-60
        for protection in ('none','unfinished','young','retain','linked','worktree','bare-repository'):
            with self.subTest(protection=protection), tempfile.TemporaryDirectory() as directory:
                root=Path(directory); candidate=root/('a'*32); candidate.mkdir()
                (candidate/'tmp').mkdir()
                (candidate/'tmp/file').write_text('derived fixture')
                if protection!='unfinished':(candidate/'done.json').write_text('{"exit":0}')
                if protection=='retain':(candidate/'.secode-retain').touch()
                if protection=='worktree':(candidate/'tmp/.git').write_text('gitdir: retained-owner')
                if protection=='bare-repository':
                    (candidate/'tmp/HEAD').write_text('ref: refs/heads/unlanded')
                    (candidate/'tmp/objects').mkdir()
                outside=root/'outside'; outside.mkdir()
                protected=outside/'data'; protected.write_text('preserved foreign content')
                if protection=='linked':
                    if os.name=='nt':
                        subprocess.run(['cmd','/c','mklink','/J',str(candidate/'link'),str(outside)],check=True,capture_output=True,timeout=5)
                    else:
                        (candidate/'link').symlink_to(outside,target_is_directory=True)
                for marker in (candidate/'tmp/.git', candidate/'tmp/HEAD', candidate/'tmp/objects'):
                    if marker.exists():os.utime(marker,(old,old))
                for path in [candidate/'tmp/file',candidate/'tmp',*candidate.iterdir(),candidate]:
                    if not path.is_symlink() and not (getattr(path.lstat(),'st_file_attributes',0)&0x400):
                        os.utime(path,(old,old))
                if protection=='young':os.utime(candidate/'tmp/file',(now,now))
                windows_retention.reclaim(root,now,time.monotonic()+5)
                self.assertTrue(candidate.exists())
                windows_retention.reclaim(root,now,time.monotonic()+5,apply=True)
                self.assertEqual(candidate.exists(),protection!='none')
                self.assertEqual(protected.read_text(),'preserved foreign content')
