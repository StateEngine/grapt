"""Keep build fixtures from discovering repositories at shared scratch roots."""
from operator_paths import data_root
import os

CEILINGS = ("/tmp", str(data_root()/"scratch"))

def apply(environment=None):
    environment = os.environ if environment is None else environment
    previous = environment.get("GIT_CEILING_DIRECTORIES", "")
    if len(previous) > 16384:
        raise ValueError("Git discovery ceilings exceed the environment bound")
    paths = previous.split(os.pathsep) if previous else []
    for ceiling in CEILINGS:
        if ceiling not in paths:
            paths.append(ceiling)
    environment["GIT_CEILING_DIRECTORIES"] = os.pathsep.join(paths)
