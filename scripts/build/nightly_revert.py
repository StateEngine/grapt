"""Publish a tested nightly revert through the ordinary canonical landing helper."""
import os
import operator_paths
import heavy
import base64
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess


@dataclass(frozen=True)
class Check:
    green: bool
    log: Path | None = None
    failures: tuple = ()
    diagnostic: str = "No source failure identity was produced; inspect the gate driver log"

    def __bool__(self):
        return self.green


def quote(value):
    if not isinstance(value, str) or len(value) > 32768 or '\0' in value:
        raise ValueError('Invalid bounded PowerShell argument')
    return "'"+value.replace("'", "''")+"'"


def remote(script, *, timeout=120):
    script = "$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[Text.UTF8Encoding]::new();"+script
    if len(base64.b64encode(script.encode('utf-16le'))) > 16384:
        raise ValueError('Nightly controller command exceeds its bound')
    encoded = base64.b64encode(operator_paths.powershell_environment(script).encode('utf-16le')).decode('ascii')
    result = heavy.run(['ssh', '-n', os.environ['SECODE_WINDOWS_HOST'], 'powershell.exe', '-NoProfile',
                             '-NonInteractive', '-EncodedCommand', encoded],
                            capture_output=True, text=True, timeout=timeout)
    if len(result.stdout)+len(result.stderr) > 64*1024*1024:
        raise RuntimeError('Nightly controller output exceeds its bound')
    if result.returncode:
        raise RuntimeError('Nightly controller refused: '+result.stderr[-4096:]+result.stdout[-4096:])
    return result.stdout


def registry():
    raw = remote("& node $env:STATEENGINE_ENTRY agents --project secode --json;exit $LASTEXITCODE")
    if len(raw) > 65536:
        raise ValueError('Agent registry response exceeds 64 KiB')
    value = json.loads(raw)
    if value.get('project_id') != 'secode' or value.get('source') != 'stateengine-registry':
        raise ValueError('Nightly requires the authoritative secode agent registry')
    agents = value.get('agents', [])
    if not 0 < len(agents) <= 256:
        raise ValueError('Invalid bounded agent registry')
    return agents, value['lead_agent_id']


def recipients(plan, agents, lead, actor):
    ids = [row['agent_id'] for row in agents]
    if len(ids) != len(set(ids)) or actor not in ids or lead not in ids:
        raise ValueError('Notification actor or coordinator is not registered')
    email_id = plan['author_email'].removesuffix('@stateengine.local')
    owners = {row['agent_id'] for row in agents
              if row['agent_id'] == email_id or row.get('display_name') == plan['author_name']}
    if len(owners) != 1:
        raise ValueError('Cannot identify the landing author uniquely; no automatic revert')
    return sorted(owners | {lead})


def send(actor, recipient, body, *, timeout=None, idempotency_key=None):
    if idempotency_key is not None and (not isinstance(idempotency_key, str)
            or not re.fullmatch(r'[A-Za-z0-9:_.-]{1,128}', idempotency_key)):
        raise ValueError('Notification delivery identity must be a bounded key')
    identity = '' if idempotency_key is None else ' --idempotency-key ' + quote(idempotency_key)
    # Transfer the exact text as base64, never as an interpolated shell argument.
    encoded = base64.b64encode(body.encode('utf-8')).decode('ascii')
    script = ("$p=Join-Path $env:TEMP ('secode-nightly-mail-'+[Guid]::NewGuid().ToString('N')+'.txt');"
              "try{[IO.File]::WriteAllBytes($p,[Convert]::FromBase64String("+quote(encoded)+"));"
              "& node $env:STATEENGINE_ENTRY msg send --project secode --project-root $env:STATEENGINE_PROJECT_ROOT"
              " --agent-id "+quote(actor)+" --to "+quote(recipient)+
              identity+" --kind finding --body-file $p --json;if($LASTEXITCODE){throw 'Notification refused'}}"
              "finally{Remove-Item -LiteralPath $p -ErrorAction SilentlyContinue}")
    return remote(script) if timeout is None else remote(script, timeout=timeout)


