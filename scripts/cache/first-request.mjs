#!/usr/bin/env node
// One clean prompt through the normal CLI/Product, measured from durable usage.
import { execFile } from 'node:child_process';
import { lstat, mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs, promisify } from 'node:util';
import { spawnOwned, stopOwned } from './process-cleanup.mjs';

const execute = promisify(execFile);
const MAX_FILE = 32 * 1024 * 1024;
const MAX_FRAME = 8 * 1024 * 1024;
const TIMEOUT = 180_000;
const PROMPT = 'Reply with only READY. Do not call tools or change files.';

export function summarize(accounting, log) {
  if (accounting.schema !== 'secode.session-accounting.v1'
      || accounting.enumeration_complete !== true || accounting.attempts?.length !== 1) {
    throw new Error('Expected exactly one enumerated durable attempt');
  }
  const attempt = accounting.attempts[0];
  const input = attempt.usage?.input_total;
  const providerInput = input?.provenance?.kind === 'provider-reported'
    || (input?.provenance?.kind === 'provider-derived'
      && input.provenance.rule === 'inclusive-input-from-partition');
  if (attempt.operation !== 'assistant' || attempt.step_terminal_observed !== true
      || input?.status !== 'observed' || !Number.isSafeInteger(input.tokens) || input.tokens < 0
      || !providerInput) {
    throw new Error('Provider input or durable terminal is unavailable');
  }
  const samples = [];
  for (const line of log.split(/\r?\n/)) {
    if (/^GRAPT_PROVIDER_DISPATCH_LOSS /.test(line)) {
      throw new Error('Dispatch coverage is incomplete');
    }
    if (!line.startsWith('GRAPT_PROVIDER_DISPATCH attempt=')) continue;
    const fields = Object.fromEntries(line.split(' ').slice(1).map(field => field.split('=')));
    samples.push(fields);
  }
  if (samples.length !== 1 || samples[0].attempt !== attempt.attempt) {
    throw new Error('Expected one dispatch matching the durable attempt');
  }
  const count = key => {
    const value = samples[0][key];
    if (!/^\d+$/.test(value ?? '') || !Number.isSafeInteger(Number(value))) {
      throw new Error('Request content breakdown is unavailable');
    }
    return Number(value);
  };
  const body = count('request_body_bytes');
  const tools = count('tools_bytes');
  const instructions = count('instructions_bytes');
  if (body > 4 * 1024 * 1024 || tools + instructions > body) {
    throw new Error('Request content breakdown is inconsistent');
  }
  return {
    schema: 'grapt.first-request.v1',
    outcome: input.tokens <= 6000 ? 'within-budget' : 'over-budget',
    session: accounting.session,
    attempt: attempt.attempt,
    provider: attempt.provider,
    model: attempt.model,
    inclusive_input_tokens: input.tokens,
    budget_tokens: 6000,
    remaining_tokens: 6000 - input.tokens,
    input_provenance: input.provenance,
    content_breakdown: {
      unit: 'serialized JSON bytes; not provider token attribution',
      total: body,
      tools,
      instructions,
      input_options_and_envelope: body - tools - instructions,
    },
  };
}

async function boundedRead(filename) {
  const stat = await lstat(filename);
  if (!stat.isFile() || stat.size > MAX_FILE) throw new Error('Report input is irregular or oversized');
  const body = await readFile(filename, 'utf8');
  if (Buffer.byteLength(body) > MAX_FILE) throw new Error('Report input grew beyond its bound');
  return body;
}

export class Acp {
  constructor(child) {
    this.child = child;
    this.ordinal = 0;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.bytes = 0;
    this.errorBytes = 0;
    this.frames = 0;
    this.tools = false;
    this.closed = new Promise(resolve => child.once('close', resolve));
    child.stdout.on('data', chunk => {
      try { this.receive(chunk); } catch (error) { this.fail(error); }
    });
    child.on('error', () => this.fail(new Error('ACP process failed to start')));
    child.stdin.on('error', () => this.fail(new Error('ACP input closed')));
    child.once('close', () => this.fail(new Error('ACP process closed')));
    child.stderr.on('data', chunk => {
      this.errorBytes += chunk.length;
      if (this.errorBytes > MAX_FILE) this.fail(new Error('ACP diagnostic output bound'));
    }); // Product's bounded diagnostic file is read separately.
    this.timer = setTimeout(() => this.fail(new Error('First-request deadline exceeded')), TIMEOUT);
  }

  fail(error) {
    this.failure = error;
    for (const { reject } of this.pending.values()) reject(error);
    this.pending.clear();
  }

  send(value) {
    const body = JSON.stringify(value) + '\n';
    if (Buffer.byteLength(body) > MAX_FRAME) throw new Error('ACP outgoing frame bound');
    this.child.stdin.write(body);
  }

