"""Prepare explicitly supplied accounts in a private directory, never a report."""
import argparse
import json
import hmac
import os
from pathlib import Path
import stat
import urllib.request

from budget import key_material

LUNA = 'gpt-6-luna'
HAIKU = 'anthropic/claude-haiku-5.5'
# Anthropic's input-overflow bound equals its published context window:
# https://platform.claude.com/docs/en/build-with-claude/context-windows
HAIKU_INPUT_LIMIT = 1000000


def document(path, maximum=65536):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > maximum:
        raise ValueError('Account document must be a bounded regular file')
    value = json.loads(path.read_text())
    if not isinstance(value, dict): raise ValueError('Account document must be an object')
    return value


def codex_target(target, home):
    """Open every component beneath the home anchor without following links."""
    relative = target.relative_to(home)
    if not relative.parts:
        raise ValueError('Codex account target is not a regular file')
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    directory = os.open(home.anchor, directory_flags)
    try:
        for component in home.parts[1:] + relative.parts[:-1]:
            previous = directory
            directory = os.open(component, directory_flags, dir_fd=previous)
            os.close(previous)
        return os.open(relative.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                       dir_fd=directory)
    finally:
        os.close(directory)


def codex_document(path, maximum=65536):
    """Read a managed Codex link without changing its binding or copying it."""
    if not path.is_symlink():
        return document(path, maximum)
    if not all(hasattr(os, name) for name in ['getuid', 'O_NOFOLLOW', 'O_DIRECTORY']):
        raise ValueError('Codex account target ownership cannot be checked')
    try:
        target = Path(os.path.realpath(path, strict=True))
        if not target.is_relative_to(Path.home()):
            raise ValueError('Codex account target is outside the home directory')
        descriptor = codex_target(target, Path.home())
        try:
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError('Codex account target is not a regular file')
            if metadata.st_uid != os.getuid():
                raise ValueError('Codex account target has a foreign uid')
            if metadata.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
                raise ValueError('Codex account target is group- or world-writable')
            if metadata.st_size > maximum:
                raise ValueError('Codex account target exceeds the document bound')
            with os.fdopen(descriptor, 'rb', closefd=False) as auth:
                raw = auth.read(maximum + 1)
        finally:
            os.close(descriptor)
        if len(raw) > maximum:
            raise ValueError('Codex account target exceeds the document bound')
    except OSError:
        raise ValueError('Codex account target is unreadable') from None
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError):
        raise ValueError('Codex account document is invalid') from None
    if not isinstance(value, dict):
        raise ValueError('Codex account document must be an object')
    return value


def aligned(codex, opencode):
    tokens, oauth = codex_document(codex).get('tokens', {}), document(opencode).get('openai', {})
    if (not isinstance(tokens, dict) or not isinstance(oauth, dict)
            or oauth.get('type') != 'oauth' or not isinstance(oauth.get('accountId'), str) or not oauth['accountId']
            or tokens.get('account_id') != oauth['accountId']):
        raise ValueError('Default Codex and reference OpenCode accounts do not match; no call authorized')
    return tokens


def fetch(url, headers=None):
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers=headers or {}), timeout=15) as response:
            body = response.read(16*1024*1024+1)
        if len(body) > 16*1024*1024:
            raise ValueError('Account model response exceeded its bound')
        return json.loads(body)
    except (OSError, ValueError):
        raise RuntimeError('Account model discovery failed; no credential or response body recorded') from None


def private_write(path, body):
    with path.open('x', encoding='utf-8') as file:
        path.chmod(0o600)
        file.write(body)


