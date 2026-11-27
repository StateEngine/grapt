"""Environment for disposable build fixtures that do not own the parent run."""
import os


def hardware_sample():
    """Private-lock tests exercise ordering; resource policy has separate tests."""
    return dict(available=512*1024**3, cores=128, load=0,
                nvme_free=1024**4, root_free=1024**4)


def isolated_environment():
    # Preserve tools, scratch and the Cargo job cap; discard inherited output
    # ownership, governor hints and wrappers before fixture-local values are set.
    outputs = {'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'RUSTC_WRAPPER',
               'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS'}
    return {key: value for key, value in os.environ.items()
            if not key.startswith(('SECODE_', 'SCCACHE_')) and key not in outputs}


def nextest_result(log, commands):
    """Synthetic receipt input for verifier fixtures; no qualification claim."""
    if not any(command[:3]==['cargo','nextest','run'] for command in commands):
        return None
    import nextest_gate
    nextest_gate.report(log).write_text('<testsuites><testsuite><testcase name="fixture-input"/></testsuite></testsuites>')
    return nextest_gate.verify(log)
