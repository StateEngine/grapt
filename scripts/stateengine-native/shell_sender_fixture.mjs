// Cross-repository native witness. All StateEngine writes use its disposable fixture.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

const [mode, source, descriptor, session] = process.argv.slice(2);
const load = path => import(pathToFileURL(join(source, path)).href);
if (mode === 'setup') {
  const { registeredFixture } = await load('test/helpers/mcp-door-harness.mjs');
  const { stageSenderPin } = await load('test/helpers/fixture-floor.mjs');
  const fixture = await registeredFixture('grapt-shell-sender');
  await mkdir(join(fixture.root, 'memory/seats/restart-seat'), {recursive:true});
  await writeFile(join(fixture.root, 'memory/seats/restart-seat/identity.md'),
    '---\nagent_id: restart-seat\ndisplay_name: Shell fixture\nrole: fixture\n---\n');
  await stageSenderPin(fixture.root, 'stateengine', {sessionId:session,agentId:'restart-seat',sourceCli:'grapt'});
  await writeFile(descriptor, JSON.stringify({...fixture,session}));
  process.stdin.resume();
  await new Promise(resolve => process.stdin.once('end',resolve));
} else {
  const fixture = JSON.parse(await readFile(descriptor,'utf8'));
    assert.equal(mode,'check');
    assert.equal(process.env.SECODE_SESSION_ID,fixture.session);
    assert.equal(process.env.STATEENGINE_SEAT_SURFACE,'grapt');
    process.env.STATEENGINE_ROOT = fixture.root;
    process.env.STATEENGINE_REGISTRY_ROOT = fixture.registryRoot;
    process.env.SECODE_STATEENGINE_REGISTRY_ROOT = fixture.registryRoot;
    const { authenticateSender } = await load('src/sender-authentication.mjs');
    const authenticated = await authenticateSender(fixture.root,'stateengine');
    assert.equal(authenticated.seat,'restart-seat');
    assert.equal(authenticated.sessionId,fixture.session);
    const listed = spawnSync(process.execPath,[join(source,'bin/se.mjs'),'msg','list','--project','stateengine','--json'],
      {cwd:fixture.root,env:process.env,encoding:'utf8',timeout:20_000,maxBuffer:1024*1024});
    assert.equal(listed.status,0,listed.stderr || listed.stdout);
    JSON.parse(listed.stdout);
    console.log('REAL_SE_MSG_LIST_AUTHENTICATED=restart-seat');
}
