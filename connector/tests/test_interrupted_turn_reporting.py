"""B2: sentinel detection on the trailing segment.

Tests for _is_interrupt_sentinel and _split_trailing_sentinel which detect
Hermes' "Operation interrupted." placeholder text.
"""

from kallisti_connector.herald_runner import _is_interrupt_sentinel, _split_trailing_sentinel


def test_sentinel_after_preamble_is_detected():
    """The screenshot's exact shape: preamble, then the marker."""
    text = "Let me pull your play history and grab those recs. Operation interrupted."
    answer, sentinel = _split_trailing_sentinel(text)
    assert sentinel == "Operation interrupted."
    assert answer == "Let me pull your play history and grab those recs."
    # The old startswith predicate missed exactly this case.
    assert not _is_interrupt_sentinel(text)


def test_sentinel_on_its_own_line_is_detected():
    answer, sentinel = _split_trailing_sentinel(
        "Checking.\n\nOperation interrupted: handling API error (500)"
    )
    assert sentinel.startswith("Operation interrupted:")
    assert answer == "Checking."


def test_sentinel_only_turn():
    answer, sentinel = _split_trailing_sentinel("Operation interrupted.")
    assert sentinel == "Operation interrupted."
    assert answer == ""


def test_ordinary_prose_is_not_a_sentinel():
    text = "The operation interrupted earlier, but it is fixed now."
    answer, sentinel = _split_trailing_sentinel(text)
    assert sentinel is None
    assert answer == text


def test_empty_turn():
    assert _split_trailing_sentinel("") == ("", None)
