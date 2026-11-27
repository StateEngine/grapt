"""Keep release test sandboxes only when a test fails."""

from pathlib import Path
import shutil
import tempfile

import pytest


SANDBOX = pytest.StashKey[Path]()
FAILED = pytest.StashKey[bool]()


@pytest.fixture
def tmp_path(
    request: pytest.FixtureRequest, tmp_path_factory: pytest.TempPathFactory
) -> Path:
    parent = None
    if request.config.getoption("basetemp") is not None:
        parent = tmp_path_factory.getbasetemp()
    sandbox = Path(tempfile.mkdtemp(prefix="release-test-", dir=parent))
    request.node.stash[SANDBOX] = sandbox
    return sandbox


@pytest.hookimpl(wrapper=True, tryfirst=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo[None]):
    report = yield
    sandbox = item.stash.get(SANDBOX, None)
    if sandbox is None:
        return report
    if report.failed:
        item.stash[FAILED] = True
        report.sections.append(("Retained release test sandbox", str(sandbox)))
    elif report.when == "teardown" and not item.stash.get(FAILED, False):
        try:
            shutil.rmtree(sandbox)
        except OSError as error:
            report.outcome = "failed"
            report.longrepr = f"Could not clean release test sandbox {sandbox}: {error}"
    return report
