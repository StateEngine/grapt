"""Capacity waits never dispatch while the host is pressured."""
import unittest
from unittest.mock import patch
import pressure


class PressureTests(unittest.TestCase):
    def test_busy_reading_waits_and_admits_the_later_idle_sample(self):
        with patch.object(pressure, 'read', side_effect=[{'some_avg10_percent': 20},
                {'some_avg10_percent': .02}]), patch.object(pressure.time, 'sleep') as sleep:
            result = pressure.wait(.5, 10)
        sleep.assert_called_once()
        self.assertEqual(result['some_avg10_percent'], .02)
        self.assertEqual(result['threshold_percent'], .5)

    def test_busy_capacity_has_a_deadline(self):
        with patch.object(pressure, 'read', return_value={'some_avg10_percent': 20}), \
                patch.object(pressure.time, 'monotonic', side_effect=[0, 11]):
            with self.assertRaisesRegex(RuntimeError, 'capacity unavailable'):
                pressure.wait(.5, 10)

    def test_missing_or_invalid_pressure_is_not_idle(self):
        for text in ['full avg10=0\n', 'some avg10=nan\n', 'some avg10=-1\n']:
            with self.subTest(text=text), patch.object(pressure.Path, 'read_text', return_value=text):
                with self.assertRaises(ValueError):
                    pressure.read()
