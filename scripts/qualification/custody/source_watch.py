"""Fail-closed filesystem event watch for source roots during qualification.

The caller starts this after its intentional mutation, before any build or
execution. A restored edit still leaves an OS event. Build output and receipts
must live outside the watched source root.
"""

import ctypes
import os
import select
import sys
import threading
from pathlib import Path


IGNORED = {".git", "__pycache__"}


class SourceChanged(RuntimeError):
    pass


class SourceWatch:
    def __init__(self, root):
        self.root = Path(root).resolve(strict=True)
        self.changed = threading.Event()
        self.stopping = threading.Event()
        self.error = None
        self.handle = None
        self.thread = None

    def __enter__(self):
        if sys.platform.startswith("linux"):
            self._linux_start()
        elif os.name == "nt":
            self._windows_start()
        else:
            raise SourceChanged(f"source watch unsupported on {sys.platform}")
        return self

    def __exit__(self, *_unused):
        self.stopping.set()
        if sys.platform.startswith("linux"):
            # Drain queued events before closing: an edit restored just before
            # process exit may be invisible to a final Git status snapshot.
            try:
                while select.select([self.handle], [], [], 0)[0]:
                    if os.read(self.handle, 65536):
                        self.changed.set()
            except OSError as error:
                self.error = str(error)
            os.close(self.handle)
        else:
            self.kernel.CancelIoEx(self.handle, None)
            self.kernel.CloseHandle(self.handle)
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            self.error = "source watcher did not stop"

    def assert_unchanged(self):
        if self.error:
            raise SourceChanged(f"source watch failed: {self.error}")
        if self.changed.is_set():
            raise SourceChanged(f"source changed during run: {self.root}")

    def _linux_start(self):
        libc = ctypes.CDLL(None, use_errno=True)
        libc.inotify_init1.argtypes = [ctypes.c_int]
        libc.inotify_init1.restype = ctypes.c_int
        libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
        libc.inotify_add_watch.restype = ctypes.c_int
        self.handle = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
        if self.handle < 0:
            raise SourceChanged(f"inotify_init1 failed: errno {ctypes.get_errno()}")
        # Modify, close-write, attributes, create/delete and move events.
        mask = 0x2 | 0x4 | 0x8 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800
        count = 0
        try:
            for directory, dirs, _files in os.walk(self.root):
                dirs[:] = [name for name in dirs if name not in IGNORED]
                if libc.inotify_add_watch(self.handle, os.fsencode(directory), mask) < 0:
                    raise SourceChanged(
                        f"inotify_add_watch failed for {directory}: errno {ctypes.get_errno()}"
                    )
                count += 1
            if not count:
                raise SourceChanged("no source directories watched")
        except Exception:
            os.close(self.handle)
            raise
        self.thread = threading.Thread(target=self._linux_loop, daemon=True)
        self.thread.start()

    def _linux_loop(self):
        try:
            while not self.stopping.is_set():
                if select.select([self.handle], [], [], 0.1)[0]:
                    if os.read(self.handle, 65536):
                        self.changed.set()
        except OSError as error:
            if not self.stopping.is_set():
                self.error = str(error)

    def _windows_start(self):
        from ctypes import wintypes

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel = kernel
        kernel.CreateFileW.argtypes = [
            wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
            wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
        ]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.ReadDirectoryChangesW.argtypes = [
            wintypes.HANDLE, wintypes.LPVOID, wintypes.DWORD, wintypes.BOOL,
            wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), wintypes.LPVOID,
            wintypes.LPVOID,
        ]
        kernel.ReadDirectoryChangesW.restype = wintypes.BOOL
        kernel.CancelIoEx.argtypes = [wintypes.HANDLE, wintypes.LPVOID]
        kernel.CancelIoEx.restype = wintypes.BOOL
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        # FILE_LIST_DIRECTORY, share all, OPEN_EXISTING, BACKUP_SEMANTICS.
        self.handle = kernel.CreateFileW(str(self.root), 1, 7, None, 3, 0x02000000, None)
        if self.handle == ctypes.c_void_p(-1).value:
            raise SourceChanged(f"CreateFileW source watch failed: {ctypes.get_last_error()}")
        self.thread = threading.Thread(target=self._windows_loop, daemon=True)
        self.thread.start()

    def _windows_loop(self):
        from ctypes import wintypes

        kernel = self.kernel
        buffer = ctypes.create_string_buffer(65536)
        count = wintypes.DWORD()
        flags = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20
        while not self.stopping.is_set():
            ok = kernel.ReadDirectoryChangesW(
                self.handle, buffer, len(buffer), True, flags,
                ctypes.byref(count), None, None,
            )
            if not ok:
                if not self.stopping.is_set():
                    self.error = f"ReadDirectoryChangesW failed: {ctypes.get_last_error()}"
                return
            if self.stopping.is_set():
                return
            if count.value == 0:
                self.error = "source event buffer overflow"
                return
            offset = 0
            while offset < count.value:
                next_offset = int.from_bytes(buffer[offset:offset + 4], "little")
                name_bytes = int.from_bytes(buffer[offset + 8:offset + 12], "little")
                name = buffer[offset + 12:offset + 12 + name_bytes].decode("utf-16-le")
                if name.split("\\", 1)[0].lower() not in IGNORED:
                    self.changed.set()
                if next_offset == 0:
                    break
                offset += next_offset
