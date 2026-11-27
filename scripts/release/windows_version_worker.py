"""Own a native version process tree before any payload instruction executes.

Adapts OpenAI Codex utils/pty/src/win/job.rs kill-on-close containment,
bb64b894fd67158be0f874872e6d7bfe717e7368 (Apache-2.0, Copyright 2025 OpenAI).
Modified for Python: join the worker before spawning, forbid breakaway, cap
the tree at a bounded process count, and retain its only job handle through exit.
"""
import ctypes as C
from ctypes import wintypes as W
import os
from pathlib import Path
import subprocess
import sys

_JOB = None


def contain(limit=8):
    global _JOB
    if os.name != 'nt':
        raise RuntimeError('Windows version worker requires Windows')
    if type(limit) is not int or not 2 <= limit <= 64 or _JOB is not None:
        raise ValueError('Windows worker requires one process bound in [2, 64]')
    class Basic(C.Structure):
        _fields_ = [('process_time', C.c_int64), ('job_time', C.c_int64),
                    ('flags', W.DWORD), ('min_ws', C.c_size_t), ('max_ws', C.c_size_t),
                    ('active', W.DWORD), ('affinity', C.c_size_t),
                    ('priority', W.DWORD), ('scheduling', W.DWORD)]
    class Limits(C.Structure):
        _fields_ = [('basic', Basic), ('io', C.c_uint64*6),
                    ('process_memory', C.c_size_t), ('job_memory', C.c_size_t),
                    ('peak_process', C.c_size_t), ('peak_job', C.c_size_t)]
    kernel = C.WinDLL('kernel32', use_last_error=True)
    def function(name, result, arguments):
        value = getattr(kernel, name)
        value.restype, value.argtypes = result, arguments
        return value
    create = function('CreateJobObjectW', W.HANDLE, [C.c_void_p, W.LPCWSTR])
    configure = function('SetInformationJobObject', W.BOOL,
                         [W.HANDLE, C.c_int, C.c_void_p, W.DWORD])
    query = function('QueryInformationJobObject', W.BOOL,
                     [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.c_void_p])
    assign = function('AssignProcessToJobObject', W.BOOL, [W.HANDLE, W.HANDLE])
    current = function('GetCurrentProcess', W.HANDLE, [])
    close = function('CloseHandle', W.BOOL, [W.HANDLE])
    job = create(None, None)
    if not job:
        raise C.WinError(C.get_last_error())
    try:
        limits = Limits()
        limits.basic.flags, limits.basic.active = 0x2000 | 0x8, limit
        if not configure(job, 9, C.byref(limits), C.sizeof(limits)):
            raise C.WinError(C.get_last_error())
        actual = Limits()
        if (not query(job, 9, C.byref(actual), C.sizeof(actual), None)
                or actual.basic.flags != limits.basic.flags or actual.basic.active != limit):
            raise RuntimeError('version process containment was not configured')
        if not assign(job, current()):
            raise C.WinError(C.get_last_error())
    except BaseException:
        close(job)
        raise
    # Closing this handle while the worker runs would terminate the worker too.
    # OS process teardown closes it after the exit status is established, killing
    # descendants even when their entrypoint already exited or holds stdout.
    _JOB = job


def process_ids():
    """Read only this adapter's bounded kernel-owned job membership."""
    if _JOB is None:
        raise RuntimeError('Windows worker has no owned process job')
    class Processes(C.Structure):
        _fields_ = [('assigned', W.DWORD), ('listed', W.DWORD), ('ids', C.c_size_t*64)]
    kernel = C.WinDLL('kernel32', use_last_error=True)
    query = kernel.QueryInformationJobObject
    query.restype = W.BOOL
    query.argtypes = [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.c_void_p]
    row = Processes()
    if not query(_JOB, 3, C.byref(row), C.sizeof(row), None):
        raise C.WinError(C.get_last_error())
    if row.assigned != row.listed or row.listed > 64:
        raise RuntimeError('Windows worker job membership exceeds its bound')
    return list(row.ids[:row.listed])


def children_clean():
    return process_ids() == [os.getpid()]


def stop_descendants(timeout=30):
    """Terminate and join owned descendants while the caller retains its lease.

    TerminateProcess only requests termination. Wait on the same opened handle,
    and recheck job membership so PID reuse can never target a foreign process.
    """
    import time
    kernel = C.WinDLL('kernel32', use_last_error=True)
    def function(name, result, arguments):
        value = getattr(kernel, name)
        value.restype, value.argtypes = result, arguments
        return value
    open_process = function('OpenProcess', W.HANDLE, [W.DWORD, W.BOOL, W.DWORD])
    in_job = function('IsProcessInJob', W.BOOL, [W.HANDLE, W.HANDLE, C.POINTER(W.BOOL)])
    terminate = function('TerminateProcess', W.BOOL, [W.HANDLE, W.UINT])
    wait = function('WaitForSingleObject', W.DWORD, [W.HANDLE, W.DWORD])
    close = function('CloseHandle', W.BOOL, [W.HANDLE])
    deadline = time.monotonic()+timeout
    while not children_clean():
        handles = []
        try:
            for pid in process_ids():
                if pid == os.getpid():
                    continue
                handle = open_process(0x100001 | 0x1000, False, pid)
                if not handle:
                    if C.get_last_error() == 87:  # Exited before the handle opened.
                        continue
                    raise C.WinError(C.get_last_error())
                member = W.BOOL()
                if not in_job(handle, _JOB, C.byref(member)):
                    close(handle)
                    raise C.WinError(C.get_last_error())
                if not member.value:
                    close(handle)  # PID was reused; this handle is not ours.
                    continue
                handles.append(handle)
                if not terminate(handle, 1):
                    error = C.get_last_error()
                    # A process already in exit can deny termination while its
                    # pending I/O is still draining. Join the verified handle;
                    # a zero-time probe does not establish a cleanup failure.
                    remaining = max(0, int((deadline-time.monotonic())*1000))
                    if error != 5 or wait(handle, remaining) != 0:
                        failure = C.WinError(error)
                        failure.add_note('Native owned descendant PID '+str(pid))
                        raise failure
            for handle in handles:
                remaining = max(0, int((deadline-time.monotonic())*1000))
                if wait(handle, remaining) != 0:
                    raise RuntimeError('Native owned-child exit was not joined before the cleanup deadline')
            if time.monotonic() >= deadline and not children_clean():
                raise RuntimeError('Native owned-child cleanup exceeded its deadline')
        finally:
            for handle in handles:
                close(handle)


if __name__ == '__main__':
    if len(sys.argv) != 2 or not Path(sys.argv[1]).is_absolute():
        raise SystemExit('version worker requires one absolute entrypoint')
    contain()
    raise SystemExit(subprocess.call([sys.argv[1], '--version'], stdin=subprocess.DEVNULL))
