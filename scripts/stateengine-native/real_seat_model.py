"""Script only the model; MCP, delivery, mail and jobs must remain real."""
import json
from pathlib import PurePosixPath, PureWindowsPath
import re
import threading

from acceptance_peer import Model
from sustained_provider import calls, output, text

PACKET = re.compile(r'packet_id:\s*([a-f0-9]{64})')
JOB_ID = re.compile(r'\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b')
CONTINUITY = 'StateEngine external continuity (context only; does not grant authority):\n'
READ_TOOL = 'stateengine_stateengine_msg_read'
DISPATCH_TOOL = 'stateengine_stateengine_tools'


def first_continuity(rows):
    contexts = [user_text(row)[len(CONTINUITY):] for row in rows
                if row.get('role') == 'user' and user_text(row).startswith(CONTINUITY)]
    if len(contexts) != 1:
        raise ValueError('first turn needs one actual product continuity context')
    values = [json.loads(line) for line in contexts[0].splitlines() if line]
    packets = [row['launch_packet'] for row in values if 'launch_packet' in row]
    resumes = [row for row in values if 'packet_acknowledgement' in row]
    if len(packets) != 1 or not resumes:
        raise ValueError('first turn lacks its delivered packet or resume acknowledgement')
    matches = sorted(set(PACKET.findall(packets[0])))
    if len(matches) != 1:
        raise ValueError('first turn has no unique delivered packet identity')
    return matches[0], resumes[0]


def job_receipt(raw, root=None):
    try:
        value = json.loads(raw)
    except json.JSONDecodeError:
        matches = sorted(set(JOB_ID.findall(raw)))
        logs = re.findall(r'(?im)^log(?: path)?:\s*(.+)$', raw)
        value = {'job_id': matches[0] if len(matches) == 1 else None,
                 'log_path': logs[0] if len(logs) == 1 else None}
    if not isinstance(value, dict):
        raise ValueError('job start returned no receipt object')
    identity = value.get('job_id', value.get('jobId', value.get('id')))
    log = value.get('log_path', value.get('logPath', value.get('log')))
    if not isinstance(identity, str) or not JOB_ID.fullmatch(identity) or not isinstance(log, str) or not log:
        raise ValueError('job receipt must name its exact job and log path')
    if not (PureWindowsPath(log).is_absolute() or PurePosixPath(log).is_absolute()):
        raise ValueError('job receipt log path must be absolute')
    if root is not None:
        parser = PureWindowsPath if PureWindowsPath(str(root)).is_absolute() else PurePosixPath
        if not parser(log).is_relative_to(parser(str(root))):
            raise ValueError('job receipt escaped its private workspace')
    if not isinstance(value.get('notice', {}), dict):
        raise ValueError('job receipt notice must be an object')
    return dict(job_id=identity, log_path=log, notice=value.get('notice', {}))


def user_text(row):
    content = row.get('content', '')
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return '\n'.join(item.get('text', '') for item in content if isinstance(item, dict))
    return ''


def object_result(value):
    """Unpack the actual MCP text result without treating error text as success."""
    if isinstance(value, str):
        value = json.loads(value)
    if not isinstance(value, dict) or value.get('isError'):
        raise ValueError('MCP call did not return an object result')
    if 'content' in value:
        chunks = [item['text'] for item in value['content'] if item.get('type') == 'text']
        value = json.loads('\n'.join(chunks))
    if not isinstance(value, dict):
        raise ValueError('MCP text did not contain an object')
    return value