  request(method, params) {
    if (this.failure) return Promise.reject(this.failure);
    const id = ++this.ordinal;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      try { this.send({ jsonrpc: '2.0', id, method, params }); }
      catch (error) { this.pending.delete(id); reject(error); }
    });
  }

  receive(chunk) {
    this.bytes += chunk.length;
    if (this.bytes > MAX_FILE) throw new Error('ACP capture bound');
    this.buffer = Buffer.concat([this.buffer, chunk]);
    let newline;
    while ((newline = this.buffer.indexOf(10)) !== -1) {
      if (newline > MAX_FRAME || ++this.frames > 10_000) throw new Error('ACP frame bound');
      let row;
      try { row = JSON.parse(this.buffer.subarray(0, newline).toString('utf8')); }
      catch { throw new Error('ACP returned malformed JSON'); }
      if (!row || typeof row !== 'object') throw new Error('ACP returned an invalid frame');
      this.buffer = this.buffer.subarray(newline + 1);
      if (row.method === 'session/request_permission' && row.id !== undefined) {
        this.tools = true;
        this.send({ jsonrpc: '2.0', id: row.id, result: { outcome: { outcome: 'cancelled' } } });
      } else if (row.method === 'session/update'
          && row.params?.update?.sessionUpdate === 'tool_call') {
        this.tools = true;
        this.send({ jsonrpc: '2.0', method: 'session/cancel', params: { sessionId: row.params.sessionId } });
      } else if (row.method === undefined && this.pending.has(row.id)) {
        const pending = this.pending.get(row.id);
        this.pending.delete(row.id);
        if (row.error) pending.reject(new Error('ACP request refused; inspect the private profile log'));
        else pending.resolve(row.result);
      }
    }
    if (this.buffer.length > MAX_FRAME) throw new Error('ACP incomplete frame bound');
  }

  async stop() {
    clearTimeout(this.timer);
    await stopOwned(this.child, this.closed);
  }
}

export async function main(args) {
  const { values } = parseArgs({ args, options: {
    bundle: { type: 'string' }, accounts: { type: 'string' }, output: { type: 'string' },
    model: { type: 'string' }, help: { type: 'boolean' },
  } });
  if (values.help) {
    console.log('first-request.mjs --bundle <package> --accounts <account-inputs> --output <new-directory> --model <provider/model>');
    return 0;
  }
  for (const key of ['bundle', 'accounts', 'output', 'model']) {
    if (!values[key]) throw new Error(`Missing --${key}`);
  }
  const bundle = path.resolve(values.bundle);
  const accounts = path.resolve(values.accounts);
  const output = path.resolve(values.output);
  const suffix = process.platform === 'win32' ? '.exe' : '';
  const cli = path.join(bundle, `grapt${suffix}`);
  const product = path.join(bundle, `grapt-server${suffix}`);
  const service = path.join(output, 'profile');
  const workspace = path.join(output, 'workspace');
  await mkdir(output); // Refuse an existing run; never open a live seat profile.
  await mkdir(workspace);
  await execute(product, ['init-profile', '--service', service, '--from', accounts],
    { cwd: workspace, timeout: TIMEOUT, maxBuffer: 2 * 1024 * 1024, windowsHide: true });
  const acp = new Acp(spawnOwned(cli, ['acp', '--embedded', '--product', product, '--service', service],
    { cwd: workspace, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }));
  let session;
  try {
    await acp.request('initialize', { protocolVersion: 1, clientCapabilities: {} });
    session = (await acp.request('session/new', { cwd: workspace, mcpServers: [] })).sessionId;
    await acp.request('session/set_config_option', { sessionId: session, configId: 'model', value: values.model });
    const result = await acp.request('session/prompt', {
      sessionId: session, prompt: [{ type: 'text', text: PROMPT }],
    });
    if (acp.tools || result.stopReason !== 'end_turn') {
      throw new Error('Expected a text-only clean request; no budget result claimed');
    }
  } finally {
    await acp.stop();
  }
  const { stdout } = await execute(product, ['usage', '--service', service, '--session', session],
    { timeout: TIMEOUT, maxBuffer: 2 * 1024 * 1024, windowsHide: true });
  const accounting = JSON.parse(stdout);
  const report = summarize(accounting, await boundedRead(path.join(service, 'logs', 'server.log')));
  if (`${report.provider}/${report.model}` !== values.model) throw new Error('Observed model differs from selection');
  await writeFile(path.join(output, 'usage.json'), stdout, { flag: 'wx', mode: 0o600 });
  const encoded = JSON.stringify(report, null, 2) + '\n';
  await writeFile(path.join(output, 'first-request.json'), encoded, { flag: 'wx', mode: 0o600 });
  console.log(encoded.trimEnd());
  return report.outcome === 'within-budget' ? 0 : 3;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).then(code => { process.exitCode = code; }).catch(error => {
    // Child-process exception strings can contain private stderr or configuration.
    console.error(error?.cmd ? 'First-request subprocess failed; inspect the private run directory.'
      : `First request refused: ${error.message}`);
    process.exitCode = 2;
  });
}
