import unittest

from smoke_answer import is_ready_answer


class ReadyAnswerTests(unittest.TestCase):
    def test_accepts_only_ready_with_trailing_punctuation_or_whitespace(self):
        for answer in ["READY", "READY.", " READY.\n", "READY!", "READY ?!", "READY…"]:
            with self.subTest(answer=answer):
                self.assertTrue(is_ready_answer(answer))

    def test_rejects_other_words_and_leading_punctuation(self):
        for answer in ["", "Ready", "NOT READY", "READY now", "READY\nERROR", ".READY", "READY1", "READY$", "READY🙂"]:
            with self.subTest(answer=answer):
                self.assertFalse(is_ready_answer(answer))


if __name__ == "__main__":
    unittest.main()
