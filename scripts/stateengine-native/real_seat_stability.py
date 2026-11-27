"""Observe the real idle subscription without sending input or polling mail."""
import time


def subscription_lines(root, session):
    path = root / 'product.stderr'
    if not path.is_file() or path.stat().st_size > 2 * 1024 * 1024:
        raise ValueError('subscription log absent or exceeded 2 MiB')
    opened = 'native mail subscription opened session=' + session
    dropped = 'native mail subscription dropped session=' + session + ' '
    unavailable = 'native mail unavailable session=' + session + ' '
    rows = [(number, line) for number, line in enumerate(
        path.read_text(encoding='utf-8').splitlines(), 1)
        if line == opened or line.startswith((dropped, unavailable))]
    return rows, opened


def observe_stability(control, session, child, journal, checks, seconds=120):
    """Require one continuously live subscription throughout the idle interval."""
    began = time.monotonic()
    before, opened = subscription_lines(control.root, session)
    live = bool(before) and before[-1][1] == opened
    journal.record('subscription_idle_interval_started', seconds=seconds,
                   source='product.stderr', source_line=before[-1][0] if before else None,
                   exact_line=before[-1][1] if before else None)
    reason = None
    after = before
    while live:
        control.guard()
        if child.poll() is not None:
            reason = 'product exited during subscription observation'
            break
        after, _ = subscription_lines(control.root, session)
        if after != before:
            reason = 'subscription changed during the idle interval'
            break
        if time.monotonic() - began >= seconds:
            break
        time.sleep(0.1)
    elapsed = time.monotonic() - began
    passed = live and reason is None and elapsed >= seconds
    changes = after[len(before):] if after[:len(before)] == before else after
    evidence = [{'source_line': number, 'exact_line': line} for number, line in changes[:3]]
    line = journal.record('native_mail_subscription_stable', passed=passed,
        idle_seconds=elapsed, required_seconds=seconds, source='product.stderr', changes=evidence,
        reason=reason if live else 'no live subscription at start of idle interval')
    checks['native_mail_subscription_stable'] = passed, line
    return passed
