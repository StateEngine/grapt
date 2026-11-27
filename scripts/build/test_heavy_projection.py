import tempfile
import time
from pathlib import Path
import unittest
from heavy_runs import Runs

class ProjectionTests(unittest.TestCase):
    def test_slow_capacity_probe_does_not_publish_an_aged_status_sample(self):
        from unittest.mock import patch
        import heavy_queue
        import heavy_sampling
        from fixture_environment import hardware_sample
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            queue = heavy_queue.Queue(root/'queue', root/'heavy')
            ticket, notification = queue.enqueue(1, 'status-freshness', time.monotonic()+10, ram=True)
            clock = [time.monotonic()]

            def delayed_capacity():
                clock[0] += 3
                return True

            try:
                with patch.object(time, 'monotonic', side_effect=lambda: clock[0]), \
                        patch.object(heavy_sampling, 'processes', return_value={}), \
                        patch.object(heavy_sampling, 'host', return_value=hardware_sample()), \
                        patch.object(queue, 'shared_locks', return_value=0), \
                        patch.object(heavy_queue, 'ram_available', side_effect=delayed_capacity):
                    with self.assertRaisesRegex(TimeoutError, 'sample aged'):
                        queue.snapshot(clock[0]+10, detailed=True)
            finally:
                notification.close()
                queue.remove(ticket)

    def test_owner_then_kind_then_full_gate_median_uses_only_successful_holds(self):
        with tempfile.TemporaryDirectory() as directory:
            runs=Runs(Path(directory),[])
            runs.recent=[{'owner':name,'exit':code,'hold_seconds':duration} for name,code,duration in [
                ('seat-a-full',0,1000),('seat-b-full',0,3000),('seat-a-full',0,2000),
                ('seat-a-affected',0,100),('seat-a-full',1,43200)]]
            self.assertEqual(runs.projection('seat-a-full'),1500)
            self.assertEqual(runs.projection('other-full'),2000)
            self.assertEqual(runs.projection('other-affected'),100)
            self.assertEqual(runs.projection('unknown-owner'),2000)

    def test_empty_history_has_an_explicit_bounded_bootstrap_estimate(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(Runs(Path(directory),[]).projection('new-full'),2700)

    def test_new_ticket_persists_the_measured_projection_in_the_legacy_claim_shape(self):
        from heavy_queue import Queue
        from heavy_drain import Drain
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); queue=Queue(root/'queue',root/'heavy')
            with queue.locked(time.monotonic()+5):
                tickets,runs=queue.state()
                runs.recent=[{'id':'1-1','owner':'prior-full','exit':0,'hold_seconds':1234,
                              'targets':['ram'],'concurrent':True}]
                runs.save()
            ticket,notification=queue.enqueue(1,'next-full',time.monotonic()+5,max_hold=1800)
            try:
                with queue.locked(time.monotonic()+5):
                    tickets,_=queue.state()
                    claim=Drain(queue.directory,tickets).state['claims'][ticket['id']]
                    self.assertEqual(claim,{'max_hold':1800,'projected':1234})
            finally:
                notification.close(); queue.remove(ticket)

    def test_exact_wait_boundary_keeps_the_first_five_minutes_open(self):
        from types import SimpleNamespace
        from heavy_drain import Drain
        with tempfile.TemporaryDirectory() as directory:
            ticket={'id':'1-1','owner':'exclusive','class':1,'enqueued_ns':1_000_000_000}
            drain=Drain(Path(directory),[ticket]); drain.declare(ticket,600,360)
            runs=SimpleNamespace(active={})
            self.assertIsNone(drain.update([ticket],[],runs,lambda row:False,300,4,0))
            self.assertEqual(drain.update([ticket],[],runs,lambda row:False,301,4,0),'1-1')
