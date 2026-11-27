// Windows carrier for the existing R reliability journey, adapted from its
// earlier Grapt C8 observer. Dependencies are the retained observer's pinned
// @lydell/node-pty 1.2.0-beta.12 and @xterm/headless 5.5.0, supplied by NODE_PATH.
const fs = require('node:fs');
const pty = require('@lydell/node-pty');
const {Terminal} = require('@xterm/headless');
const [binary, profile, session, directory, capture] = process.argv.slice(2);
const backend = process.env.GRAPT_CONPTY_BACKEND || 'system';
if (!['system', 'modern'].includes(backend)) throw Error('unknown ConPTY backend');
const terminal = new Terminal({cols:160, rows:45, allowProposedApi:true, scrollback:200});
const child = pty.spawn(binary, ['--service', profile, '--session', session, '--directory', directory],
    {name:'xterm-256color', cols:160, rows:45, cwd:profile, env:process.env,
        useConpty:true, useConptyDll:backend === 'modern'});
const raw = fs.createWriteStream(capture, {flags:'wx'});
let bytes=0, ended=null, input='', requests=0, finishing=false, started=false;
child.onData(data => {
    // node-pty publishes its Windows PID asynchronously before the first data.
    if (!started) {
        if (!Number.isInteger(child.pid) || child.pid <= 0)
            throw Error('ConPTY data arrived without a native process identity');
        started=true;
        console.log(JSON.stringify({started:true,pid:child.pid,transport:'Windows ConPTY',backend,cols:160,rows:45}));
    }
    bytes += Buffer.byteLength(data);
    if (bytes > 32*1024*1024) {
        child.kill();
        throw Error('ConPTY output exceeded 32 MiB');
    }
    raw.write(data);
    terminal.write(data);
});
child.onExit(event => { ended=event; });
function screen() {
    const rows=[], buffer=terminal.buffer.active;
    for (let y=0; y<terminal.rows; y++)
        rows.push(buffer.getLine(buffer.viewportY+y)?.translateToString(true)||'');
    return rows.join('\n');
}
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function execute(request) {
    if (++requests > 4096) throw Error('ConPTY control count exceeded 4096');
    if (request.action === 'send') {
        if (typeof request.text !== 'string' || request.text.length > 65536)
            throw Error('invalid terminal input');
        child.write(request.text);
        return {sent:true};
    }
    if (request.action === 'expect') {
        if (typeof request.text !== 'string' || request.text.length > 8192)
            throw Error('invalid expected text');
        for (let i=0; i<400; i++) {
            if (screen().includes(request.text)) return {screen:screen()};
            if (ended) throw Error('ConPTY ended before expected text: '+JSON.stringify(ended));
            await pause(50);
        }
        throw Error('ConPTY expected text deadline');
    }
    if (request.action === 'screen') {
        await new Promise(resolve => terminal.write('', resolve));
        return {screen:screen()};
    }
    if (request.action === 'exit') {
        for (let i=0; i<300 && !ended; i++) await pause(50);
        if (!ended || ended.exitCode !== 0) throw Error('ConPTY did not exit cleanly');
        finishing=true;
        return {exit:ended, pid:child.pid, terminal_bytes:bytes};
    }
    if (request.action === 'stop') {
        if (!ended) child.kill();
        finishing=true;
        return {stopped:true};
    }
    throw Error('unknown ConPTY action');
}
function finish() {
    terminal.dispose();
    raw.end(() => process.stdout.write('', () => process.exit(0)));
}
let queue=Promise.resolve();
process.stdin.setEncoding('utf8');
process.stdin.on('data', data => {
    input += data;
    if (input.length > 131072) throw Error('ConPTY control frame bound');
    let end;
    while ((end=input.indexOf('\n')) >= 0) {
        const row=input.slice(0,end);
        input=input.slice(end+1);
        queue=queue.then(async () => {
            try { console.log(JSON.stringify({...await execute(JSON.parse(row)),pid:child.pid})); }
            catch (error) { console.log(JSON.stringify({error:String(error),screen:screen()})); }
            if (finishing) finish();
        });
    }
});
process.stdin.on('end', () => queue.finally(() => {
    if (!ended) child.kill();
    finish();
}));
setTimeout(() => { if (!ended) child.kill(); process.exit(2); }, 900000).unref();
