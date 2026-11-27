"""Bounded, line-addressable observations for the real seat launch witness."""
import json
from pathlib import Path
import threading

ASSERTIONS = (
    'generated_mcp_config_trusted', 'resume_packet_on_first_turn',
    'stateengine_tools_listed', 'stateengine_tool_callable',
    'native_mail_subscribed', 'native_mail_subscription_stable', 'mail_wakes_idle_seat',
    'seat_read_acknowledged', 'background_job_wakes_idle_seat',
)


class Journal:
    def __init__(self, root):
        self.root = Path(root)
        self.path = self.root / 'observations.jsonl'
        self.stream = self.path.open('x', encoding='utf-8')
        self.lock = threading.Lock()
        self.rows = []

    def record(self, event, **fields):
        row = dict(event=event, **fields)
        encoded = json.dumps(row, ensure_ascii=True)
        with self.lock:
            if len(self.rows) >= 512 or len(encoded) > 8192:
                raise ValueError('seat observation bound exceeded')
            self.stream.write(encoded + '\n')
            self.stream.flush()
            self.rows.append((row, encoded))
            return len(self.rows)

    def __enter__(self):
        return self

    def __exit__(self, *_error):
        self.close()

    def report(self, checks):
        """A result cites an existing exact line, including missing prerequisites."""
        results = []
        for name in ASSERTIONS:
            passed, line = checks.get(name, (False, None))
            if line is None:
                reason = next((row.get('reason') for row, _ in reversed(self.rows)
                               if row.get('event') == 'witness_refused'), 'assertion not observed')
                line = self.record(name, passed=False, observed=False, reason=reason)
            if not isinstance(line, int) or not 1 <= line <= len(self.rows):
                raise ValueError('assertion has no observation line')
            observation = self.rows[line - 1][0]
            if observation.get('event') != name or observation.get('passed') is not passed:
                raise ValueError('assertion does not match its observation')
            results.append(dict(assertion=name, status='PASS' if passed else 'FAIL',
                                log='observations.jsonl', line=line,
                                exact_line=self.rows[line - 1][1]))
        (self.root / 'assertions.json').write_text(json.dumps(results, indent=2) + '\n',
                                                  encoding='utf-8')
        lines = [f"{row['status']} {row['assertion']} observations.jsonl:{row['line']} "
                 + row['exact_line'] for row in results]
        (self.root / 'assertions.txt').write_text('\n'.join(lines) + '\n', encoding='utf-8')
        return all(row['status'] == 'PASS' for row in results)

    def close(self):
        self.stream.close()
