#!/usr/bin/env python3
"""Execute fixed snapshots once; keep failed and timed-out attempts in the denominator."""
import argparse
import difflib
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import subprocess
import sys
import time
import tempfile
import pressure
import automatic_settings
import anthropic
import grapt_usage
import provider_timing
import cache_diagnostics
import cache_metrics
import report as reporting
from budget import Budget, BudgetStop
from capture import timestamped_log, redact_value, retain_server_log
from accounts import aligned, private_write, document, execution_inputs
from report import LIVE_KINDS, streaming_defect

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts/build"))
from gate import stop_group

MAX_LOG = 32 * 1024 * 1024


def runtime_environment():
    # Native binaries are explicit. No ambient keys, proxies, backend overrides,
    # credential sockets, seat hooks or user-config channels cross into agents.
    home = Path.home()
    return {'PATH': os.defpath, 'HOME': str(home), 'LANG': 'C.UTF-8',
            'LC_ALL': 'C.UTF-8', 'TZ': 'UTC', 'TERM': 'dumb',
            'CODEX_HOME': str(home/'.codex')}


def agent_sandbox(argv, harness, snapshot, run_root, env, product_binary=None, service_root=None,
                  account_override=None, extra_paths=(), auth='chatgpt-oauth'):
    # Hide other checkouts, seat state and saved conversations. Keep only the
    # selected native executable/account file, system tools and disposable work.
    binary = Path(shutil.which(argv[0])).resolve()
    argv = [str(binary), *argv[1:]]
    mounts = ["bwrap", "--die-with-parent", "--unshare-pid", "--ro-bind", "/", "/",
              "--tmpfs", "/home", "--tmpfs", "/tmp", "--tmpfs", "/var/tmp",
              "--tmpfs", "/mnt",
              "--tmpfs", str(ROOT), "--tmpfs", str(run_root)]
    for path in [binary, Path(shutil.which(product_binary)).resolve() if product_binary else None]:
        if path is not None:
            mounts.extend(["--ro-bind", str(path), str(path)])
    if harness == 'codex':
        helper = binary.with_name('codex-code-mode-host')
        if helper.is_file():
            mounts.extend(['--ro-bind', str(helper), str(helper)])
    for path in extra_paths:
        mounts.extend(['--ro-bind', str(path), str(path)])
    home = Path(os.environ["HOME"])
    accounts = {
        "codex": Path(env.get("CODEX_HOME", home / ".codex")) / "auth.json",
        "claude": home / ".claude/.credentials.json",
        "opencode": Path(env.get("XDG_DATA_HOME", home / ".local/share")) / "opencode/auth.json",
    }
    account = account_override or accounts.get(harness)
    if account is not None and account.is_file():
        if harness == "opencode":
            native = json.loads(account.read_text())
            provider, required = ('anthropic', 'api') if auth == 'anthropic-api' else (
                ('openrouter', 'api') if auth == 'openrouter' else ('openai', 'oauth'))
            if auth == 'anthropic-api':
                if native != {} or not env.get('ANTHROPIC_API_KEY'):
                    raise RuntimeError('OpenCode direct Anthropic must use only the selected environment key')
            elif native.get(provider, {}).get('type') != required:
                raise RuntimeError("OpenCode needs its own ChatGPT sign-in; metered credentials cannot substitute")
        mounts.extend(["--ro-bind", str(account), str(account)])
    mounts.extend(["--bind", str(snapshot), str(snapshot)])
    if service_root:
        mounts.extend(["--bind", service_root, service_root])
        # Profile state stays writable; selected bearer credentials cannot rotate.
        # Bind these after the parent profile so its writable mount cannot hide them.
        for name in ['provider.credential', 'openrouter.credential', 'anthropic.credential']:
            credential = Path(service_root)/name
            if credential.is_file():
                mounts.extend(['--ro-bind', str(credential), str(credential)])
    return [*mounts, "--proc", "/proc", "--dev", "/dev", "--chdir", str(snapshot), *argv]


def bounded_process(argv, cwd, prompt, log, timeout, env, monitor=None, receive_timestamps=False,
                    redact=False):
    def limits():
        resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_LOG, MAX_LOG))
    began = time.monotonic()
    secret = env.get('ANTHROPIC_API_KEY')
    captured = receive_timestamps or bool(secret) or redact
    with (timestamped_log(log, MAX_LOG, secrets=(secret.encode(),) if secret else (),
                          timestamp=receive_timestamps, redact=redact) if captured else log.open("wb")) as output:
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                 stdout=output, stderr=subprocess.STDOUT,
                                 start_new_session=True, preexec_fn=limits)
        if captured:
            output.close()  # Only the child holds the pipe write end.
        status = "completed"
        try:
            pending = prompt.encode()
            while True:
                remaining = timeout - (time.monotonic()-began)
                if remaining <= 0:
                    status = 'timeout'
                    break
                try:
                    child.communicate(pending, timeout=min(5, remaining) if monitor else remaining)
                    break
                except subprocess.TimeoutExpired:
                    pending = None
                    if monitor:
                        monitor()
                    else:
                        raise
        except subprocess.TimeoutExpired:
            status = "timeout"
        finally:
            # Reuse the reviewed cleanup: descendants stop even if the leader exited.
            stop_group(child)
    if status != "timeout" and child.returncode:
        status = "crash" if child.returncode < 0 else "failed"
    return status, child.returncode, time.monotonic()-began


