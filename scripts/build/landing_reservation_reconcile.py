"""Explicit consumption of a separately issued, sealed owner reconciliation."""
from datetime import datetime
from pathlib import Path
import re

import landing_reservation as reservation
from landing_reservation_history import absent_pins, commit, digest, directory, locked, read, refresh, require

FIELDS = {'schema', 'reconciliation_id', 'boot_id', 'observer', 'issued_utc', 'request_raw_sha256',
          'attestation_path', 'retained_record', 'retained_raw_sha256', 'current_publisher_revision',
          'current_publisher_generation', 'current_publisher_raw_sha256', 'terminal',
          'intervening_cancellation', 'outside_observation', 'evidence', 'outcome'}
SOURCES = {'retained': ('retained_record_path', 'retained_raw_sha256'),
           'stop': ('shared_stop_path', 'shared_stop_raw_sha256'),
           'controller': ('controller_result_path', 'controller_result_raw_sha256'),
           'admission': ('canceled_admission_path', 'canceled_admission_raw_sha256'),
           'message': ('joined_stop_message_path', 'joined_stop_message_raw_sha256')}


def stamp(value):
    require(isinstance(value, str) and len(value) <= 80, 'Invalid reconciliation timestamp')
    value = datetime.fromisoformat(value)
    require(value.utcoffset() is not None, 'Reconciliation timestamp needs a timezone')
    return value


def evidence(attestation, root):
    entries = attestation['evidence']
    require(isinstance(entries, dict) and set(entries) == {'current', *SOURCES}, 'Sealed evidence set differs')
    captured, files = {}, []
    for name, entry in entries.items():
        require(isinstance(entry, dict) and set(entry) == {'source_path', 'file', 'raw_sha256'}
                and entry['file'] == name+'.raw' and isinstance(entry['source_path'], str)
                and Path(entry['source_path']).is_absolute(), 'Sealed evidence entry differs')
        raw, value = read(root/(name+'.raw'))
        require(digest(raw) == entry['raw_sha256'], 'Sealed original evidence digest differs')
        captured[name] = (raw, value)
        files.append((name+'.raw', raw))
    request_raw, request = read(root/'request.raw')
    fields = {'schema', 'boot_id', 'expected_current_revision', 'expected_current_raw_sha256', 'joined_stop_message_ref'}
    fields |= {field for pair in SOURCES.values() for field in pair}
    require(isinstance(request, dict) and set(request) == fields
            and request['schema'] == 'guest-owner-reconcile-request/v1'
            and request['boot_id'] == attestation['boot_id']
            and digest(request_raw) == attestation['request_raw_sha256']
            and type(request['expected_current_revision']) is int
            and request['expected_current_revision'] == attestation['current_publisher_revision']
            and request['expected_current_raw_sha256'] == attestation['current_publisher_raw_sha256'],
            'Sealed request does not bind the owner attestation')
    for name, (path_key, hash_key) in SOURCES.items():
        require(request[path_key] == entries[name]['source_path']
                and request[hash_key] == entries[name]['raw_sha256'], 'Sealed request source differs')
    return captured, request, [*files, ('request.raw', request_raw)]