def execution_inputs(source, target, cohort, codex, opencode, paid_key=None):
    """Validate and freeze only the selected profile inputs before native init."""
    if cohort == 'haiku-direct':
        return direct_execution_inputs(source, target, paid_key)
    names = ['opencode.jsonc', 'provider.credential', 'account-models.json', 'model-catalog.json']
    if cohort == 'haiku':
        names += ['openrouter.credential', 'openrouter-models.json']
    elif cohort != 'luna':
        raise ValueError('Unsupported benchmark account cohort')
    contents = {}
    for name in names:
        path = source/name
        metadata = path.lstat()
        # Match the product's inventory import bound; credentials/config stay small.
        maximum = 16 * 1024 * 1024 if name in {'account-models.json', 'openrouter-models.json'} else 65536
        if (not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != 0o600
                or metadata.st_size > maximum):
            raise ValueError('Grapt account input must be a bounded private regular file')
        contents[name] = path.read_text()
    config = json.loads(contents['opencode.jsonc'])
    provider = config.get('provider') if isinstance(config, dict) else None
    if not isinstance(provider, dict):
        raise ValueError('Grapt benchmark provider configuration is invalid')
    allowed = {'id', 'model', 'base_model', 'family', 'route', 'account',
               'credential_version', 'policy_version', 'variants', 'accounts'}
    if set(config) != {'store', 'product', 'catalog', 'provider'} or set(provider)-allowed:
        raise ValueError('Grapt benchmark inputs contain an unsupported route/config override')
    primary = {key: provider.get(key) for key in ['id', 'model', 'base_model', 'route']}
    if primary != {'id': 'openai', 'model': LUNA, 'base_model': LUNA, 'route': 'chatgpt-codex'}:
        raise ValueError('Grapt benchmark account route/model does not match the selected cohort')
    if cohort == 'luna':
        tokens = aligned(codex, opencode)
        if (provider.get('account') != tokens['account_id'] or provider.get('accounts')
                or not isinstance(tokens.get('access_token'), str)
                or not hmac.compare_digest(contents['provider.credential'].strip().encode(),
                                           tokens['access_token'].encode())):
            raise ValueError('Grapt Luna account or credential differs from the selected native account')
    else:
        routes = provider.get('accounts')
        expected = {'id': 'openrouter', 'route': 'openrouter-compatible',
                    'protocol_family': 'openai-compatible', 'endpoint': 'https://openrouter.ai/api/v1',
                    'credential': 'openrouter.credential', 'discovery': 'openrouter-models.json'}
        if (not isinstance(routes, list) or len(routes) != 1 or not isinstance(routes[0], dict)
                or any(routes[0].get(key) != value for key, value in expected.items())
                or set(routes[0])-set(expected)-{'account', 'credential_version', 'policy_version'}
                or provider.get('account') != 'benchmark-inert-primary'
                or contents['provider.credential'].strip() != 'BENCHMARK_INERT_PRIMARY'):
            raise ValueError('Grapt paid benchmark route does not match the selected cohort')
        if not paid_key or not hmac.compare_digest(contents['openrouter.credential'].strip().encode(), paid_key.encode()):
            raise ValueError('Grapt paid credential differs from the budget-monitored key')
    target.mkdir(mode=0o700)
    for name, body in contents.items():
        private_write(target/name, body)
    return target


def prepare_direct(source, target, model):
    """Reuse the disclosed Haiku window; change only the account route.

    Preparation uses no key and makes no provider call. The model ID must be
    the ID confirmed in the selected Anthropic account's models discovery.
    """
    from anthropic import adapters_for_route
    adapters_for_route({}, 'anthropic-direct', model)  # Validate the named ID.
    config = document(source/'opencode.jsonc')
    catalog = document(source/'model-catalog.json')
    original = catalog['openrouter']['models'][HAIKU]
    native = dict(original, id=model, name=model)
    catalog.pop('openrouter')
    catalog['anthropic'] = {'id': 'anthropic', 'name': 'Anthropic',
                            'npm': '@ai-sdk/anthropic', 'models': {model: native}}
    config['provider']['accounts'] = [{
        'id': 'anthropic', 'route': 'anthropic-direct', 'account': 'benchmark-anthropic',
        'credential_version': 'benchmark-api-v1', 'policy_version': 'benchmark-v1',
        'protocol_family': 'anthropic-messages', 'endpoint': 'https://api.anthropic.com/v1',
        'credential': 'anthropic.credential', 'discovery': 'anthropic-account-models.json'}]
    inventory = {'models': [{'slug': model, 'visibility': 'list',
                             'context_window': native['limit']['context'],
                             'supported_reasoning_levels': []}]}
    values = [('opencode.jsonc', config), ('model-catalog.json', catalog),
              ('account-models.json', document(source/'account-models.json', 16*1024*1024)),
              ('anthropic-account-models.json', inventory)]
    target.mkdir(mode=0o700)
    for name, value in values:
        private_write(target/name, json.dumps(value))
    private_write(target/'provider.credential', 'BENCHMARK_INERT_PRIMARY')
    return {'provider_calls': 0, 'route': 'anthropic-direct', 'model': model,
            'account_source': str(target)}