def telemetry(log, harness):
    calls = cache_metrics.Calls(harness)
    codex_writes = []
    claude_reported_writes = None
    grapt_sessions = set()
    usage = None
    usage_complete = True
    estimated_cost = None
    compactions = None
    automatic = None
    actual_model = None
    cache_write_tokens = 0
    live_updates = dict.fromkeys(LIVE_KINDS, 0)
    for line in log.read_text(errors="replace").splitlines():
        try:
            row = json.loads(line)
            if isinstance(row, dict) and 'received_at' in row and 'event' in row:
                row = row['event']
        except ValueError:
            continue
        if not isinstance(row, dict):
            continue
        if row.get('direction') == 'server' and isinstance(row.get('message'), dict):
            row = row['message']
        calls.observe(row)
        if harness == 'grapt':
            identity = (row.get('result', {}).get('sessionId')
                        if isinstance(row.get('result'), dict) else None)
            if row.get('type') in {'benchmark.grapt.completed', 'benchmark.automatic.completed'}:
                identity = row.get('session')
            if isinstance(identity, str):
                grapt_sessions.add(identity)
        if harness == 'grapt' and row.get('method') == 'session/update':
            kind = row.get('params', {}).get('update', {}).get('sessionUpdate')
            # ACP names reasoning chunks agent_thought_chunk; the report calls them thought.
            kind = 'thought' if kind == 'agent_thought_chunk' else kind
            if kind in live_updates:
                live_updates[kind] += 1
        if row.get("type") == "turn.completed" and isinstance(row.get("usage"), dict):
            entry = row["usage"]
            if (not all(type(entry.get(key)) is int and entry[key] >= 0
                        for key in ("input_tokens", "cached_input_tokens", "output_tokens"))
                    or entry["cached_input_tokens"] > entry["input_tokens"]):
                usage_complete = False
                continue
            if usage is None:
                usage = {"input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0}
            for key in usage:
                usage[key] += entry[key]
            codex_writes.append(entry.get('cache_write_input_tokens'))
        if harness == 'opencode' and row.get('type') == 'step_finish':
            entry = row.get('part', {}).get('tokens', {})
            cache = entry.get('cache', {})
            values = [entry.get('input'), entry.get('output'), cache.get('read'), cache.get('write')]
            if not all(type(value) is int and value >= 0 for value in values):
                usage_complete = False
                continue
            if usage is None:
                usage = {'input_tokens': 0, 'cached_input_tokens': 0, 'output_tokens': 0}
            # OpenCode reports uncached input separately from both cache classes.
            usage['input_tokens'] += values[0]+values[2]+values[3]
            usage['cached_input_tokens'] += values[2]
            usage['output_tokens'] += values[1]
            cache_write_tokens += values[3]
        if row.get('type') == 'benchmark.grapt.completed' and isinstance(row.get('usage'), dict):
            entry = row['usage']
            values = [entry.get('inputTokens'), entry.get('outputTokens'),
                      entry.get('cachedReadTokens', 0), entry.get('cachedWriteTokens', 0)]
            if all(type(value) is int and value >= 0 for value in values):
                usage = {'input_tokens': values[0]+values[2]+values[3],
                         'cached_input_tokens': values[2], 'output_tokens': values[1]}
                cache_write_tokens = values[3]
            else:
                usage_complete = False
        if row.get("type") == "result" and isinstance(row.get("usage"), dict):
            entry = row['usage']
            values = [entry.get('input_tokens'), entry.get('output_tokens'),
                      entry.get('cache_read_input_tokens', 0), entry.get('cache_creation_input_tokens', 0)]
            if all(type(value) is int and value >= 0 for value in values):
                usage = {'input_tokens': values[0]+values[2]+values[3],
                         'cached_input_tokens': values[2], 'output_tokens': values[1]}
                cache_write_tokens = values[3]
            else:
                usage_complete = False
            explicit_writes = entry.get('cache_creation_input_tokens')
            claude_reported_writes = (explicit_writes if type(explicit_writes) is int
                                     and 0 <= explicit_writes <= 2**53 else None)
            estimated_cost = row.get("total_cost_usd")
            models = row.get("modelUsage", {})
            if isinstance(models, dict) and len(models) == 1:
                actual_model = next(iter(models))
        if isinstance(row.get("message"), dict) and isinstance(row["message"].get("model"), str):
            actual_model = row["message"]["model"]
        if row.get("type") in {"context.compacted", "compact_boundary"} or (
                row.get("type") == "system" and row.get("subtype") == "compact_boundary"):
            compactions = (compactions or 0) + 1
        if row.get('type') == 'benchmark.automatic.completed':
            automatic = row
    if not usage_complete:
        usage = None
    if estimated_cost is None and harness == "codex" and usage is not None:
        # GPT-6 Luna API-equivalent pricing; OAuth subscription spend is separate.
        required = ("input_tokens", "cached_input_tokens", "output_tokens")
        if all(isinstance(usage.get(key), int) for key in required):
            estimated_cost = ((usage["input_tokens"]-usage["cached_input_tokens"])*0.10
                              + usage["cached_input_tokens"]*0.01 + usage["output_tokens"]*0.50)/1_000_000
    # Missing telemetry stays unknown, including compaction counts.
    result = {"usage": usage, "api_equivalent_cost_usd": estimated_cost, "actual_model": actual_model,
            "cache_write_tokens": cache_write_tokens if harness in {"opencode", "claude", "grapt"} and usage is not None else None,
            "observed_compactions": compactions if harness in {"codex", "claude"} else None,
            "live_acp_updates": live_updates if harness == 'grapt' else None}
    result['call_usage'] = calls.result(usage)
    result['cache_write_tokens_observed'] = (cache_write_tokens if harness == 'opencode' and usage is not None
                                           else claude_reported_writes if harness == 'claude' and usage is not None
                                           else None)
    if (result['call_usage'] is not None and result['cache_write_tokens_observed'] is not None
            and sum(c['cache_write_tokens'] for c in result['call_usage'])
                != result['cache_write_tokens_observed']):
        # Equal inclusive input/read totals do not resolve a contradictory write partition.
        result['call_usage'] = None
        result['cache_write_tokens_observed'] = None
    if result['call_usage'] is not None:
        result['cache_write_tokens_observed'] = sum(c['cache_write_tokens'] for c in result['call_usage'])
        durations = [c.get('cache_write_durations') for c in result['call_usage']]
        if all(isinstance(d, dict) for d in durations):
            result['cache_write_durations'] = {k: sum(d[k] for d in durations) for k in ['5m', '1h']}
    elif (harness == 'codex' and usage is not None and codex_writes
          and all(type(v) is int and 0 <= v <= 2**53 for v in codex_writes)):
        result['cache_write_tokens_observed'] = sum(codex_writes)
    if automatic is not None:
        result.update(usage=None, api_equivalent_cost_usd=None, cache_write_tokens=None, call_usage=None)
        result.pop('cache_write_tokens_observed', None)
        result.pop('cache_write_durations', None)
        result.update({key: automatic.get(key) for key in
                       ['observed_compactions', 'compaction_coverage', 'automatic_compactions',
                        'continued_same_session', 'pre_final_automatic_compactions']})
        result.update({key: automatic.get(key) for key in
                       ['new_work_after_compaction', 'stage_before_final', 'stage_after_final']})
    if harness == 'grapt':
        result['grapt_session'] = next(iter(grapt_sessions)) if len(grapt_sessions) == 1 else None
    return result


def create_snapshot(task, directory):
    directory.mkdir(parents=True)
    for name, body in task["files"].items():
        path = (directory / name).resolve()
        if not path.is_relative_to(directory.resolve()) or ".git" in Path(name).parts:
            raise ValueError("Snapshot path escapes its root")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
    subprocess.run(["git", "init", "-q", str(directory)], check=True)
    subprocess.run(["git", "add", "--", *task["files"]], cwd=directory, check=True)
    subprocess.run(["git", "-c", "user.name=Benchmark", "-c", "user.email=benchmark@localhost",
                    "commit", "-q", "-F", "-", "--", *task["files"]],
                   cwd=directory, input=b"Fixed benchmark snapshot\n", check=True)
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=directory).decode().strip()


