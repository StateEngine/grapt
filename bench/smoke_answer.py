"""Recognize the native readiness smoke's exact answer with trailing punctuation."""

import unicodedata


def is_ready_answer(answer):
    text = answer.strip()
    if not text.startswith("READY"):
        return False
    return all(
        character.isspace() or unicodedata.category(character).startswith("P")
        for character in text[len("READY") :]
    )
