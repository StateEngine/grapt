"""Raw shared holders consume measured capacity without becoming queue owners."""
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import external_holders
import heavy_hardware as hardware


class ExternalHolderTests(unittest.TestCase):
    def test_zero_one_and_seven_idle_or_busy_external_families(self):
        frequency = os.sysconf('SC_CLK_TCK')
        host = dict(available=96*hardware.GIB, total=128*hardware.GIB, cores=48, load=137,
                    nvme_free=200*hardware.GIB, root_free=100*hardware.GIB)
        for count in (0, 1, 7):
            for busy in (False, True):
                with self.subTest(count=count, busy=busy), tempfile.TemporaryDirectory() as temporary:
                    forest = {pid: dict(parent=0, start='0', rss=hardware.GIB, ticks=0)
                              for pid in range(1, count + 1)}
                    initial, prior = external_holders.measure(set(forest), set(), forest, host, 12,
                                                              None, 100, [100, 1000])
                    self.assertEqual(initial['count'], count)
                    for row in forest.values():
                        row['ticks'] = 4 * frequency if busy else 0
                    charged, _ = external_holders.measure(set(forest), set(), forest, host, 12,
                                                          prior, 101, [100 + round(count*4/48*1000) if busy else 100, 2000])
                    self.assertEqual(charged['count'], count)
                    self.assertEqual(charged['slots'], min(count, 6))
                    self.assertEqual(charged['rss'], count * hardware.GIB)
                    self.assertEqual(charged['peer']['cpu'], min(count * 4, 24) if busy else 0)
                    profiles = hardware.Profiles(Path(temporary), [], SimpleNamespace(active={}, recent=[]))
                    peers = profiles.peers([], count, charged)
                    self.assertIsNone(hardware.refusal(dict(host, load=charged['load']),
                        dict(peak_rss=8*hardware.GIB, cpu=3), peers, charged['slots']))
                    # A governed orphan remains conservatively reserved in addition to external work.
                    _, orphan = profiles.peers([], count+1, charged)
                    self.assertEqual(orphan['peak_rss'], hardware.UNKNOWN_LEASE_RSS)
                    if busy:
                        saturated, _ = external_holders.measure(set(forest), set(), forest, host, 12,
                                                               prior, 101, [1100, 2000])
                        self.assertEqual(hardware.refusal(dict(host, load=saturated['load']),
                            dict(peak_rss=8*hardware.GIB, cpu=3), peers, saturated['slots']), 'load guard')

        # Nested raw locks share one process tree; governed families retain their owner.
        host = dict(available=128*hardware.GIB, total=128*hardware.GIB, cores=48, load=0)
        forest = {1:dict(parent=0,start='0',rss=hardware.GIB,ticks=0),
                  2:dict(parent=1,start='0',rss=hardware.GIB,ticks=0),
                  3:dict(parent=0,start='0',rss=hardware.GIB,ticks=0)}
        charged, _ = external_holders.measure({1,2,3}, {3}, forest, host, 12, None, 100, [0,100])
        self.assertEqual(charged['count'], 2)
        self.assertEqual(charged['rss'], 2 * hardware.GIB)

        # A governed subtree beneath an external ancestor is already reserved
        # by Profiles.peers and must not be charged a second time.
        forest[3].update(parent=1, rss=8 * hardware.GIB, ticks=400 * frequency)
        forest[4] = dict(parent=3, start='0', rss=2 * hardware.GIB, ticks=200 * frequency)
        charged, _ = external_holders.measure({1,3}, {3}, forest, host, 12, None, 100, [0,100])
        self.assertEqual(charged['count'], 1)
        self.assertEqual(charged['rss'], 2 * hardware.GIB)
        self.assertEqual(charged['cpu'], 0)