def hidden_checks(task, directory, log, redact=False):
    # The evaluator appears only after the agent and its descendants are stopped.
    test = directory / "test_hidden_benchmark.py"
    if test.exists() or test.is_symlink():
        raise ValueError("Agent wrote a reserved evaluator path")
    test.write_text(task["hidden_tests"])
    try:
        evaluator = ("import json,sys,unittest; sys.path.insert(0,'.'); "
                     "suite=unittest.defaultTestLoader.discover('.',pattern='test_hidden_benchmark.py'); "
                     "result=unittest.TextTestRunner().run(suite); "
                     "print(json.dumps({'schema':'benchmark.tests.v1','count':result.testsRun,'ok':result.wasSuccessful()})); "
                     "sys.exit(0 if result.wasSuccessful() and result.testsRun>0 else 1)")
        argv = ["bwrap", "--die-with-parent", "--unshare-pid", "--unshare-net"]
        # Imports execute agent-written code. Give evaluation the interpreter and
        # its libraries, without credentials, corpus, other results or networking.
        for path in ["/usr", "/lib", "/lib64"]:
            if Path(path).exists():
                argv.extend(["--ro-bind", path, path])
        argv.extend(["--tmpfs", "/tmp", "--bind", str(directory), str(directory),
                     "--proc", "/proc", "--dev", "/dev", "--chdir", str(directory),
                     sys.executable, "-I", "-B", "-c", evaluator])
        status, code, _ = bounded_process(argv, directory,
                                          "", log, 60, {"PATH": os.defpath}, redact=redact)
        verdict = None
        for line in log.read_text(errors="replace").splitlines():
            try:
                row = json.loads(line)
                if isinstance(row, dict) and row.get("schema") == "benchmark.tests.v1": verdict = row
            except ValueError:
                pass
        if not verdict or verdict.get("count", 0) <= 0 or not verdict.get("ok"):
            return "failed", code if code else 1
        return status, code
    finally:
        test.unlink()