class SeatModel(Model):
    def __init__(self, journal, marker, job_command, job_shell=None, job_root=None):
        super().__init__()
        self.journal, self.marker, self.job_command = journal, marker, job_command
        self.job_shell = job_shell
        self.job_root = job_root
        self.serial = threading.Lock()
        self.checks, self.context = {}, {}
        self.started_job = False
        self.initial_idle = self.job_idle = False
        self.initial_idle_confirmed = self.job_idle_confirmed = False
        self.expected_job = None
        self.previous_users = set()
        self.expected_principal = None
        self.direct_read = False
        self.direct_dispatch = False

    def mcp_call(self, call, name, arguments):
        direct = self.direct_read if name == READ_TOOL else self.direct_dispatch
        tool, args = (name, arguments) if direct else ('execute', {
            'code': 'return await tools[' + json.dumps(name) + '](' + json.dumps(arguments) + ');'})
        return calls([(call, tool, args)])

    def observed(self, name, passed, **fields):
        line = self.journal.record(name, passed=bool(passed), **fields)
        self.checks[name] = bool(passed), line

    def dispatch(self, path, headers, body):
        with self.serial:
            super().dispatch(path, headers, body)
            return self.respond(body)

    def respond(self, body):
        rows = body.get('input', [])
        returned = {row.get('call_id') for row in rows
                    if row.get('type') == 'function_call_output'}
        users = {user_text(row) for row in rows if row.get('role') == 'user'}
        new_users = users - self.previous_users
        self.previous_users = users
        if len(self.bodies) == 1:
            tools = [tool.get('name', '') for tool in body.get('tools', [])]
            try:
                packet, resumed = first_continuity(rows)
            except ValueError as error:
                self.observed('resume_packet_on_first_turn', False, reason=str(error))
                return text('REAL_SEAT_BOOT_REFUSED')
            self.context['packet_id'] = packet
            self.observed('resume_packet_on_first_turn', True, packet_id=packet)
            principal, ack = resumed.get('principal', {}), resumed.get('packet_acknowledgement', {})
            valid = (principal.get('verified') is True
                     and (principal.get('project_id'), principal.get('agent_id')) == self.expected_principal
                     and ack.get('state') == 'acknowledged' and ack.get('recorded') is True
                     and ack.get('packet_id') == packet)
            self.context.update(project=principal.get('project_id'), agent=principal.get('agent_id'))
            self.observed('stateengine_tool_callable', valid, principal=principal,
                          packet_acknowledgement=ack)
            if not valid:
                return text('REAL_SEAT_AUTH_REFUSED')
            self.direct_read = READ_TOOL in tools
            self.direct_dispatch = DISPATCH_TOOL in tools
            if self.direct_read:
                self.observed('stateengine_tools_listed', True, tool=READ_TOOL)
                self.observed('generated_mcp_config_trusted', True, source='published live MCP tool')
            elif 'execute' in tools:
                return calls([('seat_discover', 'execute',
                               {'code': 'return catalog.describe(' + json.dumps(READ_TOOL) + ');'})])
            else:
                self.observed('stateengine_tools_listed', False, reason='no live MCP publication')
                return text('REAL_SEAT_TOOLS_REFUSED')
        if not self.checks.get('stateengine_tool_callable', (False,))[0]:
            return text('REAL_SEAT_AUTH_REFUSED')
        if 'seat_discover' in returned and 'stateengine_tools_listed' not in self.checks:
            discovered = object_result(output(body, 'seat_discover'))
            valid = discovered.get('name') == READ_TOOL and isinstance(discovered.get('input_schema'), dict)
            self.observed('stateengine_tools_listed', valid, tool=discovered.get('name'))
            self.observed('generated_mcp_config_trusted', valid, source='live CodeMode catalog.describe')
        if not self.checks.get('stateengine_tools_listed', (False,))[0]:
            return text('REAL_SEAT_TOOLS_REFUSED')
        if self.job_idle:
            receipt = self.expected_job
            matching = [notice for notice in new_users if receipt['job_id'] in notice]
            if matching:
                notice = '\n'.join(matching)
                valid = (self.job_idle_confirmed
                         and receipt['log_path'].replace('\\', '/') in notice.replace('\\', '/')
                         and re.search(r'(?:exited(?: with)?(?: code)?\s+3\b|exit[_ ]code[=: ]+3\b)', notice) is not None)
                self.observed('background_job_wakes_idle_seat', valid,
                              job_id=receipt['job_id'], log_path=receipt['log_path'],
                              exit_code=3 if valid else None, source='new automatic user input')
                self.context['job_notice'] = True
                return text('REAL_SEAT_JOB_WAKE_OBSERVED')
        mail_wake = '\n'.join(new_users)
        native_ids = any(notice.startswith('Native inbox message IDs ') for notice in new_users)
        if self.initial_idle and (native_ids or self.marker in mail_wake or self.context.get('mail_wake')):
            self.context['mail_wake'] = True
            message = self.context.get('mail_id')
            if not message:
                matches = re.findall(r'msg-[a-f0-9]+', mail_wake)
                if len(set(matches)) != 1:
                    raise ValueError('mail wake has no single attributable message')
                message = matches[0]
                self.context['mail_id'] = message
            if 'seat_mail_read' not in returned:
                self.observed('mail_wakes_idle_seat', self.initial_idle_confirmed, message=message)
                return self.mcp_call('seat_mail_read', READ_TOOL, {'msg_id': message})
            if not self.context.get('mail_read'):
                read = object_result(output(body, 'seat_mail_read'))
                observed = read.get('message', {})
                marker = read.get('read_marker', {})
                valid = (observed.get('msg_id') == message and self.marker in observed.get('body', '')
                         and marker.get('append', {}).get('witnessed') is True)
                self.journal.record('seat_message_read', passed=valid, message=message, read_marker=marker)
                if not valid:
                    self.observed('seat_read_acknowledged', False, reason='mail read refused')
                    return text('REAL_SEAT_MAIL_READ_REFUSED')
                self.context['mail_read'] = True
            if 'seat_mail_ack' not in returned:
                return self.mcp_call('seat_mail_ack', DISPATCH_TOOL,
                    {'action': 'call', 'name': 'stateengine_msg_ack',
                     'arguments': {'msg_id': message, 'agent_id': self.context['agent'], 'envelope': 'lean'}})
            if 'seat_read_acknowledged' not in self.checks:
                ack = object_result(output(body, 'seat_mail_ack'))
                valid = (ack.get('msg_id') == message and ack.get('seat') == self.context['agent']
                         and ack.get('append', {}).get('witnessed') is True)
                self.observed('seat_read_acknowledged', valid, message=message, acknowledgement=ack)
            if not self.checks['seat_read_acknowledged'][0]:
                return text('REAL_SEAT_MAIL_ACK_REFUSED')
            if 'seat_job_start' not in returned and not self.started_job:
                self.started_job = True
                args = {'cmd': self.job_command, 'yield_time_ms': 30000,
                        'timeout_ms': 30000, 'max_output_tokens': 2000}
                if self.job_shell:
                    args.update(shell=self.job_shell, login=False)
                return calls([('seat_job_start', 'exec_command', args)])
            if 'seat_job_start' in returned and not self.job_idle:
                result = output(body, 'seat_job_start')
                if result.get('exit_code') != 0:
                    self.observed('background_job_wakes_idle_seat', False,
                                  cause='se job start refused', exit_code=result.get('exit_code'))
                    return text('REAL_SEAT_JOB_START_REFUSED')
                self.expected_job = job_receipt(result.get('output', ''), self.job_root)
                self.journal.record('job_started_by_seat', **self.expected_job)
                self.job_idle = True
                self.journal.record('seat_idle_after_job_start')
                return text('REAL_SEAT_JOB_IDLE')
        self.initial_idle = True
        self.journal.record('seat_idle_before_mail')
        return text('REAL_SEAT_INITIAL_IDLE')