class Publisher:
    def __init__(self, actor):
        if not isinstance(actor, str) or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', actor):
            raise ValueError('Configure an explicit StateEngine notification agent; use --dry-run otherwise')
        self.actor = actor
        self.warning = None

    def report(self, result, failures):
        agents, lead = registry()
        if self.actor not in {row['agent_id'] for row in agents}:
            raise ValueError('Nightly notification actor is not registered')
        body = ('Nightly needs operator attention: '+result+
                '. Failure: '+', '.join(failures))[:4096]
        send(self.actor, lead, body)

    def __call__(self, root, state, plan):
        pending = state/'pending-notifications.json'
        if pending.exists() or pending.is_symlink():
            raise RuntimeError('Resolve the earlier revert notification delivery before publishing another revert')
        # Resolve every recipient before a canonical mutation. Missing or
        # ambiguous ownership is a blocker, never permission to skip mail.
        agents, lead = registry()
        targets = recipients(plan, agents, lead, self.actor)
        for name in ('candidate', 'good', 'culprit', 'parent', 'reverted'):
            if not re.fullmatch('[0-9a-f]{40}', plan[name]):
                raise ValueError('Nightly plan requires full commit identities')
        for name in ('red_log', 'parent_log', 'culprit_log', 'gate_log'):
            value = plan.get(name)
            if not isinstance(value, str) or not 0 < len(value) <= 256 or not Path(value).is_absolute():
                raise ValueError('The tested revert needs bounded red/green gate references')
        branch = 'nightly-revert-'+plan['reverted'][:12]
        existing = subprocess.run(['git', 'rev-parse', '--verify', 'refs/heads/'+branch],
                                  cwd=root, capture_output=True, text=True, timeout=30)
        if existing.returncode == 0:
            if existing.stdout.strip() != plan['reverted']:
                raise ValueError('Preserve the existing nightly branch; it differs from the tested revert')
        else:
            subprocess.run(['git', 'branch', branch, plan['reverted']], cwd=root, check=True, timeout=30)
        subprocess.run(['git', 'checkout', '--quiet', branch], cwd=root, check=True, timeout=30)
        script = ("& $env:SECODE_LANDING_CONTROLLER -Branch "+quote(branch)+
                  " -AcceptedCommit "+quote(plan['reverted'])+" -LinuxRoot "+quote(str(root))+
                  " -Owner "+quote(self.actor)+" -Tier 1 -GateLog "+quote(plan['gate_log'])+
                  " -ExpectedBase "+quote(plan['candidate'])+";if(-not $?){exit 1}")
        output = remote(script, timeout=15000)
        landed = re.findall(r'^LANDED ([0-9a-f]{40}) \(was ([0-9a-f]{40})\)\s*$', output, re.M)
        if landed != [(plan['reverted'], plan['candidate'])]:
            raise RuntimeError('Canonical helper did not confirm the exact tested revert and base')
        body = (f"landed {plan['reverted']}: nightly reverted {plan['subject'][:512]!r}. "
                f"Culprit {plan['culprit']}; checked green parent {plan['parent']}. "
                'Failure: '+', '.join(plan['failures'])[:1024]+'. '
                f"Evidence: red {plan['red_log']}; repeated culprit {plan['culprit_log']}; "
                f"parent green {plan['parent_log']}; revert green {plan['gate_log']}.")
        pending = []
        for target in targets:
            try:
                send(self.actor, target, body)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                pending.append(dict(to=target, body=body, error=str(error)[:4096]))
        if pending:
            # A mail failure cannot turn a successful canonical revert into a
            # claimed failed landing. Preserve the unresolved delivery visibly.
            path = state/'pending-notifications.json'
            path.write_text(json.dumps(pending, indent=2)+'\n')
            self.warning = 'revert landed; notification delivery needs operator attention'
            print(self.warning, flush=True)
        return plan['reverted']
