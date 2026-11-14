"""Reasoning sanitizer — strips chain-of-thought from model output.

Operates over complete (non-streaming) content. Recognizes:
- Paired XML-style tags: think, thinking, reasoning, thought, REASONING_SCRATCHPAD
- Leading unclosed tags (treat content as all-reasoning, return empty)
- Preserves literal tag text in prose/code samples (tags that appear mid-content
  and are NOT paired are left as-is)

This is a defense-in-depth layer. The primary strip happens at the connector
boundary; the same sanitizer is reapplied at relay and client boundaries.
"""

from __future__ import annotations

import re

# Tags recognized as reasoning containers (case-insensitive).
_REASONING_TAGS = [
    "think",
    "thinking",
    "reasoning",
    "thought",
    "REASONING_SCRATCHPAD",
]

# Build regex patterns for open/close tags.
_OPEN_PATTERN = re.compile(
    r"<(" + "|".join(re.escape(t) for t in _REASONING_TAGS) + r")>",
    re.IGNORECASE,
)
_CLOSE_PATTERN = re.compile(
    r"</(" + "|".join(re.escape(t) for t in _REASONING_TAGS) + r")>",
    re.IGNORECASE,
)


def strip_reasoning(text: str) -> str:
    """Remove reasoning blocks from *text* and return sanitized content.

    Handles paired tags of any recognized reasoning type.  An unclosed
    opening tag at the very beginning of the response is treated as if
    the entire response is reasoning (returns empty string).  Mid-content
    unclosed tags are left as-is (they are likely literal code/prose).

    Idempotent — applying twice produces the same result.
    """
    if not text:
        return text

    # Check for leading unclosed reasoning tag first.
    lead_match = _OPEN_PATTERN.match(text)
    if lead_match:
        tag_name = lead_match.group(1)
        # Search for the MATCHING close tag (not just any close tag)
        close_re = re.compile(r"</" + re.escape(tag_name) + r">", re.IGNORECASE)
        close_match = close_re.search(text, lead_match.end())
        if close_match is None:
            # Leading unclosed tag — entire response is reasoning
            return ""

    # Build result by scanning for paired tags.
    result_parts: list[str] = []
    pos = 0

    while pos < len(text):
        open_match = _OPEN_PATTERN.search(text, pos)
        if open_match is None:
            # No more reasoning tags — keep everything from pos
            result_parts.append(text[pos:])
            break

        tag_name = open_match.group(1)

        # Find MATCHING close tag (same tag name) starting from after the open tag
        close_re = re.compile(r"</" + re.escape(tag_name) + r">", re.IGNORECASE)
        close_match = close_re.search(text, open_match.end())

        if close_match is not None:
            # Paired — keep text before the open tag, skip the block
            result_parts.append(text[pos:open_match.start()])
            pos = close_match.end()
        elif open_match.start() == 0:
            # Leading unclosed tag — entire response is reasoning
            return ""
        else:
            # Unclosed mid-content tag — treat as literal text.
            # Keep everything up to and including the open tag, then continue.
            result_parts.append(text[pos:open_match.end()])
            pos = open_match.end()

    result = "".join(result_parts)
    # Collapse multiple blank lines that may result from removing blocks
    result = re.sub(r"\n{3,}", "\n\n", result)
    return result.strip()


def sanitize_message_content(text: str) -> tuple[str, bool]:
    """Sanitize message content, returning (sanitized_text, was_stripped).

    *was_stripped* is True when reasoning content was removed.
    """
    sanitized = strip_reasoning(text)
    was_stripped = sanitized != text
    return sanitized, was_stripped
