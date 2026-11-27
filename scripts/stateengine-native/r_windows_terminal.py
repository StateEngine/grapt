"""Native ConPTY adapter for the existing R composer and approval journey."""
import json
import os
from pathlib import Path
import queue
import subprocess
import threading

from acceptance_peer import CAPABILITY, PRIVATE_CURSOR
from headless_acceptance import SESSION, require
from r_sustained_terminal import RTerminal

KEYS = {'Enter':'\r', 'BSpace':'\x7f', 'Escape':'\x1b', 'C-c':'\x03',
        'C-q':'\x11', 'Up':'\x1b[A', 'Down':'\x1b[B', 'Left':'\x1b[D', 'Right':'\x1b[C'}


def native_session(started):
    require(started.get('started') and started.get('transport') == 'Windows ConPTY',
            'observer did not start a real ConPTY')
    pid = started.get('pid')
    require(type(pid) is int and pid > 0, 'observer omitted the native R process identity')
    return str(pid)


class WindowsTerminal(RTerminal):
    def start(self, index, prompt=None, directory=None):
        require(os.name == 'nt' and prompt is None, 'Windows ConPTY adapter requires native Windows')
        environment = dict(self.journey.environment, NODE_PATH=str(self.args.node_modules))
        self.replies = queue.Queue(maxsize=2)
        self.errors = (self.journey.root / f'conpty-{index}.stderr').open('xb')
        self.observer = subprocess.Popen([self.args.node, str(Path(__file__).with_name('r_conpty.cjs')),
            str(self.args.tui), str(self.journey.profile), SESSION,
            str(directory or self.journey.profile), str(self.journey.root / f'conpty-{index}.raw')],
            env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors)
        self.session='starting'

        def read():
            while True:
                row = self.observer.stdout.readline(65537)
                try:
                    self.replies.put(row, timeout=5)
                except queue.Full:
                    return
                if not row or len(row)>65536:
                    return
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        started = self.reply()
        self.session = native_session(started)
        self.args.record('ConPTY start', {'argv':self.observer.args, 'pid':started['pid']})
        self.expect('enter send')
        self.expect('gpt-live')
        self.expect('Connected')
        self.capture('connected-'+str(index))

    def reply(self):
        try:
            raw = self.replies.get(timeout=30)
        except queue.Empty as error:
            raise RuntimeError('ConPTY observer reply deadline') from error
        require(raw and len(raw)<=65536, 'ConPTY observer ended or exceeded its frame bound')
        value = json.loads(raw)
        require(CAPABILITY not in raw.decode() and PRIVATE_CURSOR not in raw.decode(),
                'private binding reached terminal output')
        require('error' not in value, 'ConPTY observer refused: '+str(value))
        return value

    def request(self, action, **fields):
        self.args.record('ConPTY', dict(action=action, **fields))
        self.observer.stdin.write((json.dumps(dict(action=action, **fields))+'\n').encode())
        self.observer.stdin.flush()
        value = self.reply()
        require(value.get('pid') == int(self.session), 'ConPTY replaced the original R process')
        return value

    def drive(self, operation, *arguments):
        if operation == 'capture':
            return self.request('screen')['screen']
        if operation == 'wait':
            require(len(arguments)==4 and arguments[0]=='--text' and arguments[2]=='--timeout-ms',
                    'unsupported ConPTY wait')
            return self.request('expect', text=arguments[1])['screen']
        if operation == 'send':
            if arguments and arguments[0]=='--text':
                require(len(arguments)==2, 'unsupported literal ConPTY input')
                text=arguments[1]
            else:
                require(all(key in KEYS or len(key)==1 and key.isprintable() for key in arguments),
                        'unsupported ConPTY key')
                text=''.join(KEYS.get(key, key) for key in arguments)
            self.request('send', text=text)
            return ''
        if operation == 'stop':
            self.request('stop')
            self.dispose()
            return ''
        raise RuntimeError('unsupported ConPTY operation: '+operation)

    def dispose(self):
        try:
            self.observer.stdin.close()
        except OSError:
            pass
        try:
            self.observer.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.observer.kill()
            self.observer.wait(timeout=5)
        self.reader.join(timeout=5)
        self.observer.stdout.close()
        self.errors.close()
        require(not self.reader.is_alive(), 'ConPTY reply reader did not join')
        self.session=None

    def close(self):
        self.drive('send', 'C-c')
        self.expect('Press Ctrl+C again to exit')
        self.drive('send', 'C-c')
        self.request('exit')
        self.dispose()

    def cleanup(self):
        if self.session:
            try:
                self.drive('stop')
            finally:
                if self.session:
                    self.dispose()
