"""Observe and type only into the canonical producer's verified private R console.

Uses console handles rather than desktop focus or global keyboard events.
The controller must already hold the bench kit's kill-on-close job and lease.
"""
import ctypes as C
from ctypes import wintypes as W
import os
from pathlib import Path

from headless_acceptance import require, wait


def matches(row, binary, profile, session):
    args = row.get('cmdline') or []
    expected = {'--service': str(profile), '--session': session}
    return (row.get('exe') and Path(row['exe']).resolve() == binary.resolve()
            and all(args.count(key) == 1 and args.index(key) + 1 < len(args)
                    and args[args.index(key) + 1] == value for key, value in expected.items()))


def job_members(job):
    class Members(C.Structure):
        _fields_ = [('assigned', W.DWORD), ('listed', W.DWORD), ('pids', C.c_size_t * 32)]
    kernel = C.WinDLL('kernel32', use_last_error=True)
    query = kernel.QueryInformationJobObject
    query.restype, query.argtypes = W.BOOL, [W.HANDLE, W.INT, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)]
    members, size = Members(), W.DWORD()
    if not query(job, 3, C.byref(members), C.sizeof(members), C.byref(size)):
        raise C.WinError(C.get_last_error())
    require(0 < members.assigned <= 32 and members.listed == members.assigned,
            'owned job census unavailable or truncated')
    return list(members.pids[:members.listed])


def release_empty_controller(job, release):
    wait(lambda: job_members(job) == [os.getpid()], 'owned job contains only its controller', seconds=5)
    release()


class Coord(C.Structure):
    _fields_ = [('x', C.c_short), ('y', C.c_short)]


class Rect(C.Structure):
    _fields_ = [(name, C.c_short) for name in ('left', 'top', 'right', 'bottom')]


class BufferInfo(C.Structure):
    _fields_ = [('size', Coord), ('cursor', Coord), ('attributes', W.WORD),
                ('window', Rect), ('maximum', Coord)]


class Key(C.Structure):
    _fields_ = [('down', W.BOOL), ('repeat', W.WORD), ('virtual', W.WORD),
                ('scan', W.WORD), ('character', W.WCHAR), ('control', W.DWORD)]


class Event(C.Union):
    _fields_ = [('key', Key), ('padding', C.c_byte * 16)]


class Input(C.Structure):
    _fields_ = [('kind', W.WORD), ('event', Event)]