def attempt(task, harness, adapter, run_root, timeout, replacements, budget=None):
    folder = run_root / harness / task["id"]
    snapshot = folder / "repo"
    began = time.monotonic()
    baseline = None
    private = None
    quarantine = None
    log_secrets = ()
    record = {"task": task["id"], "category": task["category"], "held_out": task["held_out"],
              "harness": harness, "requested_model": adapter["model"], "requested_auth": adapter["auth"],
              "actual_model": None, "verified_auth": None,
              "interventions": 0, "diff_quality": None, "success": False}
    record.update(native_harness=adapter.get('harness', harness), cohort=adapter.get('cohort'))
    native_harness = record['native_harness']
    record['route'] = adapter.get('route', adapter['auth'])
    record['compaction_mode'] = (replacements.get('compaction_mode', 'client-summary')
                                if record['native_harness'] == 'grapt' else 'native-client')
    record.update(live_acp_updates=None, streaming_defect=None)
    try:
        if adapter['auth'] == 'anthropic-api':
            quarantine = anthropic.quarantine_directory(run_root)
        if adapter['auth'] == 'openrouter':
            if budget is None: raise BudgetStop('Paid row requires shared-key usage verification')
            record['openrouter_usage_before'] = budget.check()
        baseline = create_snapshot(task, snapshot)
        private_parent = Path.home()/'.cache/grapt/bench-private'
        private_parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        private = tempfile.TemporaryDirectory(prefix='attempt-', dir=private_parent)
        private_root = Path(private.name)
        prompt = task["prompt"] + "\nMake the smallest correct change. Run the public tests. Do not commit."
        values = dict(replacements, prompt=prompt, service_root=str(private_root / "service"))
        argv = list(adapter["argv"])
        for key, value in values.items():
            argv = [arg.replace("{"+key+"}", str(value)) for arg in argv]
        env = runtime_environment()
        if adapter['auth'] == 'anthropic-api':
            anthropic.environment(replacements['anthropic_key'], env)
            log_secrets = (env['ANTHROPIC_API_KEY'].encode(),)
            if adapter.get('harness', harness) == 'opencode' and 'automatic_compaction' not in task:
                env['OPENCODE_CONFIG_CONTENT'] = json.dumps({'provider': {'anthropic': {
                    'options': {'baseURL': 'https://api.anthropic.com/v1'}}}})
        account_override = None
        if 'automatic_compaction' in task:
            profile = automatic_settings.trigger_profile(replacements.get('compaction_profile', 'c04-32k'),
                         replacements.get('compaction_server_side', False), native_harness)
            automatic_settings.validate_policy(task['automatic_compaction'], profile)
            prompt = json.dumps({key: task[key] for key in ['prompt', 'automatic_compaction']})
            automatic_native = (replacements.get('grapt_binary') if native_harness == 'grapt' else
                      replacements.get(native_harness+'_binary', adapter['argv'][0]))
            if not shutil.which(str(automatic_native)):
                raise RuntimeError('Selected automatic native binary is not installed')
            automatic_native = str(Path(shutil.which(str(automatic_native))).resolve())
            argv = [sys.executable, str(ROOT/'bench/automatic_session.py'), '--harness', native_harness,
                    '--binary', str(automatic_native), '--model', adapter['model'],
                    '--trigger-profile', profile['trigger_profile'],
                    '--server-side', 'on' if profile['server_side'] else 'off']
            if native_harness == 'grapt':
                argv.extend(['--product', replacements['product_binary'], '--service', values['service_root']])
            record['compaction_profile'] = automatic_settings.configure(
                env, native_harness, adapter, replacements)
            record['compaction_mode'] = record['compaction_profile']['compaction_mode']
            if not record['compaction_profile']['threshold_controlled']:
                record.update(compaction_coverage='uncontrolled', observed_compactions=None,
                              provider_calls=0)
                raise RuntimeError(record['compaction_profile']['reason'])
        if native_harness == 'opencode' and adapter['auth'] == 'anthropic-api':
            # The native SDK consumes the selected environment key. Mount an
            # empty private auth record so no host subscription can substitute.
            account_override = private_root/'data/opencode/auth.json'
            account_override.parent.mkdir(parents=True, mode=0o700)
            private_write(account_override, '{}')
            env['XDG_DATA_HOME'] = str(private_root/'data')
        elif native_harness == 'opencode':
            native = document(Path.home()/'.local/share/opencode/auth.json')
            provider = 'openrouter' if adapter['auth'] == 'openrouter' else 'openai'
            selected = ({'type': 'api', 'key': budget.credential}
                        if adapter['auth'] == 'openrouter' else native.get(provider))
            if not selected: raise RuntimeError('Selected OpenCode account is absent')
            account_override = private_root/'auth.json'
            private_write(account_override, json.dumps({provider: selected}))
            # Mount the selected native auth record at OpenCode's normal path;
            # no unrelated credentials or benchmark reports carry a copy.
            env['XDG_DATA_HOME'] = str(private_root/'data')
            (private_root/'data/opencode').mkdir(parents=True)
            account_override.rename(private_root/'data/opencode/auth.json')
            account_override = private_root/'data/opencode/auth.json'
        if adapter.get("profile_required"):
            account = replacements.get("account_source")
            if not account:
                raise RuntimeError("Supply the explicit Grapt account-input directory; no live profile fallback")
            account = execution_inputs(Path(account)/adapter['cohort'], private_root/'inputs',
                adapter['cohort'], Path.home()/'.codex/auth.json',
                Path.home()/'.local/share/opencode/auth.json',
                paid_key=env['ANTHROPIC_API_KEY'] if adapter['auth'] == 'anthropic-api' else
                         budget.credential if adapter['auth'] == 'openrouter' else None)
            status, code, _ = bounded_process(
                [replacements["product_binary"], "init-profile", "--service", values["service_root"],
                 "--from", str(Path(account).resolve())], snapshot, "", folder/"profile.log", 300, env)
            if status != "completed":
                raise RuntimeError(f"Supported isolated profile initialization failed: {status}/{code}")
            if 'automatic_compaction' in task:
                automatic_settings.grapt_profile(Path(values['service_root']),
                                                  record['compaction_profile']['native_setting'])
            else:
                config_path = Path(values['service_root'])/'opencode.jsonc'
                config = document(config_path)
                config.setdefault('compaction', {})['server_side'] = record['compaction_mode'] == 'provider-server'
                config_path.write_text(json.dumps(config)+'\n')
        if shutil.which(argv[0]) is None:
            raise RuntimeError("Harness binary is not installed")
        extras = []
        if 'automatic_compaction' in task:
            extras = [ROOT/'bench'/name for name in ['automatic_session.py', 'automatic_settings.py', 'compaction_evidence.py',
                      'codex_compaction.py', 'opencode_compaction.py', 'claude_compaction.py']]
            native = Path(shutil.which(str(automatic_native))).resolve()
            extras.append(native)
            if native_harness == 'codex' and native.with_name('codex-code-mode-host').is_file():
                extras.append(native.with_name('codex-code-mode-host'))
        if adapter.get('profile_required'):
            extras.extend([Path(replacements['grapt_adapter']), Path(replacements['grapt_binary']).resolve()])
        argv = agent_sandbox(argv, native_harness, snapshot, run_root, env,
                             replacements.get("product_binary") if adapter.get("profile_required") else None,
                             values["service_root"] if adapter.get("profile_required") else None,
                             account_override=account_override, extra_paths=extras, auth=adapter['auth'])
        record["status"], record["exit_code"], record["wall_seconds"] = bounded_process(
            argv, snapshot, prompt, folder / "agent.jsonl", timeout, env,
            monitor=budget.check if adapter['auth'] == 'openrouter' else None, receive_timestamps=True)
        record.update(telemetry(folder / "agent.jsonl", adapter.get('harness', harness)))
        if native_harness == 'grapt':
            record.update(grapt_usage.collect(private_root/'service/database.sqlite3',
                                               record.get('grapt_session'), folder/'usage.json'))
            record.update(provider_timing.collect(folder/'agent.jsonl',
                                                  private_root/'service/database.sqlite3',
                                                  record.get('grapt_session')))
            record.update(cache_diagnostics.collect(folder/'agent.jsonl',
                                                    private_root/'service/database.sqlite3',
                                                    record.get('grapt_session'), folder/'request-cache.json'))
        if adapter['auth'] == 'openrouter':
            record['openrouter_usage_after'] = budget.check()
        record["test_status"], record["test_exit"] = hidden_checks(
            task, snapshot, folder / "tests.log", redact=adapter['auth'] == 'anthropic-api')
        record["success"] = record["status"] == "completed" and record["test_status"] == "completed"
        if 'automatic_compaction' in task:
            record['success'] = record['success'] and record.get('compaction_coverage') == 'passed'
    except BudgetStop as error:
        folder.mkdir(parents=True, exist_ok=True)
        record.update(status='budget_stopped', error=str(error), wall_seconds=time.monotonic()-began)
        if budget is not None and adapter['auth'] == 'openrouter':
            record['openrouter_usage_at_stop'] = budget.last
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        folder.mkdir(parents=True, exist_ok=True)
        record.update(status="setup_failed", error=str(error))
        if record.get('compaction_coverage') == 'uncontrolled':
            record['status'] = 'uncontrolled'
        record.setdefault("wall_seconds", time.monotonic()-began)
    finally:
        if private is not None:
            try:
                if record['native_harness'] == 'grapt':
                    record['product_server_log'] = retain_server_log(
                        private_root/'service', folder/'server.log', MAX_LOG, log_secrets)
            except (OSError, ValueError, RuntimeError) as error:
                record.update(status='evidence_failed', success=False,
                              error=f'Server log capture failed: {error}',
                              product_server_log={'state': 'failed'})
            if adapter['auth'] == 'anthropic-api':
                # Direct paid profiles are never retained with diagnostic data.
                private.cleanup()
                if private_root.exists():
                    raise RuntimeError('Direct Anthropic private profile survived teardown')
                record['private_profile_removed'] = True
            else:
                if ((record['native_harness'] == 'grapt' and record.get('status') in {'failed', 'crash', 'timeout'})
                        or (replacements.get('keep_failed_private') and not record['success'])):
                    preserved = private_root.with_name(private_root.name+'-diagnostic')
                    private_root.rename(preserved)
                    record['private_diagnostic'] = str(preserved)
                private.cleanup()
    if baseline is None:
        record["untracked_files"] = []
        (folder/"changes.diff").write_text("")
        if adapter['auth'] == 'anthropic-api':
            record = redact_value(record)
        (folder/"result.json").write_text(json.dumps(record, indent=2)+"\n")
        return record
    diff_path = folder/"changes.diff"
    try:
        status, code, _ = bounded_process(["git", "-c", "core.fsmonitor=false", "diff",
                                          "--no-ext-diff", "--no-textconv", "--binary", baseline, "--"], snapshot,
                                          "", diff_path, 10, {"PATH": os.defpath},
                                          redact=adapter['auth'] == 'anthropic-api')
        if status != "completed":
            raise RuntimeError(f"Changed-code evidence failed: {status}/{code}")
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        record.update(status="evidence_failed", success=False, error=str(error))
    names = folder / "untracked.txt"
    try:
        status, code, _ = bounded_process(["git", "-c", "core.fsmonitor=false", "ls-files",
                                          "--others", "-z"], snapshot,
                                          "", names, 10, {"PATH": os.defpath},
                                          redact=adapter['auth'] == 'anthropic-api')
        if status != "completed":
            raise RuntimeError(f"Untracked evidence failed: {status}/{code}")
        record["untracked_files"] = [name for name in names.read_text().split("\0") if name]
        if len(record['untracked_files']) > 4096:
            raise ValueError('Added-file evidence exceeds its enumeration bound')
        record['untracked_files'] = [name for name in record['untracked_files']
            if not (Path(name).suffix == '.pyc' and '__pycache__' in Path(name).parts)]
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        record.update(status="evidence_failed", success=False, error=str(error), untracked_files=[])
    # Include new source contents so quality review does not miss untracked implementations.
    with (folder / "changes.diff").open("a") as output:
        for name in record["untracked_files"]:
            path = snapshot / name
            if path.is_symlink() or not path.resolve().is_relative_to(snapshot.resolve()):
                record.update(status="evidence_failed", success=False, error="Untracked path escapes the snapshot")
                continue
            if not path.is_file():
                record.update(status="evidence_failed", success=False, error="Untracked evidence must be a regular file")
                continue
            if path.stat().st_size > MAX_LOG or output.tell() + path.stat().st_size > MAX_LOG:
                record.update(status="evidence_failed", success=False, error="Diff exceeds evidence bound")
                break
            content = path.read_bytes()
            try:
                lines = content.decode().splitlines(keepends=True)
                additions = difflib.unified_diff([], lines, fromfile="/dev/null", tofile="b/"+name)
            except UnicodeDecodeError:
                additions = [f"Binary new file b/{name}; sha256 {hashlib.sha256(content).hexdigest()}\n"]
            for addition in additions:
                if adapter['auth'] == 'anthropic-api':
                    addition = redact_value(addition)
                if output.tell() + len(addition.encode()) > MAX_LOG:
                    record.update(status='evidence_failed', success=False, error='Diff exceeds evidence bound')
                    break
                output.write(addition)
            if record['status'] == 'evidence_failed':
                break
    record['streaming_defect'] = streaming_defect(record)
    if 'automatic_compaction' in task and native_harness == 'grapt':
        record['success'] = record['success'] and record['streaming_defect'] is False
    if adapter['auth'] == 'anthropic-api':
        try:
            invalid = anthropic.redact_evidence(folder)
        except (OSError, ValueError, RuntimeError):
            # This is our disposable row. Unknown/unreadable evidence cannot be
            # preserved while claiming it contains no key material.
            anthropic.contain_evidence(folder, quarantine)
            folder.mkdir()
            invalid = True
        masked = redact_value(record)
        invalid = invalid or masked != record
        record = masked
        if invalid:
            record.update(status='evidence_failed', success=False,
                          error='Direct Anthropic evidence failed privacy/type checks; sanitized or removed')
    (folder / "result.json").write_text(json.dumps(record, indent=2)+"\n")
    return record


