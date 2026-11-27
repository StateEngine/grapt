"""Script the model for one native-context walk using the existing transport.

The product and TUI remain real. Both this model and the MCP source are
disposable fixtures; this is workflow evidence, not live StateEngine evidence.
"""

import importlib.util
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True

path = Path(__file__).resolve().parents[1] / "native-tui" / "loopback-provider.py"
spec = importlib.util.spec_from_file_location("native_walk_transport", path)
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)

original_prompt = transport.prompt_of
original_frames = transport.frames
last_results = []
MARKER = "Native context fixture"
CALLS = [
    ("stateengine_stateengine_working_state_get", {}),
    ("stateengine_stateengine_memory_recall", {"query": "calc", "limit": 3}),
    ("stateengine_stateengine_atlas_repo_map", {"query": "calc", "token_budget": 512}),
    ("stateengine_stateengine_atlas_impact", {"target": "src/calc.rs", "limit": 3}),
]


def observe_prompt(body):
    global last_results
    decoded = json.loads(body)
    rows = decoded.get("input", [])
    # Automatic continuity is itself a user-role context item. It may follow
    # the human prompt, so the transport's last-user heuristic is insufficient
    # for this isolated one-prompt recipe.
    last_results = [row for row in rows if row.get("type") == "function_call_output"]
    prompt = MARKER if MARKER in body.decode() else original_prompt(body)
    if MARKER in prompt:
        serialized = json.dumps(last_results)
        print("NATIVE_CONTEXT_OBSERVED " + json.dumps({
            "result_count": len(last_results),
            "source_returned": "NATIVE_SOURCE_VICTOR6284" in serialized,
            "stale_preserved": "fixture-index-stale" in serialized,
            "absent_preserved": "fixture edge index absent" in serialized,
            "private_handles_absent": all(private not in body.decode() for private in (
                "NATIVE_FIXTURE_CAPABILITY", "NATIVE_FIXTURE_PRIVATE_CURSOR")),
        }), flush=True)
    return prompt


def context_frames(delay_ms, prompt, continuing=False):
    if MARKER not in prompt:
        return original_frames(delay_ms, prompt, continuing)
    ordinal = len(last_results)
    if ordinal < len(CALLS):
        name, args = CALLS[ordinal]
        return transport.tool_call_frames(args, delay_ms / 1000, name)
    if ordinal != len(CALLS):
        raise RuntimeError("native fixture continuation exceeded its four-tool bound")
    text = "Native context tools completed. Impact evidence is stale and incomplete."
    return [
        ("data: " + json.dumps({"type": "response.output_text.delta", "delta": text}) + "\n\n", 0),
        ('data: {"type":"response.output_text.done"}\n\n', 0),
        ('data: {"type":"response.completed","response":{"usage":{"input_tokens":12,"output_tokens":8,"total_tokens":20}}}\n\n', 0),
    ]


transport.prompt_of = observe_prompt
transport.frames = context_frames

if __name__ == "__main__":
    raise SystemExit(transport.main())