class Console:
    def __init__(self, binary, profile, session, job):
        require(os.name == 'nt' and job, 'Windows console requires the held controller job')
        import psutil
        self.handles, self.attached = [], False
        self.k = C.WinDLL('kernel32', use_last_error=True)
        self.open_process = self.api('OpenProcess', W.HANDLE, W.DWORD, W.BOOL, W.DWORD)
        self.close_handle = self.api('CloseHandle', W.BOOL, W.HANDLE)
        self.in_job = self.api('IsProcessInJob', W.BOOL, W.HANDLE, W.HANDLE, C.POINTER(W.BOOL))
        self.wait_process = self.api('WaitForSingleObject', W.DWORD, W.HANDLE, W.DWORD)
        self.terminate = self.api('TerminateProcess', W.BOOL, W.HANDLE, W.UINT)

        def find():
            rows = [p.info for p in psutil.process_iter(['pid', 'exe', 'cmdline'])
                    if matches(p.info, binary, profile, session)]
            require(len(rows) <= 1, 'multiple R processes match the private Session')
            return rows[0] if rows else None

        row = wait(find, 'canonical private R process', seconds=20)
        self.pid = row['pid']
        self.process = self.checked(self.open_process(0x100000 | 0x1000 | 1, False, self.pid))
        self.handles.append(self.process)
        try:
            # The retained handle pins the process identity through cleanup.
            require(matches(psutil.Process(self.pid).as_dict(['exe', 'cmdline']), binary, profile, session),
                    'R identity changed before opening its process handle')
            member = W.BOOL()
            self.checked(self.in_job(self.process, job, C.byref(member)))
            require(member.value, 'canonical R escaped the controller kill-on-close job')
            self.api('FreeConsole', W.BOOL)()
            self.checked(self.api('AttachConsole', W.BOOL, W.DWORD)(self.pid))
            self.attached = True
            self.window = self.checked(self.api('GetConsoleWindow', W.HWND)())
            pids = (W.DWORD * 32)()
            count = self.api('GetConsoleProcessList', W.DWORD, C.POINTER(W.DWORD), W.DWORD)(pids, 32)
            require(0 < count <= 32 and self.pid in pids[:count], 'private R console identity absent')
            for pid in pids[:count]:
                handle = self.checked(self.open_process(0x1000, False, pid))
                try:
                    self.checked(self.in_job(handle, job, C.byref(member)))
                    require(member.value, 'private console contains a process outside the owned job')
                finally:
                    self.close_handle(handle)
            self.checked(self.api('SetConsoleCtrlHandler', W.BOOL, C.c_void_p, W.BOOL)(None, True))
            create = self.api('CreateFileW', W.HANDLE, W.LPCWSTR, W.DWORD, W.DWORD,
                              C.c_void_p, W.DWORD, W.DWORD, W.HANDLE)
            self.input = self.checked(create('CONIN$', 0x40000000, 3, None, 3, 0, None))
            self.handles.append(self.input)
            self.output = self.checked(create('CONOUT$', 0x80000000, 3, None, 3, 0, None))
            self.handles.append(self.output)
        except BaseException:
            self.close()
            raise

    def api(self, name, result, *arguments):
        function = getattr(self.k, name)
        function.restype, function.argtypes = result, list(arguments)
        return function

    @staticmethod
    def checked(value):
        if not value or value == C.c_void_p(-1).value:
            raise C.WinError(C.get_last_error())
        return value

    def alive(self):
        return self.wait_process(self.process, 0) == 258

    def screen(self):
        require(self.alive(), 'original R process exited')
        info = BufferInfo()
        self.checked(self.api('GetConsoleScreenBufferInfo', W.BOOL, W.HANDLE,
            C.POINTER(BufferInfo))(self.output, C.byref(info)))
        width, height = info.size.x, info.window.bottom - info.window.top + 1
        require(0 < width <= 512 and 0 < height <= 200, 'console viewport bound')
        size, read = width * height, W.DWORD()
        text = C.create_unicode_buffer(size)
        self.checked(self.api('ReadConsoleOutputCharacterW', W.BOOL, W.HANDLE, W.LPWSTR,
            W.DWORD, Coord, C.POINTER(W.DWORD))(self.output, text, size,
            Coord(0, info.window.top), C.byref(read)))
        require(read.value == size, 'console viewport read truncated')
        return '\n'.join(text.value[index:index + width].rstrip() for index in range(0, size, width))

    def expect(self, text):
        return wait(lambda: (screen if text in (screen := self.screen()) else None),
                    'original R viewport containing ' + text, seconds=20)

    def send(self, text):
        require(self.alive() and 0 < len(text) <= 4096 and text.isascii(), 'console input bound')
        records = (Input * (len(text) * 2))()
        for index, character in enumerate(text):
            for offset, down in enumerate((True, False)):
                record = records[2 * index + offset]
                record.kind = 1
                record.event.key = Key(down, 1, 13 if character == '\r' else 0, 0, character, 0)
        written = W.DWORD()
        self.checked(self.api('WriteConsoleInputW', W.BOOL, W.HANDLE, C.POINTER(Input), W.DWORD,
            C.POINTER(W.DWORD))(self.input, records, len(records), C.byref(written)))
        require(written.value == len(records), 'console input was partially delivered')

    def stop(self):
        if self.alive():
            self.checked(self.terminate(self.process, 1))
        require(self.wait_process(self.process, 5000) == 0, 'owned R did not stop')

    def close(self):
        for handle in reversed(self.handles):
            self.close_handle(handle)
        self.handles.clear()
        if self.attached:
            self.api('FreeConsole', W.BOOL)()
            self.attached = False

    def window_closed(self):
        user = C.WinDLL('user32', use_last_error=True)
        user.IsWindow.restype, user.IsWindow.argtypes = W.BOOL, [W.HWND]
        return not user.IsWindow(self.window)
