import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { Acp, summarize } from './first-request.mjs';
import { spawnOwned, stopOwned } from './process-cleanup.mjs';

const dispatch = 'GRAPT_PROVIDER_DISPATCH attempt=attempt_one at_ms=1 request_body_bytes=10000 tools_bytes=6000 instructions_bytes=3000';
function accounting(tokens = 6000) {
  return {
    schema: 'secode.session-accounting.v1', enumeration_complete: true, session: 'ses_budget',
    attempts: [{ attempt: 'attempt_one', operation: 'assistant', step_terminal_observed: true,
      provider: 'openai', model: 'sol', usage: { input_total: {
        status: 'observed', tokens, provenance: { kind: 'provider-reported' },
      } } }],
  };
}

test('the full inclusive input decides the budget, including its boundary', () => {
  assert.equal(summarize(accounting(6000), dispatch).outcome, 'within-budget');
  const result = summarize(accounting(6001), dispatch);
  assert.equal(result.outcome, 'over-budget');
  assert.equal(result.remaining_tokens, -1);
  assert.deepEqual(result.content_breakdown, {
    unit: 'serialized JSON bytes; not provider token attribution',
    total: 10000, tools: 6000, instructions: 3000, input_options_and_envelope: 1000,
  });
});

test('zero is observed; missing and estimated input never become zero', () => {
  assert.equal(summarize(accounting(0), dispatch).inclusive_input_tokens, 0);
  for (const input of [null, { status: 'unavailable' }, {
    status: 'observed', tokens: 1, provenance: { kind: 'harness-estimated' },
  }]) {
    const value = accounting();
    value.attempts[0].usage.input_total = input;
    assert.throws(() => summarize(value, dispatch), /unavailable/);
  }
});

test('Anthropic inclusive input derived from its reported partition is retained', () => {
  const value = accounting(7000);
  value.attempts[0].provider = 'anthropic';
  value.attempts[0].usage.input_total.provenance = {
    kind: 'provider-derived', rule: 'inclusive-input-from-partition',
  };
  assert.equal(summarize(value, dispatch).inclusive_input_tokens, 7000);
});

test('retries, compaction, incomplete enumeration and an absent terminal refuse', () => {
  const second = accounting();
  second.attempts.push({ ...second.attempts[0], attempt: 'attempt_two' });
  const summary = accounting(); summary.attempts[0].operation = 'compaction';
  const partial = accounting(); partial.enumeration_complete = false;
  const open = accounting(); open.attempts[0].step_terminal_observed = false;
  for (const value of [second, summary, partial, open]) assert.throws(() => summarize(value, dispatch));
});

test('a foreign, duplicated or lost dispatch cannot attest the request', () => {
  for (const log of ['', dispatch.replace('attempt_one', 'other'), `${dispatch}\n${dispatch}`,
    `${dispatch}\nGRAPT_PROVIDER_DISPATCH_LOSS count_at_least=1`]) {
    assert.throws(() => summarize(accounting(), log));
  }
});

test('unknown, overflowing and overlapping byte categories refuse', () => {
  for (const log of [dispatch.replace('10000', 'null'), dispatch.replace('10000', '9007199254740992'),
    dispatch.replace('10000', '8000')]) assert.throws(() => summarize(accounting(), log));
});

test('the ACP client cancels tool permission requests and cleans up on EOF', { timeout: 5000 }, async () => {
  const peer = `
    import readline from 'node:readline';
    const send = row => console.log(JSON.stringify(row));
    let prompt;
    for await (const line of readline.createInterface({input:process.stdin})) {
      const row = JSON.parse(line);
      if (row.method === 'session/prompt') {
        prompt = row.id;
        send({jsonrpc:'2.0',id:999,method:'session/request_permission',params:{options:[{kind:'allow_once',optionId:'allow'}]}});
      } else if (row.id === 999) {
        if (row.result?.outcome?.outcome !== 'cancelled') process.exit(7);
        send({jsonrpc:'2.0',id:prompt,result:{stopReason:'cancelled'}});
      }
    }
  `;
  const client = new Acp(spawnOwned(process.execPath, ['--input-type=module', '-e', peer],
    { stdio: ['pipe', 'pipe', 'pipe'] }));
  try {
    assert.equal((await client.request('session/prompt', {})).stopReason, 'cancelled');
    assert.equal(client.tools, true);
  } finally { await client.stop(); }
});

test('malformed ACP data never appears in the diagnostic', { timeout: 5000 }, async () => {
  const client = new Acp(spawnOwned(process.execPath, ['-e',
    "process.stdin.once('data',()=>console.log('PRIVATE invalid JSON'));process.stdin.resume()"],
  { stdio: ['pipe', 'pipe', 'pipe'] }));
  try {
    await assert.rejects(client.request('initialize', {}), { message: 'ACP returned malformed JSON' });
  } finally { await client.stop(); }
});

test('shutdown allows the existing owner to finish after the old ten-second cutoff', { timeout: 20_000 }, async () => {
  const child = spawnOwned(process.execPath, ['-e',
    "process.stdin.resume();process.stdin.on('end',()=>setTimeout(()=>process.exit(0),11000))"]);
  const closed = new Promise(resolve => child.once('close', resolve));
  child.stdout.resume(); child.stderr.resume();
  await stopOwned(child, closed);
  assert.equal(child.exitCode, 0);
});

test('a failed cleanup exit is joined and never reported as success', { timeout: 5000 }, async () => {
  const child = spawnOwned(process.execPath, ['-e',
    "process.stdin.resume();process.stdin.on('end',()=>process.exit(7))"]);
  const closed = new Promise(resolve => child.once('close', resolve));
  child.stdout.resume(); child.stderr.resume();
  await assert.rejects(stopOwned(child, closed), /exited unsuccessfully/);
  assert.equal(child.exitCode, 7);
});

test('forced cleanup joins the owned tree and preserves an unrelated process', { timeout: 45_000 }, async () => {
  const sibling = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], { stdio: 'ignore' });
  const siblingClosed = new Promise(resolve => sibling.once('close', resolve));
  const peer = `
    const {spawn} = require('node:child_process');
    const product = spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
    product.once('spawn',()=>console.log(JSON.stringify({pid:product.pid})));
    process.stdin.resume();process.stdin.on('end',()=>setInterval(()=>{},1000));
  `;
  const child = spawnOwned(process.execPath, ['-e', peer]);
  const closed = new Promise(resolve => child.once('close', resolve));
  child.stderr.resume();
  try {
    const data = await new Promise(resolve => child.stdout.once('data', resolve));
    const product = JSON.parse(data.toString()).pid;
    child.stdout.resume();
    await assert.rejects(stopOwned(child, closed, 20), /forced cleanup; owned process tree stopped/);
    assert.ok(child.exitCode !== null || child.signalCode !== null, 'ACP was not joined');
    try {
      process.kill(product, 0);
      assert.notEqual(process.platform, 'win32', 'owned Product survived');
      const state = await readFile(`/proc/${product}/stat`, 'utf8');
      assert.equal(state.slice(state.lastIndexOf(')') + 2).split(' ')[0], 'Z', 'owned Product still runs');
    } catch (error) {
      if (!['ESRCH', 'ENOENT'].includes(error.code)) throw error;
    }
    process.kill(sibling.pid, 0);
  } finally {
    sibling.kill(); await siblingClosed;
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await closed;
  }
});