def deferred_harnesses(selections, adapters):
    """Keep held cohorts planned without probing or launching their binaries."""
    deferred = {}
    for selection in selections:
        harness, separator, reason = selection.partition('=')
        reason = reason.strip()
        if not separator or harness not in adapters or not reason or len(reason) > 512:
            raise ValueError('Deferral requires a selected harness and a reason of 1–512 characters')
        if harness in deferred:
            raise ValueError('Duplicate harness deferral: '+harness)
        deferred[harness] = reason
    return deferred


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--task", action="append", help="run only these development task IDs; never retries")
    parser.add_argument("--harness", action="append", help="run only these adapter keys; repeat for a cohort")
    parser.add_argument('--defer-harness', action='append', default=[], metavar='HARNESS=REASON',
                        help='keep this row planned but do not probe or execute its native binary')
    parser.add_argument("--authorization-ref", help="The actual execution authorization message")
    parser.add_argument("--held-out", action="store_true", help="include held-out tasks only in a frozen weekly run")
    parser.add_argument("--codex-binary", default="codex", help="native CLI path; bypass version-manager shims when necessary")
    parser.add_argument("--opencode-binary", default="opencode")
    parser.add_argument("--grapt-binary", default="grapt")
    parser.add_argument("--product-binary", default="grapt-server")
    parser.add_argument("--account-source", type=Path, help="explicit existing Grapt account inputs, never a database/profile")
    parser.add_argument('--openrouter-key', type=Path, default=Path.home()/'.config/grapt-bench/openrouter.key')
    parser.add_argument('--claude-route', choices=['anthropic-direct', 'openrouter'], default='anthropic-direct')
    parser.add_argument('--anthropic-model', default='claude-haiku-5-5')
    parser.add_argument('--anthropic-key', type=Path, default=Path.home()/'.config/grapt-bench/anthropic-api.key')
    parser.add_argument('--anthropic-monthly-ledger', type=Path)
    parser.add_argument('--compaction-mode', choices=['client-summary', 'provider-server', 'native-client'],
                        default='client-summary', help='Pin SE mode independently of the product default')
    parser.add_argument('--io-pressure-threshold', type=float, help='admit each row only below this Linux IO some avg10 percentage')
    args = parser.parse_args()
    corpus_bytes = (ROOT / "fixtures/benchmarks/tasks.json").read_bytes()
    corpus = json.loads(corpus_bytes)
    adapters = json.loads((ROOT / "bench/adapters.json").read_text())
    adapters = anthropic.adapters_for_route(adapters, args.claude_route, args.anthropic_model)
    if args.harness:
        unknown = set(args.harness)-set(adapters)
        if unknown: parser.error("Unknown harness: "+", ".join(sorted(unknown)))
        if len(set(args.harness)) != len(args.harness): parser.error("Duplicate harness selection")
        adapters = {key: value for key, value in adapters.items() if key in args.harness}
    try:
        deferred = deferred_harnesses(args.defer_harness, adapters)
    except ValueError as error:
        parser.error(str(error))
    active = {key: value for key, value in adapters.items() if key not in deferred}
    tasks = [t for t in corpus["tasks"] if args.held_out or not t["held_out"]]
    if args.task:
        unknown = set(args.task)-{t['id'] for t in tasks}
        if unknown: parser.error('Unknown or excluded task: '+', '.join(sorted(unknown)))
        if len(set(args.task)) != len(args.task): parser.error('Duplicate task selection')
        tasks = [t for t in tasks if t['id'] in args.task]
    if not args.execute:
        print(json.dumps({"tasks": len(tasks), "harnesses": list(adapters), "attempts": len(tasks)*len(adapters),
                          "deferred_harnesses": deferred, "paid_calls": 0,
                          "corpus": hashlib.sha256(corpus_bytes).hexdigest()}, indent=2))
        return 0
    if not args.authorization_ref:
        parser.error("execution authorization is pending; supply the actual received authorization reference")
    if any(row['auth'] == 'anthropic-api' for row in active.values()) and not args.anthropic_monthly_ledger:
        parser.error('Direct Anthropic rows require a separate monthly ledger')
    if any(row['auth'] == 'chatgpt-oauth' for row in active.values()):
        aligned(Path.home()/'.codex/auth.json', Path.home()/'.local/share/opencode/auth.json')
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    records = []
    budget = Budget(args.openrouter_key)
    monthly = anthropic.MonthlySpend(args.anthropic_monthly_ledger) if args.anthropic_monthly_ledger else None
    report = {"planned_tasks": [t["id"] for t in tasks], "planned_harnesses": list(adapters),
              "corpus": hashlib.sha256(corpus_bytes).hexdigest(), "authorization_ref": args.authorization_ref,
              "attempts": records, "harness_versions": {}, "harness_binaries": {}, "total": 0, "passed_tests": 0,
              "quality_review_pending": 0}
    report['started_utc'] = datetime.now(timezone.utc).isoformat()
    report['deferred_harnesses'] = deferred
    report['held_out_included'] = args.held_out
    report['adapters'] = {key: {field: value for field, value in adapter.items() if field != 'argv'}
                          for key, adapter in adapters.items()}
    report['runner_source'] = {name: hashlib.sha256((ROOT/name).read_bytes()).hexdigest()
                               for name in ['bench/run.py', 'bench/report.py', 'bench/adapters.json', 'bench/budget.py',
                                            'bench/accounts.py', 'bench/grapt.py', 'bench/grapt_usage.py',
                                            'bench/provider_timing.py', 'bench/cache_diagnostics.py',
                                            'bench/cache_metrics.py', 'bench/cache-prices.json',
                                            'scripts/build/gate.py']}
    report['execution_policy'] = {'isolation': 'outer bubblewrap, PID namespace, masked host homes/corpus/results',
                                  'codex_inner_sandbox': 'danger-full-access inside external isolation; nested namespaces unavailable',
                                  'attempt_limit_seconds': 600, 'evaluator_limit_seconds': 60, 'retries': 0}
    report['runtime_helpers'] = {}
    if args.io_pressure_threshold is not None:
        report['io_pressure_policy'] = {'threshold_percent': args.io_pressure_threshold,
                                        'wait_limit_seconds': 900, 'sample': 'some avg10'}
    report['product_binary'] = None
    report['budget'] = {'run_stop_usd': 30, 'utc_week_stop_usd': 70,
                        'usage_scope': 'shared key, including concurrent use elsewhere'}
    replacements = {"codex_binary": args.codex_binary, "opencode_binary": args.opencode_binary,
                    "grapt_binary": args.grapt_binary, "product_binary": args.product_binary,
                    "account_source": args.account_source}
    replacements['grapt_adapter'] = str(ROOT/'bench/grapt.py')
    replacements.update(anthropic_key=args.anthropic_key, compaction_mode=args.compaction_mode)
    def save_report():
        report['summary'] = reporting.summary(records, list(adapters), report['planned_tasks'],
                                              report['adapters'])
        report.update(total=len(records), passed_tests=sum(r["success"] for r in records),
                      quality_review_pending=len(records),
                      measurement_complete=len(records) == len(tasks)*len(adapters))
        (args.output / "results.json").write_text(json.dumps(report, indent=2)+"\n")
    save_report()  # Interrupted runs still name every planned row in their denominator.
    for harness, adapter in active.items():
        binary = args.grapt_binary if adapter.get("harness", harness) == "grapt" else adapter["argv"][0]
        for key, value in replacements.items():
            binary = binary.replace("{"+key+"}", str(value))
        log = args.output / (harness + "-version.log")
        try:
            path = Path(shutil.which(binary)).resolve()
            if not path.is_file() or path.stat().st_size > 2 * 1024 * 1024 * 1024:
                raise ValueError("CLI executable exceeds fingerprint bound")
            digest = hashlib.sha256()
            with path.open("rb") as executable:
                for chunk in iter(lambda: executable.read(1024 * 1024), b""):
                    digest.update(chunk)
            report["harness_binaries"][harness] = {"path": str(path), "sha256": digest.hexdigest()}
            if adapter.get('harness', harness) == 'codex':
                helper = path.with_name('codex-code-mode-host')
                if helper.is_file():
                    if helper.stat().st_size > 512*1024*1024: raise ValueError('Runtime helper exceeds fingerprint bound')
                    digest = hashlib.sha256()
                    with helper.open('rb') as executable:
                        for chunk in iter(lambda: executable.read(1024*1024), b''):
                            digest.update(chunk)
                    report['runtime_helpers'][harness] = {'path': str(helper), 'sha256': digest.hexdigest()}

            if adapter.get('profile_required') and report['product_binary'] is None:
                product = Path(shutil.which(args.product_binary)).resolve()
                if not product.is_file() or product.stat().st_size > 2*1024*1024*1024:
                    raise ValueError('Product executable exceeds fingerprint bound')
                digest = hashlib.sha256()
                with product.open('rb') as executable:
                    for chunk in iter(lambda: executable.read(1024*1024), b''):
                        digest.update(chunk)
                report['product_binary'] = {'path': str(product), 'sha256': digest.hexdigest()}
            status, code, _ = bounded_process([binary, "--version"], args.output, "", log, 10, runtime_environment())
            report["harness_versions"][harness] = log.read_text().strip() if status == "completed" else None
        except (OSError, TypeError, ValueError, RuntimeError, subprocess.SubprocessError):
            report["harness_versions"][harness] = None
    save_report()
    for task in tasks:
        for harness, adapter in active.items():
            try:
                admission = (pressure.wait(args.io_pressure_threshold, 900)
                             if args.io_pressure_threshold is not None else None)
            except (OSError, KeyError, ValueError, RuntimeError) as error:
                report['capacity_stop'] = str(error)
                save_report()
                print(f'Capacity stop before {harness}/{task["id"]}: {error}', flush=True)
                return 2
            if adapter['auth'] == 'anthropic-api':
                monthly.check()
            record = attempt(task, harness, adapter, args.output, 600,
                             replacements, budget=budget)
            if adapter['auth'] == 'anthropic-api':
                record['anthropic_monthly_spend'] = monthly.record(record)
                (args.output/harness/task['id']/'result.json').write_text(json.dumps(record, indent=2)+'\n')
            if admission is not None:
                record['io_pressure_before'] = admission
                (args.output/harness/task['id']/'result.json').write_text(json.dumps(record, indent=2)+'\n')
            records.append(record)
            print(f"{harness}/{task['id']}: {record['status']}", flush=True)
            save_report()
    return 0 if report['measurement_complete'] else 1


if __name__ == "__main__":
    sys.exit(main())
