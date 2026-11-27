import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import git_environment

class GitEnvironmentTests(unittest.TestCase):
    def test_defaults_preserve_existing_ceilings_without_duplicates(self):
        environment={"GIT_CEILING_DIRECTORIES":"/owned/scratch"}
        git_environment.apply(environment)
        git_environment.apply(environment)
        self.assertEqual(environment["GIT_CEILING_DIRECTORIES"].split(os.pathsep),
                         ["/owned/scratch",*git_environment.CEILINGS])

    def test_parent_repository_is_excluded_but_the_fixture_repository_still_works(self):
        # Plant only a private surrogate of /tmp/.git. Never modify the shared
        # /tmp root while other seats are running their fixtures.
        with tempfile.TemporaryDirectory(prefix="secode-ceiling-",dir="/tmp") as value:
            parent=Path(value); child=parent/"fixture"; child.mkdir()
            environment=os.environ.copy()
            for name in ["GIT_DIR","GIT_WORK_TREE","GIT_COMMON_DIR","GIT_CEILING_DIRECTORIES"]:
                environment.pop(name,None)
            subprocess.run(["git","init","-q",str(parent)],env=environment,check=True)
            args=["git","-C",str(child),"rev-parse","--show-toplevel"]
            inherited=subprocess.run(args,env=environment,capture_output=True,text=True,check=True)
            self.assertEqual(Path(inherited.stdout.strip()),parent)
            environment["GIT_CEILING_DIRECTORIES"]=str(parent)
            git_environment.apply(environment)
            isolated=subprocess.run(args,env=environment,capture_output=True,text=True)
            self.assertNotEqual(isolated.returncode,0)
            self.assertIn("not a git repository",isolated.stderr)
            subprocess.run(["git","init","-q",str(child)],env=environment,check=True)
            own=subprocess.run(args,env=environment,capture_output=True,text=True,check=True)
            self.assertEqual(Path(own.stdout.strip()),child)
