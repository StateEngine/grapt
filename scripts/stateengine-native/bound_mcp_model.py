"""The same literal discovery and real bound-tool checks on either native host."""
import json
import headless_acceptance as headless
from acceptance_peer import Model
from real_seat_model import first_continuity, object_result
from sustained_provider import calls, output, text

MARKER = "BOUND_REAL_STATEENGINE_SEND"


def require_acknowledged(resumed, binding, packet, expected_ack=None):
    principal = resumed.get('principal', {})
    acknowledgement = resumed.get('packet_acknowledgement', {})
    headless.require(principal.get('verified') is True
        and principal.get('project_id') == binding['project']
        and principal.get('agent_id') == binding['agent']
        and acknowledgement.get('state') == 'acknowledged'
        and type(acknowledgement.get('recorded')) is bool
        and isinstance(acknowledgement.get('acknowledgement_id'), str)
        and bool(acknowledgement['acknowledgement_id'])
        and (acknowledgement.get('recorded') is True if expected_ack is None
             else acknowledgement['acknowledgement_id'] == expected_ack)
        and acknowledgement.get('packet_id') == packet,
        'bound-MCP resume: matching recorded packet acknowledgement absent')
    return acknowledgement['acknowledgement_id']


def require_continuity(body, binding, packet):
    delivered, resumed = first_continuity(body.get('input', []))
    headless.require(delivered == packet, 'bound-MCP resume: delivered packet differs from launch receipt')
    return require_acknowledged(resumed, binding, delivered)

class BoundTools(Model):
    def __init__(self, leg='all'):
        super().__init__()
        self.leg = leg
        self.checks = {}
        self.diagnostics = {}
        self.engine = self.binding = self.packet = None

    @staticmethod
    def execute(call, code):
        return calls([(call, "execute", {"code": code})])

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        try:
            return self.respond(body)
        except Exception as error:
            leg = self.leg if self.leg != 'all' else (
                'discovery' if self.count() <= 2 else 'resume' if self.count() == 3 else 'channel')
            self.diagnostics['failure'] = f'bound-MCP {leg}: step {self.count()} {type(error).__name__}'
            raise

    def respond(self, body):
        ordinal = self.count()
        if ordinal == 1:
            self.acknowledgement = require_continuity(body, self.binding, self.packet)
            self.diagnostics['delivered_packet_acknowledged'] = True
        if self.leg == 'placement':
            if ordinal == 1:
                return self.execute('list', "return await tools.stateengine_stateengine_msg_list({limit:10});")
            try:
                listed = object_result(output(body, 'list'))
                self.checks['placement_dispatched'] = isinstance(listed.get('messages'), list)
            except (ValueError, KeyError, TypeError):
                self.checks['placement_dispatched'] = False
            headless.require(self.checks['placement_dispatched'], 'bound-MCP placement: real list dispatch failed')
            return text('BOUND_REAL_STATEENGINE_DONE')
        if self.leg == 'channel' and ordinal == 1:
            self.engine.seed()
            return self.execute('large', "try {return {unexpected:await tools.stateengine_stateengine_msg_list("
                "{limit:200,envelope:'full',full:true})};} catch(e) {return {failure:JSON.stringify(e)};}")
        step = ordinal + (2 if self.leg == 'channel' else 0)
        if step == 1:
            return self.execute("discover", "return {resume:catalog.search('stateengine_resume',16),"
                                "inbox:catalog.search('inbox',16),"
                                "sentence:catalog.search('stateengine resume inbox messages read send',8),"
                                "skill:catalog.search('skill',8),"
                                "list:catalog.describe('stateengine_stateengine_msg_list')};")
        if step == 2:
            found = output(body, "discover")
            self.checks["resume_discovered"] = any(row["name"] == "stateengine_stateengine_resume"
                                                   for row in found["resume"]["matches"])
            self.checks["list_discovered"] = found["list"]["name"] == "stateengine_stateengine_msg_list"
            self.checks["inbox_discovered"] = any(row["name"] == "stateengine_stateengine_msg_list"
                                                  for row in found["inbox"]["matches"])
            self.checks["sentence_discovered"] = all(any(row["name"] == "stateengine_stateengine_" + name
                for row in found["sentence"]["matches"]) for name in ["resume", "msg_list", "msg_send"])
            # Skills are a built-in tool, not a deferred MCP registration.
            self.checks["skill_catalog_honest"] = found["skill"]["total"] == 0
            headless.require(all(self.checks.values()), "bound-MCP discovery: real tools absent from CodeMode")
            if self.leg == 'discovery':
                return text('BOUND_REAL_STATEENGINE_DONE')
            return self.execute("resume", "return await tools.stateengine_stateengine_resume("
                                + json.dumps({"packet_id": self.packet}) + ");")
        if step == 3:
            resumed = object_result(output(body, "resume"))
            require_acknowledged(resumed, self.binding, self.packet, self.acknowledgement)
            self.checks["resume_dispatched"] = True
            headless.require(self.engine.seed()["seeded"], "inbox seed failed")
            return self.execute("large", "try {return {unexpected:await tools.stateengine_stateengine_msg_list("
                                "{limit:200,envelope:'full',full:true})};} catch(e) {return {failure:JSON.stringify(e)};}")
        if step == 4:
            large = output(body, "large")
            self.diagnostics["large_reply"] = {"bytes": len(json.dumps(large)),
                                               "failure": large.get("failure", "")[:500]}
            self.checks["oversize_isolated"] = "output exceeded 32768 bytes; use a smaller limit" in large.get("failure", "")
            headless.require(self.checks["oversize_isolated"], "bound-MCP channel: oversized result did not produce the bounded refusal")
            return self.execute("list", "return await tools.stateengine_stateengine_msg_list({limit:10});")
        if step == 5:
            listed = object_result(output(body, "list"))
            self.checks["list_dispatched"] = isinstance(listed.get("messages"), list)
            headless.require(self.checks["list_dispatched"], "bound-MCP channel: small read failed after oversized result")
            args = {"agent_id": self.binding["agent"], "to": self.binding["recipient"],
                    "kind": "handoff", "work_action": "return", "body": MARKER}
            return self.execute("send", "return await tools.stateengine_stateengine_msg_send("
                                + json.dumps(args) + ");")
        if step == 6:
            sent = object_result(output(body, "send"))
            self.checks["send_dispatched"] = (sent.get("append", {}).get("state") == "ok"
                and self.engine.verify(sent.get("msg_id"), MARKER)["verified"])
            headless.require(self.checks["send_dispatched"], "bound-MCP channel: real send was not committed exactly once")
            return text("BOUND_REAL_STATEENGINE_DONE")
        raise RuntimeError("unexpected provider call")
