"""Validate the shrink-only flaky quarantine in .config/nextest.toml.

The landing owner alone adds entries. Each one quarantines exactly one test for at most
seven days: a comment line naming owner, scorecard item and expiry, then an
override table that retries that single test once. The gate reads the same
file, so an invalid or expired entry refuses the whole run, and a quarantined
test fails the gate only when both attempts fail. The list shrinks to zero.
TOML parsers drop comments, so the raw text carries the annotations; the
parsed override inventory is then checked against the annotated blocks, so a
header in any other TOML spelling refuses instead of shrinking the
quarantine to an unannounced empty list.
"""
import re
import tomllib
from dataclasses import dataclass
from datetime import date, timedelta

MAX_ENTRIES = 64
MAX_HORIZON = timedelta(days=7)

OVERRIDE_HEADER='[[profile.gate.overrides]]'
# One key part of a header: a bare key or a quoted literal.
KEY_PART=r'''(?:"[^"]*"|'[^']*'|[A-Za-z0-9_-]+)'''
# Any single-line TOML spelling of the gate override header: indentation,
# inner and dotted whitespace, quoted or bare keys, a trailing comment. The
# byte-exact form stays the only spelling a quarantine entry may use.
HEADER_LINE=re.compile(r'^\s*\[\[\s*'
                       +KEY_PART+r'(\s*\.\s*'+KEY_PART+r'){2}\s*\]\]\s*(?:#.*)?$')
ENTRY_COMMENT=re.compile(r'^# flaky-quarantine: owner=([A-Za-z0-9][A-Za-z0-9_-]*) '
                         r'scorecard=(\S+) expiry=(\d{4}-\d{2}-\d{2})$')
FILTER=re.compile(r'^test\(=([A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*)\)$')
KEY=re.compile(r'^([A-Za-z0-9_-]+)\s*=\s*(.*)$')


@dataclass(frozen=True)
class Entry:
    owner: str
    scorecard: str
    expiry: date
    test: str
    retries: int


def _blocks(text):
    blocks=[]
    seen=set()
    header_indexes=set()
    lines=text.splitlines()
    index=0
    while index<len(lines):
        line=lines[index]
        if line==OVERRIDE_HEADER:
            raise ValueError('Gate override without a flaky-quarantine comment: '+line)
        match=ENTRY_COMMENT.match(line)
        if match is None:
            index+=1
            continue
        start=index
        index+=1
        if index>=len(lines) or lines[index]!=OVERRIDE_HEADER:
            raise ValueError('Flaky-quarantine comment needs an override table: '+line)
        header=index
        index+=1
        table={}
        while index<len(lines) and lines[index] and KEY.match(lines[index]):
            key,value=KEY.match(lines[index]).groups()
            value=value.strip()
            if len(value)>=2 and value[0]==value[-1] and value[0] in ('"',"'"):
                value=value[1:-1]
            table[key]=value
            index+=1
        if set(table)!={'filter','retries'}:
            raise ValueError('Quarantine table requires exactly filter and retries: '+line)
        filter_match=FILTER.match(table['filter'])
        if filter_match is None:
            raise ValueError('Quarantine filter must name one exact test: '+table['filter'])
        if table['retries']!='1':
            raise ValueError('Quarantine retries must be 1: '+table['retries'])
        try:
            expiry=date.fromisoformat(match.group(3))
        except ValueError:
            raise ValueError('Quarantine expiry is not a calendar date: '+match.group(3)) from None
        test=filter_match.group(1)
        if test in seen:
            raise ValueError('Quarantine lists the same test twice: '+test)
        if len(blocks)>=MAX_ENTRIES:
            raise ValueError('Quarantine exceeds '+str(MAX_ENTRIES)+' entries')
        seen.add(test)
        header_indexes.add(header)
        blocks.append((Entry(match.group(1),match.group(2),expiry,test,1),
                       lines[start:index],header))
    _validate_inventory(text,lines,blocks,header_indexes)
    return blocks


def _validate_inventory(text,lines,blocks,header_indexes):
    """The parsed TOML must agree with the annotated blocks, exactly.

    tomllib accepts header spellings the raw scan cannot bind to an entry
    (indentation, inner whitespace, quoted keys, a trailing comment) and
    skips none of them. Any such line is an unannotated gate override and
    refuses the run. The parsed override list must also carry exactly the
    annotated filters with integer retries of 1, so a spelling the raw scan
    reads differently cannot silently change what is quarantined. The shape
    of each parsed row is the contract: exactly the two keys filter and
    retries. An extra key is a policy the annotated block does not own, and
    config_block would silently drop it, so the whole row is checked, not
    just the two values.
    """
    for index,line in enumerate(lines):
        if HEADER_LINE.match(line) and index not in header_indexes:
            raise ValueError('Gate override without a flaky-quarantine comment: '+line)
    try:
        parsed=tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError('Quarantine file is not valid TOML: '+str(error)) from None
    if not isinstance(parsed,dict):
        raise ValueError('Quarantine file is not a TOML table')
    profile=parsed.get('profile')
    gate=profile.get('gate') if isinstance(profile,dict) else None
    overrides=gate.get('overrides') if isinstance(gate,dict) else None
    if overrides is None:
        overrides=[]
    if not isinstance(overrides,list) or not all(isinstance(row,dict) for row in overrides):
        raise ValueError('Gate overrides must be an array of tables')
    if len(overrides)!=len(blocks):
        raise ValueError('Override inventory mismatch: TOML sees '+str(len(overrides))+
                         ' gate overrides, the quarantine annotates '+str(len(blocks)))
    for row,entry in zip(overrides,blocks):
        if set(row)!={'filter','retries'}:
            extra=sorted(set(row)-{'filter','retries'})
            missing=sorted({'filter','retries'}-set(row))
            raise ValueError('Parsed override for '+entry[0].test+' must carry exactly '+
                             'filter and retries: extra '+repr(extra)+', missing '+repr(missing))
        if row['filter']!='test(='+entry[0].test+')':
            raise ValueError('Override filter does not match its quarantine entry: '+
                             entry[0].test)
        if row['retries']!=1:
            raise ValueError('Quarantine retries must be 1: '+repr(row['retries']))


def parse_entries(text):
    """Return the validated entries; raise ValueError on any structural break."""
    return [entry for entry,_,_ in _blocks(text)]


def problems(text,today=None):
    """Expiry policy for a parseable file: past expiry or beyond the 7-day horizon."""
    entries=parse_entries(text)
    today=today or date.today()
    issues=[]
    for entry in entries:
        if entry.expiry<today:
            issues.append(entry.test+': quarantine expired on '+entry.expiry.isoformat())
        elif entry.expiry>today+MAX_HORIZON:
            issues.append(entry.test+': quarantine horizon exceeds 7 days (expiry '+
                          entry.expiry.isoformat()+')')
    return issues


def config_block(text):
    """Verbatim entry lines (comment plus table) for embedding in a gate config."""
    blocks=_blocks(text)
    if not blocks:
        return ''
    lines=[]
    for _,entry_lines,_ in blocks:
        if lines:
            lines.append('')
        lines.extend(entry_lines)
    return '\n'.join(lines)+'\n'


def quarantined_tests(text):
    """Exact test paths under quarantine, for classifying gate JUnit failures."""
    return frozenset(entry.test for entry in parse_entries(text))
