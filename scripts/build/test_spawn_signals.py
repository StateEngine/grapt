"""Spawn-window cancellation must never leave a handler on the deferral."""
import os
import signal
import unittest
from unittest.mock import patch

import heavy


class Cancelled(Exception):
    pass


class SpawnSignalTests(unittest.TestCase):
    def setUp(self):
        self.saved = {number: signal.getsignal(number) for number in (signal.SIGTERM, signal.SIGINT)}
        self.addCleanup(self.restore)

    def restore(self):
        for number, handler in self.saved.items():
            signal.signal(number, handler)

    def install(self):
        def terminate(number, frame):
            raise Cancelled(number)

        def interrupt(number, frame):
            raise Cancelled(number)

        signal.signal(signal.SIGTERM, terminate)
        signal.signal(signal.SIGINT, interrupt)
        return terminate, interrupt

    def test_a_signal_during_restoration_still_restores_every_handler(self):
        terminate, interrupt = self.install()
        real = signal.signal
        sent = []

        def restoring(number, handler):
            previous = real(number, handler)
            # Deliver TERM as soon as its own handler is back, mid-restoration.
            if number == signal.SIGTERM and handler is terminate and not sent:
                sent.append(number)
                os.kill(os.getpid(), signal.SIGTERM)
            return previous

        with self.assertRaises(Cancelled):
            with heavy.spawn_signals():
                patch.object(heavy.signal, 'signal', restoring).start()
                self.addCleanup(patch.stopall)
        patch.stopall()
        self.assertEqual(sent, [signal.SIGTERM])
        self.assertIs(signal.getsignal(signal.SIGTERM), terminate)
        self.assertIs(signal.getsignal(signal.SIGINT), interrupt)
        self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, []), set())

    def test_a_signal_in_the_spawn_window_is_delivered_after_restoration(self):
        terminate, interrupt = self.install()
        with self.assertRaises(Cancelled) as raised:
            with heavy.spawn_signals():
                os.kill(os.getpid(), signal.SIGINT)
                os.kill(os.getpid(), signal.SIGTERM)
        self.assertEqual(raised.exception.args, (signal.SIGINT,))
        self.assertIs(signal.getsignal(signal.SIGTERM), terminate)
        self.assertIs(signal.getsignal(signal.SIGINT), interrupt)
        self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, []), set())


if __name__ == '__main__':
    unittest.main()