def direct_execution_inputs(source, target, key):
    """Only the disposable0700 native import holds the selected API key."""
    if (not isinstance(key, str) or not key.startswith('sk-ant-') or key.startswith('sk-ant-oat')
            or len(key.encode()) > 4096 or any(c.isspace() for c in key)):
        raise ValueError('Direct Anthropic API key is absent')
    config = document(source/'opencode.jsonc')
    routes = config.get('provider', {}).get('accounts', [])
    expected = {'id': 'anthropic', 'route': 'anthropic-direct',
                'protocol_family': 'anthropic-messages', 'endpoint': 'https://api.anthropic.com/v1',
                'credential': 'anthropic.credential', 'discovery': 'anthropic-account-models.json'}
    provider = config.get('provider', {})
    if (set(config) != {'store', 'product', 'catalog', 'provider'}
            or not isinstance(routes, list) or len(routes) != 1 or not isinstance(routes[0], dict)
            or any(routes[0].get(k) != v for k, v in expected.items())
            or set(routes[0])-set(expected)-{'account', 'credential_version', 'policy_version'}
            or provider.get('account') != 'benchmark-inert-primary'
            or {k: provider.get(k) for k in ['id', 'model', 'base_model', 'route']}
               != {'id': 'openai', 'model': LUNA, 'base_model': LUNA, 'route': 'chatgpt-codex'}):
        raise ValueError('Direct Anthropic native account route differs')
    values = {name: document(source/name, 16*1024*1024 if 'models' in name else 65536)
              for name in ['opencode.jsonc', 'model-catalog.json', 'account-models.json',
                           'anthropic-account-models.json']}
    target.mkdir(mode=0o700)
    for name, value in values.items():
        private_write(target/name, json.dumps(value))
    private_write(target/'provider.credential', 'BENCHMARK_INERT_PRIMARY')
    private_write(target/'anthropic.credential', key)
    return target


