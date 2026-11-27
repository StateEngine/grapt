"""Bounded receive-time capture of native JSONL and diagnostic lines."""
from contextlib import contextmanager
from datetime import datetime, timezone
import base64
import json
import os
import re
import stat
import threading


def redact_value(value, secrets=(), depth=0):
    """Redact decoded JSON, including object keys and escaped strings."""
    if depth > 64:
        raise RuntimeError('Native evidence JSON nesting exceeds its bound')
    if isinstance(value, str):
        for secret in secrets:
            value = value.replace(secret.decode('utf-8'), '[REDACTED]')
        return re.sub(r'sk-ant-[A-Za-z0-9_-]*', '[REDACTED]', value)
    if isinstance(value, list):
        return [redact_value(item, secrets, depth+1) for item in value]
    if isinstance(value, dict):
        return {redact_value(key, secrets, depth+1): redact_value(item, secrets, depth+1)
                for key, item in value.items()}
    return value


def redact_bytes(body, secrets=()):
    for secret in secrets:
        body = body.replace(secret, b'[REDACTED]')
    body = re.sub(rb'sk-ant-[A-Za-z0-9_-]*', b'[REDACTED]', body)
    try:
        decoded = json.loads(body)
    except (ValueError, UnicodeDecodeError):
        return body
    masked = redact_value(decoded, secrets)
    if masked != decoded:
        return json.dumps(masked).encode('utf-8') + (b'\n' if body.endswith(b'\n') else b'')
    return body


def retain_server_log(profile, target, limit, secrets=()):
    """Copy the product's file-backed stderr before its private profile is removed."""
    directory = profile / 'logs'
    try:
        metadata = directory.lstat()
    except FileNotFoundError:
        return {'state': 'absent'}
    if not stat.S_ISDIR(metadata.st_mode):
        raise RuntimeError('Server log directory is not a real directory')
    try:
        descriptor = os.open(directory / 'server.log', os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    except FileNotFoundError:
        return {'state': 'absent'}
    with os.fdopen(descriptor, 'rb') as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit:
            raise RuntimeError('Server log is not a bounded regular file')
        body = source.read(limit + 1)
    if len(body) > limit:
        raise RuntimeError('Server log exceeds its evidence bound')
    masked = b''.join(redact_bytes(line, secrets) for line in body.splitlines(keepends=True))
    if len(masked) > limit:
        raise RuntimeError('Redacted server log exceeds its evidence bound')
    descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'wb') as output:
        output.write(masked)
    return {'state': 'retained', 'path': target.name, 'bytes': len(masked)}


@contextmanager
def timestamped_log(path, limit, secrets=(), timestamp=True, redact=False):
    reader_fd, writer_fd = os.pipe()
    reader = os.fdopen(reader_fd, 'rb')
    writer = os.fdopen(writer_fd, 'wb')
    errors = []

    def receive():
        try:
            with reader, path.open('wb') as output:
                used = 0
                while True:
                    line = reader.readline(limit + 1)
                    if not line:
                        break
                    if len(line) > limit:
                        raise RuntimeError('Native log line exceeds evidence bound')
                    if secrets or redact:
                        line = redact_bytes(line, secrets)
                    if not timestamp:
                        if used + len(line) > limit:
                            raise RuntimeError('Native log exceeds evidence bound')
                        output.write(line)
                        output.flush()
                        used += len(line)
                        continue
                    row = {'received_at': datetime.now(timezone.utc).isoformat()}
                    try:
                        row['event'] = json.loads(line)
                    except (ValueError, UnicodeDecodeError):
                        # Preserve arbitrary stderr bytes, including partial final lines.
                        row['raw_base64'] = base64.b64encode(line).decode('ascii')
                    encoded = (json.dumps(row) + '\n').encode()
                    if used + len(encoded) > limit:
                        raise RuntimeError('Timestamped log exceeds evidence bound')
                    output.write(encoded)
                    output.flush()
                    used += len(encoded)
        except (OSError, RuntimeError) as error:
            errors.append(error)

    worker = threading.Thread(target=receive, daemon=True)
    try:
        worker.start()
        yield writer
    finally:
        writer.close()
        if worker.ident is None:
            reader.close()
        else:
            worker.join(timeout=5)
            if worker.is_alive():
                raise RuntimeError('Native log receiver did not stop after process cleanup')
            if errors:
                raise RuntimeError('Native log capture failed') from errors[0]
