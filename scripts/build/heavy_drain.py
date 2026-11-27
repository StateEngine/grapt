"""Bounded exclusive admission, hold declarations and retry credit."""
import json
import math
import os
from heavy_runs import metadata

MAX_BYTES = 256 * 1024
WAIT_SECONDS = 300
MAX_EXCLUSIVE_WAIT = 3600
MAX_HOLD_SECONDS = 1800
MAX_REMAINING_SECONDS = 2700
MAX_DRAIN_SECONDS = 2700
SHARED_WINDOW_SECONDS = 300


def seconds(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


class Drain:
    # All state access is protected by Queue.locked, beside the unchanged v1 rows.
    def __init__(self, directory, tickets):
        self.directory = directory
        self.path = directory / 'exclusive-drain.json'
        try:
            with metadata(self.path, os.O_RDONLY, 'r') as stream:
                raw = stream.read(MAX_BYTES + 1)
        except FileNotFoundError:
            raw = '{"version":1,"claims":{},"active":null,"deferred":[],"refunds":[],"cooldown_until":0,"yield_owner":null}'
        if len(raw) > MAX_BYTES:
            raise ValueError('Oversized exclusive drain state')
        self.original = raw
        self.state = json.loads(raw)
        state = self.state
        if (not isinstance(state, dict) or set(state) != {'version', 'claims', 'active', 'deferred', 'refunds', 'cooldown_until', 'yield_owner'}
                or state['version'] != 1 or not isinstance(state['claims'], dict)
                or len(state['claims']) > 1024 or not isinstance(state['deferred'], list)
                or len(state['deferred']) > 1024 or not isinstance(state['refunds'], list)
                or len(state['refunds']) > 128):
            raise ValueError('Invalid exclusive drain state')
        for key, row in state['claims'].items():
            if (not isinstance(key, str) or len(key) > 80 or not isinstance(row, dict)
                    or set(row) != {'max_hold', 'projected'} or not seconds(row['max_hold'])
                    or not 0 < row['max_hold'] <= MAX_HOLD_SECONDS
                    or not seconds(row['projected']) or not 0 < row['projected'] <= 43200):
                raise ValueError('Invalid heavy hold declaration')
        if not seconds(state['cooldown_until']) or (state['yield_owner'] is not None and (not isinstance(state['yield_owner'], str) or len(state['yield_owner']) > 80)):
            raise ValueError('Invalid exclusive shared-progress window')
        active = state['active']
        if active is not None and (not isinstance(active, dict)
                or set(active) != {'ticket', 'since', 'updated', 'idle_slot_seconds'}
                or not isinstance(active['ticket'], str) or len(active['ticket']) > 80
                or any(not seconds(active[name]) for name in ('since', 'updated', 'idle_slot_seconds'))):
            raise ValueError('Invalid active exclusive drain')
        if any(not isinstance(key, str) or len(key) > 80 for key in state['deferred']):
            raise ValueError('Invalid deferred exclusive drain')
        for row in state['refunds']:
            if (not isinstance(row, dict) or set(row) != {'ticket', 'owner', 'class', 'enqueued_ns'}
                    or not isinstance(row['ticket'], str) or len(row['ticket']) > 80
                    or not isinstance(row['owner'], str) or len(row['owner']) > 128
                    or type(row['class']) is not int or row['class'] not in (0, 1)
                    or type(row['enqueued_ns']) is not int or row['enqueued_ns'] <= 0):
                raise ValueError('Invalid exclusive retry credit')
        self.live = {row['id'] for row in tickets}
        state['claims'] = {key: row for key, row in state['claims'].items() if key in self.live}
        state['deferred'] = [key for key in state['deferred'] if key in self.live]

    def save(self):
        raw = json.dumps(self.state, separators=(',', ':'))
        if len(raw.encode()) > MAX_BYTES:
            raise ValueError('Exclusive drain state exceeds its bound')
        if raw == self.original:
            return
        staging = self.path.with_suffix('.next')
        with metadata(staging, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 'w') as stream:
            stream.write(raw)
            stream.flush()
        staging.replace(self.path)
        self.original = raw

    def event(self, event, **fields):
        path = self.directory / 'exclusive-drain.jsonl'
        try:
            size = path.stat().st_size
        except FileNotFoundError:
            size = 0
        if size >= MAX_BYTES:
            path.replace(self.directory / 'exclusive-drain.previous.jsonl')
        with metadata(path, os.O_CREAT | os.O_WRONLY | os.O_APPEND, 'a') as stream:
            stream.write(json.dumps({'event': event, **fields}, separators=(',', ':')) + '\n')

    def declare(self, ticket, max_hold, projected):
        if (not seconds(max_hold) or not 0 < max_hold <= MAX_HOLD_SECONDS
                or not seconds(projected) or not 0 < projected <= 43200):
            raise ValueError('Exclusive max hold must be in (0, 1800]; projection in (0, 43200]')
        self.state['claims'][ticket['id']] = {'max_hold': max_hold, 'projected': projected}
        self.save()

    def refund(self, ticket, *, event='hold-timeout-refund'):
        if event not in ('hold-timeout-refund', 'waiter-replacement-refund', 'wait-timeout-refund'):
            raise ValueError('Unknown heavy retry-credit reason')
        row = {key: ticket[key] for key in ('owner', 'class', 'enqueued_ns')}
        row['ticket'] = ticket['id']
        self.state['refunds'] = [old for old in self.state['refunds'] if old['ticket'] != row['ticket']]
        self.state['refunds'] = (self.state['refunds'] + [row])[-128:]
        self.event(event, **row)
        self.save()

    def redeem(self, token, ticket):
        row = next((row for row in self.state['refunds'] if row['ticket'] == token), None)
        if row is None or row['owner'] != ticket['owner'] or token in self.live:
            raise ValueError('Refund is absent, still running, or belongs to another owner')
        ticket['class'] = row['class']
        ticket['enqueued_ns'] = row['enqueued_ns']
        self.state['refunds'].remove(row)
        self.save()

    def finish(self, now, reason):
        active = self.state['active']
        if active is not None:
            self.event('drain-end', ticket=active['ticket'], reason=reason,
                       seconds=max(0, now - active['since']),
                       idle_slot_minutes=active['idle_slot_seconds'] / 60)
        self.state['yield_owner'] = active['ticket'] if active and reason == 'exclusive-admitted' else None
        self.state['cooldown_until'] = now + SHARED_WINDOW_SECONDS
        self.state['active'] = None
        self.state['deferred'] = []

    def exclusive_started(self, ticket, now):
        self.finish(now, 'exclusive-admitted')
        self.state['yield_owner'] = ticket['id']
        self.state['cooldown_until'] = now + SHARED_WINDOW_SECONDS
        self.save()

    def shared_window(self, now):
        return self.state['yield_owner'] is not None or now < self.state['cooldown_until']

    def update(self, pending, holders, runs, shared_request, now, limit, kernel_count):
        if self.state['yield_owner'] is not None and self.state['yield_owner'] not in {row['id'] for row in holders}:
            self.state['yield_owner'] = None
            self.state['cooldown_until'] = now + SHARED_WINDOW_SECONDS
        active = self.state['active']
        if active is not None:
            end = min(now, active['since'] + MAX_DRAIN_SECONDS)
            active['idle_slot_seconds'] += max(0, end - active['updated']) * max(0, limit - kernel_count)
            active['updated'] = end
            candidate = next((row for row in pending if row['id'] == active['ticket']), None)
            if now - active['since'] >= MAX_DRAIN_SECONDS:
                self.finish(end, 'drain-time-limit')
                self.state['cooldown_until'] = now + SHARED_WINDOW_SECONDS
            elif candidate is None or shared_request(candidate):
                self.finish(now, 'admitted-or-cancelled')
        candidates = [row for row in pending if not shared_request(row)
                      and row['id'] in self.state['claims']
                      and now - row['enqueued_ns'] / 1e9 >= WAIT_SECONDS]
        forced = next((row for row in candidates
                       if now - row['enqueued_ns'] / 1e9 >= MAX_EXCLUSIVE_WAIT), None)
        if (self.state['active'] is None and candidates
                and self.state['yield_owner'] is None and now >= self.state['cooldown_until']):
            # Before the hard wait bound, defer behind unknown or long holders.
            blockers = []
            for row in holders:
                claim = self.state['claims'].get(row['id'])
                run = runs.active.get(row['id'], {})
                since = run.get('since')
                remaining = None if claim is None or since is None else claim['projected'] - max(0, now - since)
                if remaining is None or remaining > MAX_REMAINING_SECONDS:
                    blockers.append(row['id'])
            deferred = set(self.state['deferred']) & {row['id'] for row in holders}
            if forced is None and (blockers or deferred or kernel_count > len(holders)):
                self.state['deferred'] = sorted(set(blockers) | deferred)
            else:
                candidate = forced or candidates[0]
                if forced is not None:
                    self.event('forced-drain', ticket=candidate['id'], owner=candidate['owner'],
                               wait_seconds=now - candidate['enqueued_ns'] / 1e9,
                               reason='exclusive-wait-bound')
                self.state['active'] = dict(ticket=candidate['id'], since=now, updated=now, idle_slot_seconds=0)
                self.state['deferred'] = []
                self.event('drain-start', ticket=candidate['id'], owner=candidate['owner'],
                           max_hold=self.state['claims'][candidate['id']]['max_hold'])
        if not candidates and self.state['active'] is None:
            self.state['deferred'] = []
        self.save()
        return self.state['active']['ticket'] if self.state['active'] else None
