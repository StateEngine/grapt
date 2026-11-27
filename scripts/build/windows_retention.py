#!/usr/bin/env python3
"""Expire completed native SE Code temporary runs after seven days."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import time
import operator_paths

ROOT = Path(os.environ.get('SECODE_WINDOWS_GATE_ROOT', Path(os.environ.get('LOCALAPPDATA', Path.home()))/'secode-native-gate'))/'runs'
PC_LOCK = ROOT.parent/'.retention.lock'
RETENTION = 7*86400
MAX_ENTRIES = 65536


def identity(info):
    return (info.st_dev,info.st_ino,info.st_mode,info.st_size,info.st_mtime_ns,info.st_ctime_ns)


def inventory(root, now, deadline):
    """Return a stable old tree; retain links, young files and explicit holds."""
    pending=[root]; result={}
    while pending:
        path=pending.pop()
        if time.monotonic()>=deadline or len(result)>=MAX_ENTRIES:
            raise RuntimeError('PC retention inventory exceeded its entry or time bound')
        info=path.lstat()
        if (stat.S_ISLNK(info.st_mode) or getattr(info,'st_file_attributes',0)&0x400
                or info.st_dev!=root.lstat().st_dev or now-info.st_mtime<RETENTION
                or path.name=='.secode-retain'):
            return None
        if path.name.lower()=='.git' or (stat.S_ISDIR(info.st_mode)
                and (path/'HEAD').is_file() and (path/'objects').is_dir()):
            return None  # Repositories and worktrees require their Git removal path.
        result[path.relative_to(root).as_posix()]=identity(info)
        if stat.S_ISDIR(info.st_mode):
            with os.scandir(path) as entries:
                for entry in entries:
                    if len(pending)+len(result)>=MAX_ENTRIES or time.monotonic()>=deadline:
                        raise RuntimeError('PC retention inventory exceeded its entry or time bound')
                    pending.append(Path(entry.path))
        elif not stat.S_ISREG(info.st_mode):
            return None
    return result


def completed(path):
    marker=path/'done.json'
    try:
        info=marker.lstat()
    except FileNotFoundError:
        return False
    if not stat.S_ISREG(info.st_mode) or getattr(info,'st_file_attributes',0)&0x400 or info.st_size>128:
        return False
    with marker.open('rb') as source:
        raw=source.read(129)
    if len(raw)>128:
        return False
    row=json.loads(raw)
    return isinstance(row,dict) and set(row)=={'exit'} and type(row['exit']) is int and 0<=row['exit']<=255


def reclaim(root, now, deadline, apply=False):
    if not root.exists():
        return
    info=root.lstat()
    if not stat.S_ISDIR(info.st_mode) or getattr(info,'st_file_attributes',0)&0x400 or root.is_symlink():
        raise ValueError('Native temporary root must be a real directory')
    if (root/'.secode-retain').exists():
        return
    with os.scandir(root) as entries:
        names=[]
        for index,entry in enumerate(entries):
            if index>=16384 or time.monotonic()>=deadline:
                raise RuntimeError('Native temporary root exceeds its inventory bound')
            if re.fullmatch('[0-9a-f]{32}',entry.name):
                names.append(entry.name)
    for name in names:
        path=root/name
        if not completed(path):
            continue
        before=inventory(path,now,deadline)
        if before is None:
            continue
        print(('Removing' if apply else 'Would remove')+' completed native temporary run '+str(path),flush=True)
        if not apply:
            continue
        if not completed(path) or inventory(path,now,deadline)!=before:
            continue
        retired=root/('.retiring-'+name)
        if retired.exists() or retired.is_symlink():
            raise ValueError('Preserve an unfinished native retention attempt')
        path.rename(retired)
        # Renaming changes the directory's ctime, so compare the remaining
        # identities separately while checking that its inode stayed ours.
        after=inventory(retired,now,deadline)
        if after is None or {k:v for k,v in after.items() if k!='.'}!={k:v for k,v in before.items() if k!='.'} or after['.'][:2]!=before['.'][:2]:
            raise ValueError('Native temporary run changed; preserve '+str(retired))
        shutil.rmtree(retired)


def namespace():
    directory=ROOT.parent
    directory.mkdir(parents=True,exist_ok=True)
    info=directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or getattr(info,'st_file_attributes',0)&0x400:
        raise ValueError('Native retention root must be a real directory')
    return directory


def main():
    if os.name!='nt':
        raise RuntimeError('PC retention must run natively on Windows')
    import msvcrt
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply',action='store_true',help='default is read-only planning')
    args=parser.parse_args()
    # A completion marker follows closure of the Windows worker job. The shared
    # retention lease also fences new workers and unconfirmed crash cleanup.
    namespace()
    with PC_LOCK.open('a+b') as lock:
        if lock.tell()==0:lock.write(b'0');lock.flush()
        lock.seek(0)
        try:msvcrt.locking(lock.fileno(),msvcrt.LK_NBLCK,1)
        except OSError as error:
            if error.errno not in (13,36):raise
            print('Native operator active; temporary runs preserved',flush=True)
            return
        if PC_LOCK.with_suffix('.active').exists():
            raise RuntimeError('Native cleanup is unconfirmed; temporary runs preserved')
        reclaim(ROOT,time.time(),time.monotonic()+120,args.apply)


def logged_main():
    import traceback
    log=namespace()/'retention.log'
    if log.is_symlink() or log.exists() and not log.is_file():
        raise ValueError('Preserve an unexpected PC retention log')
    if log.exists() and log.stat().st_size>1024**2:
        log.replace(log.with_suffix('.previous.log'))
    with log.open('a') as output, contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
        try:
            print(time.strftime('%Y-%m-%d %H:%M:%S UTC',time.gmtime()),flush=True)
            main()
        except BaseException:
            traceback.print_exc()
            raise


if __name__=='__main__':
    if os.name!='nt':
        main()
    else:
        logged_main()
