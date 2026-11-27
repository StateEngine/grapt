"""The ordinary journey must reject invented recovery and compaction evidence."""
import unittest
from r_reliability_provider import ReliabilityModel, RECOVER

HEADERS = {'Authorization':'Bearer disposable-model-dummy'}

class ReliabilityFixtureTests(unittest.TestCase):
    def test_reusing_the_first_summary_cannot_claim_another_compaction(self):
        model=ReliabilityModel()
        model.stage, model.phase, model.summaries=10,'read',1
        with self.assertRaisesRegex(RuntimeError,'another automatic compaction'):
            model.dispatch('/responses',HEADERS,{'input':[{'role':'user','content':
                'Model-written continuation summary Subtraction added SUSTAIN_TASK'}]})

    def test_a_new_summary_cannot_drop_the_verified_current_objective(self):
        model=ReliabilityModel()
        model.stage, model.phase, model.summaries=10,'read',2
        with self.assertRaisesRegex(RuntimeError,'current objective or tested changes'):
            model.dispatch('/responses',HEADERS,{'input':[{'role':'user','content':
                'Model-written continuation summary SUSTAIN_TASK'}]})

    def test_another_parent_cannot_supply_the_lost_effect_warning(self):
        model=ReliabilityModel()
        model.stage=12
        with self.assertRaisesRegex(RuntimeError,'original nested effect identity'):
            model.dispatch('/responses',HEADERS,{'input':[{'role':'user','content':
                RECOVER+' [Harness notice: Tool nested_hold run inside another_parent ended with an unknown effect; do not repeat it blindly]'}]})

    def test_a_recovery_warning_never_invents_completed_tool_output(self):
        model=ReliabilityModel()
        model.stage=13
        with self.assertRaisesRegex(ValueError,'exactly one linked output'):
            model.dispatch('/responses',HEADERS,{'input':[{'role':'user','content':'tests passed OK'}]})

    def test_completed_recovery_rejects_extra_provider_work(self):
        model=ReliabilityModel()
        model.stage=14
        with self.assertRaisesRegex(ValueError,'replay or extra'):
            model.dispatch('/responses',HEADERS,{'input':[]})

if __name__=='__main__': unittest.main()
