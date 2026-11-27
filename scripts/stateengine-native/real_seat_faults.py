"""Apply one config fault before forwarding the launcher's real Session creation."""
import json
from pathlib import Path
import shutil

from launch_proxy import Proxy

FAULTS = ('none', 'config-trust', 'mail-endpoint-missing', 'job-project-missing')


class LaunchProxy(Proxy):
    def __init__(self, endpoint, control, generated, selected, fault):
        super().__init__(endpoint)
        # Observe every request, including creation after an initial metadata
        # read. A persistent connection would otherwise bypass the header hook.
        self.close_http_requests = True
        self.control, self.generated, self.selected, self.fault = control, generated, selected, fault
        self.applied = False

    def faults(self, method, path):
        # This hook runs under the proxy's request lock, before Product sees
        # identity/capability writes or can open its native subscription.
        if method == 'POST' and path.split('?', 1)[0] == '/api/session' and not self.applied:
            if self.generated.stat().st_size > 2 * 1024 * 1024:
                raise ValueError('generated config exceeded 2 MiB')
            if self.fault == 'config-trust':
                shutil.copyfile(self.generated, self.selected)
            elif self.fault == 'mail-endpoint-missing':
                value = json.loads(self.generated.read_text(encoding='utf-8'))
                if 'native_mail_endpoint' not in value.get('stateengine', {}):
                    raise ValueError('positive producer config lacks its native mail endpoint')
                del value['stateengine']['native_mail_endpoint']
                self.generated.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')
            self.control.journal.record('launch_config_observed_before_provision', fault=self.fault,
                                        generated=str(self.generated), selected=str(self.selected))
            self.applied = True
        return False, False


def selected_config(control, generated, fault):
    if fault != 'config-trust':
        return generated
    repository = control.root / 'untrusted-repository'
    control.run('fault-git-init', ['git', 'init', '--quiet', repository], timeout=10)
    selected = repository / 'launch.json'
    control.environment.update(GRAPT_CONFIG=str(selected), OPENCODE_CONFIG=str(selected))
    return selected


def job_command(control, python, job, fault):
    def quote(value):
        return "'" + str(value).replace("'", "''") + "'"
    command = '& ' + ' '.join(quote(value) for value in [*control.entry, 'job', 'start', '--json', '--', python, job])
    prefix = ''.join('$env:' + key + '=' + quote(value) + '; ' for key, value in (
        ('STATEENGINE_ROOT', control.workspace), ('STATEENGINE_REGISTRY_ROOT', control.registry),
        ('STATEENGINE_PROJECT', control.project)))
    owner = ('Remove-Item Env:STATEENGINE_PROJECT_ID -ErrorAction Ignore; '
             if fault == 'job-project-missing' else '$env:STATEENGINE_PROJECT_ID=' + quote(control.project) + '; ')
    command = prefix + owner + command
    shell = Path(control.environment['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    return command, str(shell)


def report_control(root, fault, journal, checks):
    if fault == 'none':
        return
    target = 'generated_mcp_config_trusted' if fault == 'config-trust' else (
        'native_mail_subscribed' if fault == 'mail-endpoint-missing' else 'background_job_wakes_idle_seat')
    passed, line = checks.get(target, (False, None))
    if line is None:
        lines = [index for index, (row, _) in enumerate(journal.rows, 1) if row.get('event') == target]
        line = lines[-1] if lines else None
    evidence = journal.rows[line - 1][0] if line else {}
    if fault == 'config-trust':
        reason = any(state.get('status') == 'blocked' for state in evidence.get('statuses', []))
    elif fault == 'mail-endpoint-missing':
        reason = 'endpoint' in evidence.get('exact_line', '').lower()
    else:
        reason = any(row.get('event') == 'job_started_by_seat'
                     and row.get('notice', {}).get('state') == 'off'
                     and row['notice'].get('reason') == 'no STATEENGINE_PROJECT_ID names the owning project'
                     for row, _ in journal.rows) and any(
                         row.get('event') == 'job_wake_absence_observed' for row, _ in journal.rows)
    applied = any(row.get('event') == 'launch_config_observed_before_provision'
                  and row.get('fault') == fault for row, _ in journal.rows)
    result = dict(fault=fault, assertion=target, detected=applied and not passed and bool(reason), line=line)
    (root / 'control.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
