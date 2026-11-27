"""Observe serial libtest elapsed time while preserving its ordinary output."""
import os
import codecs
import re
import selectors
import subprocess
import time
import heavy

MAX_LINE = 65536


class Observer:
    def __init__(self, output, clock=time.monotonic):
        self.output = output
        self.clock = clock
        self.line = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
        self.binary = None
        self.started = None
        self.name = None
        self.records = []

    def feed(self, raw):
        text = self.line + self.decoder.decode(raw)
        lines = text.split('\n')
        for line in lines[:-1]:
            self.inspect(line, complete=True)
        self.line = lines[-1]
        if len(self.line) > MAX_LINE:
            self.line = ''
        else:
            self.inspect(self.line, complete=False)

    def inspect(self, line, *, complete):
        run = re.fullmatch(r'\s*Running .+ \((.+)\)', line)
        if run:
            self.binary = run[1]
            self.started = self.name = None
        test = re.match(r'test ([A-Za-z0-9_:]+) \.\.\. ', line)
        if not test or not self.binary:
            return
        name = test[1]
        if self.name != name:
            self.name, self.started = name, self.clock()
        if complete:
            elapsed = self.clock() - self.started
            if elapsed >= 60:
                if len(self.records) < 256 and len(name) <= 1024:
                    self.records.append(f'# nightly-slow-test: {name} {elapsed:.3f}s ({self.binary})')
            self.started = self.name = None

    def finish(self):
        if self.records:
            self.output.write("\n" + "\n".join(self.records) + "\n")
            self.output.flush()


def wait(process, log, deadline, check=None):
    """Only serial test commands use this observer; timeout cleanup stays with gate."""
    log.flush()
    observer = Observer(log)
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while selector.get_map():
            heavy.check_hold_timeout()
            if check is not None:
                check()
            remaining = deadline-time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(process.args, 0)
            for key, _mask in selector.select(min(remaining, 1)):
                chunk = os.read(key.fd, 16384)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                log.buffer.write(chunk)
                log.buffer.flush()
                observer.feed(chunk)
    code = process.wait(timeout=max(0.001, deadline-time.monotonic()))
    if check is not None:
        check()
    observer.finish()
    return code