def validate(reader, value, original, current, current_raw, captured, request):
    prior = reader.previous
    require(value['retained_record'] == prior and value['retained_raw_sha256'] == digest(original)
            and captured['retained'][0] == original and captured['current'][0] == current_raw
            and value['current_publisher_revision'] == current['revision']
            and type(value['current_publisher_revision']) is int
            and value['current_publisher_generation'] == current['generation']
            and value['current_publisher_raw_sha256'] == digest(current_raw), 'Attestation is stale or does not bind the original cursor/current bytes')
    admission = reservation.record(captured['admission'][1], reader.boot)
    require(prior['phase'] == 'running' and admission['phase'] == 'admitted' and current['phase'] == 'idle'
            and admission['revision'] == prior['revision']+2 and current['revision'] == admission['revision']+1
            and admission['generation'] != prior['generation'] and admission['generation'] == current['generation']
            and admission['guest'] == prior['guest'] and current['clearance']['kind'] == 'cancelled-admission'
            and current['clearance']['stopped_guest'] == admission['guest'], 'Attestation is not the bounded missing terminal chain')
    terminal = value['terminal']
    require(isinstance(terminal, dict) and set(terminal) == {'generation', 'guest', 'launcher', 'qemu', 'swtpm', 'joined_stop_message_ref'}
            and all(terminal[key] == prior[key] for key in ('generation', 'guest', 'launcher', 'qemu', 'swtpm'))
            and terminal['joined_stop_message_ref'] == request['joined_stop_message_ref'], 'Attested terminal identity differs')
    reservation.text(terminal['joined_stop_message_ref'], 'msg-[0-9a-f]{24}')
    require(value['intervening_cancellation'] == dict(admission_revision=admission['revision'],
            generation=admission['generation'], admission_ref=admission['admission_ref'], clearance=current['clearance']),
            'Attested cancellation differs')
    stop, controller, message = (captured[name][1] for name in ('stop', 'controller', 'message'))
    require(isinstance(stop, dict) and set(stop) == {'schema', 'generation', 'guest', 'qemu', 'swtpm', 'stopper', 'updated_utc'}
            and stop['schema'] == 'guest-stop/v1' and stop['generation'] == prior['generation']
            and stop['guest'] == prior['guest'] and stop['stopper'] == prior['launcher']
            and stop['qemu'] is None and stop['swtpm'] is None, 'Sealed shared stop is contradictory')
    require(isinstance(controller, dict) and controller.get('reservation_generation') == prior['generation']
            and type(controller.get('controller_pid')) is int and controller['controller_pid'] == prior['launcher']['pid']
            and controller.get('joined_stop') is True and 'qemu_after' in controller and controller['qemu_after'] is None
            and 'swtpm_after' in controller and controller['swtpm_after'] is None, 'Sealed controller is not joined')
    for name in ('qemu', 'swtpm'):
        observed = controller.get(name)
        require(isinstance(observed, dict) and type(observed.get('pid')) is int
                and all(observed.get(key) == prior[name][key] for key in ('pid', 'start')), 'Sealed guest process identity differs')
    require(isinstance(message, dict) and message.get('source') == 'stateengine_msg_read'
            and message.get('project_id') == 'stateengine', 'Sealed joined-stop provenance differs')
    message = message.get('message')
    require(isinstance(message, dict) and message.get('msg_id') == terminal['joined_stop_message_ref']
            and message.get('from_seat') == prior['owner'] and message.get('to_seat') == 'floor'
            and message.get('kind') == 'done' and isinstance(message.get('refs'), list)
            and isinstance(controller.get('release_message'), str)
            and controller['release_message'] in message['refs'], 'Sealed joined-stop message differs')
    require(stamp(prior['updated_utc']) <= stamp(stop['updated_utc']) <= stamp(message['ts'])
            <= stamp(admission['updated_utc']) <= stamp(current['updated_utc']) <= stamp(value['issued_utc']),
            'Reconciliation evidence ordering differs')
    observed = value['outside_observation']
    require(isinstance(observed, dict) and set(observed) == {'guests', 'processes'}
            and isinstance(observed['guests'], dict) and set(observed['guests']) == set(reservation.ROOTS)
            and all(row == dict(qemu=None, swtpm=None, memory_gib=None) for row in observed['guests'].values())
            and isinstance(observed['processes'], list) and len(observed['processes']) == 3, 'Sealed outside census differs')
    for name, row in zip(('launcher', 'qemu', 'swtpm'), observed['processes']):
        require(isinstance(row, dict) and set(row) == {'role', 'expected', 'observed'} and row['role'] == name
                and row['expected'] == prior[name] and row['observed'] != prior[name], 'Sealed old process is not absent')
        if row['observed'] is not None:
            reservation.pin(row['observed'], reader.boot)
    absent_pins(reader, prior)


def reconcile(reader, path, census):
    original = refresh(reader)
    require(reader.previous is not None, 'Explicit reconciliation needs an original retained cursor')
    with locked(reader):
        current_raw, current = read(reader.path)
        reservation.record(current, reader.boot)
        key = digest((digest(original)+digest(current_raw)).encode())
        root = reader.path.parent/'reconciliations'/key
        require(path == root/'attestation.json', 'Reconciliation path does not bind the owned cursor/current pair')
        raw, value = read(path)
        require(isinstance(value, dict) and set(value) == FIELDS and value['schema'] == 'guest-owner-reconciliation/v1'
                and value['boot_id'] == reader.boot and value['attestation_path'] == str(path)
                and value['outcome'] == 'retained-generation-terminal-and-current-idle', 'Owner attestation schema or outcome differs')
        reservation.token(value['reconciliation_id'])
        require(value['reconciliation_id'] not in (reader.previous['generation'], current['generation']), 'Reconciliation identity reuses a generation')
        reservation.pin(value['observer'], reader.boot)
        captured, request, files = evidence(value, root)
        validate(reader, value, original, current, current_raw, captured, request)
        archive = reader.path.parent/'publications'/reader.boot
        try:
            fd = directory(archive)
        except FileNotFoundError:
            pass
        else:
            try:
                import os
                for index, name in enumerate(os.listdir(fd)):
                    require(index < 8192, 'Publication inventory exceeds its bound')
                    require(not name.startswith(str(reader.previous['revision']+1)+'-'), 'Authentic terminal exists; use ordered history')
            finally:
                os.close(fd)
        return commit(reader, original, current, current_raw, [('attestation.raw', raw), *files], census,
                      [reader.previous, captured['admission'][1]])
