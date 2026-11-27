"""Bounded, disposable product transport faults for the R startup journey."""
import select
import socket
import socketserver
import threading
from urllib.parse import urlsplit


class Proxy(socketserver.ThreadingTCPServer):
    daemon_threads = True
    max_requests = 256

    def __init__(self, endpoint):
        target = urlsplit(endpoint)
        self.target = (target.hostname, target.port)
        self.entered, self.release = threading.Event(), threading.Event()
        self.delay = self.fail = self.lose_write = self.hide_history = False
        self.close_http_requests = False
        self.requests, self.errors = [], []
        self.lock = threading.Lock()
        self.slots = threading.BoundedSemaphore(8)
        super().__init__(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={'poll_interval': .05})

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            request.close()
            self.errors.append('proxy connection bound')
            return
        super().process_request(request, address)

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.release.set()
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError('proxy did not join')

    def faults(self, method, path):
        """Optional bounded scenario-specific faults: refuse, drop reply."""
        return False, False

    @property
    def endpoint(self):
        return f'http://127.0.0.1:{self.server_address[1]}'

    def hold(self):
        self.entered.clear()
        self.release.clear()
        self.delay = True


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            self.forward()
        except (OSError, ValueError, TimeoutError) as error:
            # Client shutdown closes an SSE bridge; only unexpected fixture
            # failures are retained. Socket errors otherwise cannot pass gates.
            if not isinstance(error, (BrokenPipeError, ConnectionResetError)):
                self.server.errors.append(type(error).__name__ + ': ' + str(error))

    def forward(self):
        self.request.settimeout(5)
        header = b''
        while b'\r\n\r\n' not in header:
            part = self.request.recv(4096)
            if not part:
                return
            header += part
            if len(header) > 16384:
                raise ValueError('proxy request header bound')
        method, path, _ = header.split(b'\r\n', 1)[0].decode('ascii').split(' ')
        metadata = method == 'GET' and path.startswith('/api/session/ses_') and '/' not in path[len('/api/session/'):]
        prompt = method == 'POST' and path.endswith('/prompt')
        with self.server.lock:
            if len(self.server.requests) >= self.server.max_requests:
                raise ValueError('proxy request bound')
            self.server.requests.append((method, path))
            delay = metadata and self.server.delay
            fail = metadata and self.server.fail
            drop = prompt and self.server.lose_write
            if delay: self.server.delay = False
            if fail: self.server.fail = False
            if drop: self.server.lose_write = False
            extra_fail, extra_drop = self.server.faults(method, path)
            fail, drop = fail or extra_fail, drop or extra_drop
            hidden = (self.server.hide_history and not self.server.lose_write
                      and ('/message?' in path or path.endswith('/pending')))
        if delay:
            self.server.entered.set()
            if not self.server.release.wait(timeout=15):
                raise TimeoutError('startup read was not released')
        if fail or hidden:
            self.request.sendall(b'HTTP/1.1 503 Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n')
            return
        if self.server.close_http_requests:
            head, body = header.split(b'\r\n\r\n', 1)
            lines = [line for line in head.split(b'\r\n')
                     if not line.lower().startswith(b'connection:')]
            header = b'\r\n'.join([*lines, b'Connection: close']) + b'\r\n\r\n' + body
        with socket.create_connection(self.server.target, timeout=5) as product:
            product.sendall(header)
            # One ordinary HTTP connection; SSE bytes stream without buffering
            # an unbounded body. Both sockets and each transfer are bounded.
            for _ in range(8192):
                readable, _, _ = select.select([self.request, product], [], [], 30)
                if not readable:
                    return
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        return
                    if source is product:
                        if drop:
                            return
                        self.request.sendall(data)
                    else:
                        product.sendall(data)
            raise ValueError('proxy transfer bound')
