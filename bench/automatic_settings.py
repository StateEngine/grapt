"""Isolated native pressure settings; the advertised model windows stay intact."""
import hashlib
import json
from pathlib import Path
import shutil

THRESHOLD = 32000
OUTPUT_RESERVE = 16384
TRIGGER_PROFILES = {'c04-32k': THRESHOLD, 'c04-long-128k': 128000}


def trigger_profile(name='c04-32k', server_side=False, harness='grapt'):
    if not isinstance(name, str) or name not in TRIGGER_PROFILES:
        raise ValueError(f'Unknown C04 trigger profile: {name!r}')
    if type(server_side) is not bool:
        raise ValueError('C04 server_side must be a boolean')
    if harness != 'grapt' and (name != 'c04-32k' or server_side):
        raise ValueError(f'C04 trigger profile {name!r} / server_side={server_side} requires grapt')
    return {'trigger_profile': name, 'threshold_tokens': TRIGGER_PROFILES[name],
            'server_side': server_side}


def validate_policy(policy, profile):
    if (policy['threshold_tokens'] != profile['threshold_tokens'] or policy['archive_blocks'] != 12
            or policy['archive_block_bytes'] != 24576 or policy['final_stage_symbol'] != 'migrate_many'):
        raise ValueError(f'C04 fixed pressure policy differs from trigger profile {profile["trigger_profile"]!r}')


def bounded_json(path):
    if path.is_symlink() or path.stat().st_size > 1024*1024:
        raise ValueError('Native configuration input exceeds its bound')
    return json.loads(path.read_text())


def grapt_inventory(root, provider, name):
    config = bounded_json(root/'opencode.jsonc')['provider']
    if config['id'] == provider:
        discovery = config.get('discovery', 'account-models.json')
    else:
        accounts = [row for row in config.get('accounts', []) if row['id'] == provider]
        if len(accounts) != 1:
            raise ValueError('SE requires exactly one selected inventory account')
        discovery = accounts[0]['discovery']
    if discovery not in {'account-models.json', 'openrouter-models.json', 'anthropic-account-models.json'}:
        raise ValueError('SE inventory must be an admitted benchmark snapshot')
    document = bounded_json(root/discovery)
    rows = document.get('models') if isinstance(document, dict) else document
    if not isinstance(rows, list) or len(rows) > 256:
        raise ValueError('SE inventory lacks a bounded model list')
    selected = [row for row in rows if row.get('slug', row.get('id')) == name]
    if len(selected) != 1 or selected[0].get('visibility') == 'hide':
        raise ValueError('SE requires exactly one visible selected inventory model')
    row = selected[0]
    window = row.get('context_window')
    percent = row.get('effective_context_window_percent', 95)
    if type(window) is not int or window <= 0 or type(percent) is not int or not 1 <= percent <= 100:
        raise ValueError('SE inventory context window or effective percentage is invalid')
    return {'inventory_file': discovery, 'inventory_sha256': hashlib.sha256((root/discovery).read_bytes()).hexdigest(),
            'model_context_window': window, 'effective_context_window_percent': percent,
            'effective_percent_source': 'inventory' if 'effective_context_window_percent' in row else 'SE loader default',
            # Match ConnectedModel::context(): apply the inventory percentage
            # before deriving headroom for the requested input pressure.
            'context_budget_tokens': window*percent//100, 'inventory_effective_budget_tokens': window*percent//100,
            'output_reserve_tokens': OUTPUT_RESERVE}


