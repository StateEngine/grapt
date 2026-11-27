"""The fixture must reject replay, ambiguous tool linkage and invented summaries."""
import unittest

from sustained_provider import SustainedModel, output


class SustainedFixtureTests(unittest.TestCase):
    def test_duplicate_output_cannot_satisfy_a_background_poll(self):
        row = {"type": "function_call_output", "call_id": "call_poll", "output": "{}"}
        with self.assertRaisesRegex(ValueError, "exactly one linked output"):
            output({"input": [row, row]}, "call_poll")

    def test_summary_without_verified_tool_outcome_is_refused(self):
        model = SustainedModel()
        with self.assertRaisesRegex(ValueError, "objective or verified tool outcome"):
            model.dispatch("/responses", {"Authorization": "Bearer disposable-model-dummy"},
                {"input": [{"role": "user", "content": "Output exactly the Markdown structure shown inside <template> calc.py"}]})

    def test_an_extra_request_cannot_replay_completed_work(self):
        model = SustainedModel()
        model.stage = 11
        with self.assertRaisesRegex(ValueError, "replay or extra"):
            model.dispatch("/responses", {"Authorization": "Bearer disposable-model-dummy"}, {"input": []})

    def test_useful_tools_alone_cannot_claim_automatic_compaction(self):
        model = SustainedModel()
        model.stage = 4
        with self.assertRaisesRegex(ValueError, "automatic compaction lost"):
            model.dispatch("/responses", {"Authorization": "Bearer disposable-model-dummy"},
                {"input": [{"role": "user", "content": "SUSTAIN_TOOL_DONE"}]})


if __name__ == "__main__":
    unittest.main()
