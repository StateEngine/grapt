"""Join only Microsoft-signed telemetry left inside this worker's MSVC job."""
import ctypes as C
from ctypes import wintypes as W
import json
import os
from pathlib import Path
import subprocess
import time

import windows_version_worker as job


def microsoft_signed(path):
    literal = str(path).replace("'", "''")
    script = ("$s=Get-AuthenticodeSignature -LiteralPath '"+literal+"';"
              "@{valid=($s.Status -eq 'Valid');subject=$s.SignerCertificate.Subject}|ConvertTo-Json -Compress")
    raw = subprocess.check_output(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                                  creationflags=subprocess.CREATE_NO_WINDOW, timeout=10)
    if len(raw) > 4096:
        raise ValueError('MSVC signature result exceeds its bound')
    row = json.loads(raw)
    return row.get('valid') is True and 'O=Microsoft Corporation' in row.get('subject', '').split(', ')


def join_telemetry(worker):
    """Hold each job-verified process handle from image check through final join."""
    root = worker.environment.get('VCTOOLSINSTALLDIR')
    if not root:
        raise RuntimeError('Native survivors have no installed MSVC toolchain identity')
    toolchain = Path(root).resolve(strict=True)
    kernel = C.WinDLL('kernel32', use_last_error=True)
    def function(name, result, arguments):
        value = getattr(kernel, name)
        value.restype, value.argtypes = result, arguments
        return value
    open_process = function('OpenProcess', W.HANDLE, [W.DWORD, W.BOOL, W.DWORD])
    in_job = function('IsProcessInJob', W.BOOL, [W.HANDLE, W.HANDLE, C.POINTER(W.BOOL)])
    image = function('QueryFullProcessImageNameW', W.BOOL, [W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD)])
    wait = function('WaitForSingleObject', W.DWORD, [W.HANDLE, W.DWORD])
    terminate = function('TerminateProcess', W.BOOL, [W.HANDLE, W.UINT])
    close = function('CloseHandle', W.BOOL, [W.HANDLE])
    handles, actions = [], []
    try:
        for pid in job.process_ids():
            if pid == os.getpid():
                continue
            worker.check()
            handle = open_process(0x100001 | 0x1000, False, pid)
            if not handle:
                if C.get_last_error() == 87:
                    continue  # Already exited; final job census still must be clean.
                raise C.WinError(C.get_last_error())
            handles.append((pid, handle, None))
            member = W.BOOL()
            if not in_job(handle, job._JOB, C.byref(member)) or not member.value:
                raise RuntimeError('Native survivor is no longer in the worker job')
            if wait(handle, 0) == 0:
                continue
            buffer, size = C.create_unicode_buffer(32768), W.DWORD(32768)
            if not image(handle, 0, buffer, C.byref(size)):
                raise C.WinError(C.get_last_error())
            path = Path(buffer.value).resolve(strict=True)
            if (path.name.lower() != 'vctip.exe' or not path.is_relative_to(toolchain/'bin')
                    or not microsoft_signed(path)):
                raise RuntimeError('Native tests left an unapproved owned descendant: '+str(pid)+' '+str(path))
            handles[-1] = (pid, handle, str(path))
        # Classify every live survivor before terminating any of them.
        deadline = min(worker.deadline, time.monotonic()+20)
        natural_deadline = min(deadline, time.monotonic()+10)
        for pid, handle, path in handles:
            if path is None:
                continue
            worker.check()
            remaining = max(0, int((natural_deadline-time.monotonic())*1000))
            status = wait(handle, remaining)
            action = 'exited'
            if status == 258:
                action = 'terminated'
                if not terminate(handle, 1):
                    error = C.get_last_error()
                    if error != 5:
                        raise C.WinError(error)
                    action = 'joined_exit_pending'
                status = wait(handle, max(0, int((deadline-time.monotonic())*1000)))
            if status != 0:
                raise RuntimeError('MSVC telemetry did not exit within the cleanup deadline')
            row = dict(pid=pid, path=path, action=action)
            actions.append(row)
            worker.log.write(('Native MSVC telemetry cleanup: '+json.dumps(row)+'\n').encode())
            worker.log.flush()
        if not job.children_clean():
            raise RuntimeError('Native tests still have owned descendants after telemetry cleanup')
        return actions
    finally:
        for _pid, handle, _path in handles:
            close(handle)