def prepare(root, codex, opencode, openrouter_key, include_paid=True):
    tokens = aligned(codex, opencode)
    token = tokens.get('access_token')
    if not isinstance(token, str) or not token or len(token.encode()) > 4096 or any(c.isspace() for c in token):
        raise ValueError('Codex access token does not fit Grapt credential format')
    discovery = fetch('https://chatgpt.com/backend-api/codex/models?client_version=0.159.0',
                      {'Authorization': 'Bearer '+token, 'chatgpt-account-id': tokens['account_id'],
                       'originator': 'codex_cli_rs', 'User-Agent': 'codex_cli_rs/0.159.0'})
    luna = next((m for m in discovery.get('models', []) if m.get('slug') == LUNA), None)
    if luna is None or type(luna.get('context_window')) is not int or luna['context_window'] <= 0:
        raise ValueError('Luna is not described by the selected subscription account')
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    catalog = {'openai': {'id': 'openai', 'name': 'OpenAI', 'npm': '@ai-sdk/openai', 'models': {
        LUNA: {'id': LUNA, 'name': luna.get('display_name', LUNA), 'family': 'gpt-6',
               'tool_call': True, 'reasoning': True, 'modalities': {'input': ['text'], 'output': ['text']},
               'limit': {'context': luna['context_window'], 'output': 32768}}}}}
    for cohort in ['luna', *(['haiku'] if include_paid else [])]:
        target = root/cohort; target.mkdir(mode=0o700)
        config = {'store': {'authority': 'initialize'}, 'product': {'build': 'benchmark-setup'},
                  'catalog': {'source': 'models.dev-chatgpt'},
                  'provider': {'id': 'openai', 'model': LUNA, 'base_model': LUNA, 'family': 'gpt-6',
                               'route': 'chatgpt-codex', 'account': tokens['account_id'] if cohort == 'luna' else 'benchmark-inert-primary',
                               'credential_version': 'benchmark-login-v1', 'policy_version': 'benchmark-v1', 'variants': []}}
        private_write(target/'provider.credential', token if cohort == 'luna' else 'BENCHMARK_INERT_PRIMARY')
        private_write(target/'account-models.json', json.dumps({'models': [luna]}))
        if cohort == 'haiku':
            live = fetch('https://openrouter.ai/api/v1/models')
            model = next((m for m in live.get('data', []) if m.get('id') == HAIKU), None)
            if model is None: raise ValueError('Haiku5.5 is absent from OpenRouter catalog')
            if model['context_length'] != HAIKU_INPUT_LIMIT:
                raise ValueError('Haiku catalog window differs from the published input bound')
            catalog['openrouter'] = {'id': 'openrouter', 'name': 'OpenRouter', 'npm': '@openrouter/ai-sdk-provider',
                'models': {HAIKU: {'id': HAIKU, 'name': model['name'], 'family': 'claude-haiku',
                    'tool_call': 'tools' in model['supported_parameters'], 'reasoning': True,
                    'modalities': {'input': ['text'], 'output': ['text']},
                    'limit': {'context': model['context_length'], 'input': HAIKU_INPUT_LIMIT,
                              'output': model['top_provider']['max_completion_tokens']}}}}
            config['provider']['accounts'] = [{'id': 'openrouter', 'route': 'openrouter-compatible',
                'account': 'benchmark-openrouter', 'credential_version': 'benchmark-key-v1',
                'policy_version': 'benchmark-v1', 'protocol_family': 'openai-compatible',
                'endpoint': 'https://openrouter.ai/api/v1', 'credential': 'openrouter.credential',
                'discovery': 'openrouter-models.json'}]
            private_write(target/'openrouter.credential', key_material(openrouter_key))
            private_write(target/'openrouter-models.json', json.dumps({'models': [{
                'slug': HAIKU, 'visibility': 'list', 'context_window': model['context_length'],
                'supported_reasoning_levels': []}]}))
        private_write(target/'opencode.jsonc', json.dumps(config))
        private_write(target/'model-catalog.json', json.dumps(catalog))
    return {'account_alignment': True, 'luna_context': luna['context_window'], 'provider_calls': 0,
            'account_source': str(root), 'cohorts': ['luna', *(['haiku'] if include_paid else [])]}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--codex-auth', type=Path, default=Path.home()/'.codex/auth.json')
    parser.add_argument('--opencode-auth', type=Path, default=Path.home()/'.local/share/opencode/auth.json')
    parser.add_argument('--openrouter-key', type=Path, default=Path.home()/'.config/grapt-bench/openrouter.key')
    parser.add_argument('--direct-anthropic-from', type=Path,
                        help='Prepare direct input from the existing approved Haiku cohort; no key or call')
    parser.add_argument('--anthropic-model', default='claude-haiku-5-5')
    args = parser.parse_args()
    if args.direct_anthropic_from:
        print(json.dumps(prepare_direct(args.direct_anthropic_from, args.output, args.anthropic_model)))
    else:
        print(json.dumps(prepare(args.output, args.codex_auth, args.opencode_auth, args.openrouter_key)))
