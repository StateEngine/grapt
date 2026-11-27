"""Shared hidden creation policy for each owned native-test interpreter."""
import asyncio
import os
from pathlib import Path
import subprocess
import sys

RUNNER = str(Path(__file__).with_name('hidden_runner.py').resolve())
NO_WINDOW = 0x08000000
INSTALLED = False


def command(arguments):
    if isinstance(arguments, (str, bytes)):
        raise ValueError('Owned hidden commands require an argument vector')
    values = [os.fspath(value) for value in arguments]
    if os.path.normcase(os.path.abspath(values[0])) != os.path.normcase(sys.executable):
        return values
    position = 1
    while position < len(values) and values[position] in ('-I', '-B'):
        position += 1
    if position >= len(values):
        raise ValueError('Owned Python child must name its script')
    script = values[position]
    if os.path.normcase(os.path.abspath(script)) == os.path.normcase(RUNNER):
        return values
    if not os.path.isabs(script) or not script.endswith('.py'):
        raise ValueError('Owned Python child requires an absolute script; refuse uncovered launch')
    return [values[0], '-I', '-B', RUNNER, *values[position:]]


def options(values):
    result = dict(values)
    result['creationflags'] = result.get('creationflags', 0) | NO_WINDOW
    return result


def install():
    global INSTALLED
    if INSTALLED:
        return
    if os.name != 'nt':
        raise RuntimeError('Hidden creation policy requires Windows')
    original = subprocess.Popen
    class HiddenProcess(original):
        def __init__(self, arguments, *positional, **keywords):
            super().__init__(command(arguments), *positional, **options(keywords))
    subprocess.Popen = HiddenProcess
    original_exec = asyncio.create_subprocess_exec
    async def hidden_exec(program, *arguments, **keywords):
        return await original_exec(*command([program, *arguments]), **options(keywords))
    asyncio.create_subprocess_exec = hidden_exec
    original_shell = asyncio.create_subprocess_shell
    async def hidden_shell(script, **keywords):
        return await original_shell(script, **options(keywords))
    asyncio.create_subprocess_shell = hidden_shell
    INSTALLED = True