def opencode_limit(model, adapter, replacements):
    path = Path(replacements.get('opencode_model_witness') or
                Path(__file__).with_name('native-model-limits.json'))
    witness = bounded_json(path)
    binary = shutil.which(str(replacements['opencode_binary']))
    if not binary or hashlib.sha256(Path(binary).read_bytes()).hexdigest() != witness['binary_sha256']:
        raise ValueError('OpenCode native limit witness names another binary')
    row = witness['models'][adapter['model']]
    digest = hashlib.sha256(json.dumps(model, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    if digest != row['configured_model_sha256'] or not witness['network_disabled'] or witness['credentials_mounted']:
        raise ValueError('OpenCode native limit witness does not match the actual configuration')
    limits = row['resolved_limit']
    if any(limits.get(key) != model['limit'].get(key) for key in ['context', 'output']):
        raise ValueError('OpenCode native limit witness changes the catalog window')
    return limits, hashlib.sha256(path.read_bytes()).hexdigest()


def configure(env, harness, adapter, replacements):
    profile = trigger_profile(replacements.get('compaction_profile', 'c04-32k'),
                              replacements.get('compaction_server_side', False), harness)
    threshold = profile['threshold_tokens']
    result = {'kind': 'controlled-native-automatic-compaction', 'threshold_tokens': THRESHOLD,
              'threshold_controlled': True,
              'default_window_saturation': False, 'model_window_overridden': False}
    result.update(profile)
    result['compaction_mode'] = ('provider-server' if profile['server_side'] else
                                 'client-summary' if harness == 'grapt' else 'native-client')
    result['route'] = adapter.get('route', adapter.get('auth', 'fixture'))
    if harness == 'codex':
        result['native_setting'] = {'model_auto_compact_token_limit': THRESHOLD}
    elif harness == 'claude':
        result['native_setting'] = {'CLAUDE_CODE_AUTO_COMPACT_WINDOW': 64000,
                                    'CLAUDE_AUTOCOMPACT_PCT_OVERRIDE': 50}
    elif harness in {'opencode', 'grapt'}:
        root = Path(replacements['account_source'])/adapter['cohort']
        catalog = bounded_json(root/'model-catalog.json')
        provider, name = adapter['model'].split('/', 1)
        model = catalog[provider]['models'][name]
        window = model['limit']['context']
        if type(window) is not int or window <= threshold:
            raise ValueError('Native model catalog lacks a usable actual window')
        result['model_context_window'] = window
        if harness == 'opencode':
            limits, digest = opencode_limit(model, adapter, replacements)
            result.update(resolved_model_limits=limits, native_limit_witness_sha256=digest)
            input_limit = limits.get('input')
            if type(input_limit) is not int or input_limit <= THRESHOLD:
                result.update(threshold_controlled=False, threshold_tokens=None,
                              reason='uncontrolled: no supported threshold; native model lacks limit.input')
                return result
            buffer = input_limit-THRESHOLD
            config = {'compaction': {'auto': True, 'buffer': buffer, 'keep': {'tokens': 8000}},
                      'provider': {provider: {'models': {name: model}}}}
            if adapter.get('auth') == 'anthropic-api':
                config['provider'][provider]['options'] = {'baseURL': 'https://api.anthropic.com/v1'}
            env['OPENCODE_CONFIG_CONTENT'] = json.dumps(config)
            result['native_setting'] = config['compaction']
        else:
            inventory = grapt_inventory(root, provider, name)
            if inventory['model_context_window'] != window:
                raise ValueError('SE actual inventory and catalog windows disagree')
            result.update(inventory)
            result['native_setting'] = grapt_config(inventory['context_budget_tokens'], replacements)
            result['supported_source_ref'] = replacements['grapt_compaction_ref']
    else:
        raise ValueError('Automatic pressure requires one of the four native harnesses')
    return result


def grapt_config(budget, replacements):
    # Supported schema comes from the released source, not a catalog-policy override.
    reference = replacements.get('grapt_compaction_ref')
    if not isinstance(reference, str) or not reference.strip() or len(reference) > 512:
        raise ValueError('SE requires the released supported compaction buffer source ref')
    # SE reserves the output budget separately from the input budget.
    profile = trigger_profile(replacements.get('compaction_profile', 'c04-32k'),
                              replacements.get('compaction_server_side', False))
    buffer = budget-profile['threshold_tokens']-OUTPUT_RESERVE
    if buffer < 0:
        raise ValueError('SE model has no room for the disclosed input threshold')
    return {'auto': True, 'buffer': buffer, 'keep': {'tokens': 8000},
            'server_side': profile['server_side']}


def grapt_profile(service, native_setting):
    path = service/'opencode.jsonc'
    if path.is_symlink() or path.stat().st_size > 1024*1024:
        raise ValueError('Isolated SE profile configuration exceeds its bound')
    config = json.loads(path.read_text())
    config['compaction'] = native_setting
    path.write_text(json.dumps(config)+'\n')
