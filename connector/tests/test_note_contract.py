"""Tests for note enrichment contract validation and fixture round-trip."""
from __future__ import annotations

import json
from pathlib import Path

from herald_connector.note_contract import (
    CommandResult,
    Citation,
    EnrichmentRequest,
    EnrichmentResult,
    NoteDirective,
    Section,
    V1_COMMAND_ALLOWLIST,
)


FIXTURES_DIR = Path(__file__).parent / "fixtures" / "notes"


def test_enrichment_request_fixture():
    """Verify the shared request fixture parses correctly."""
    fixture = json.loads((FIXTURES_DIR / "note_enrichment_request.json").read_text())
    req = EnrichmentRequest.from_dict(fixture)

    assert req.schema_version == 1
    assert req.note_id == "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
    assert req.client_run_id == "f47ac10b-58cc-4372-a567-0e02b2c3d479"
    assert req.source_drawing_revision == 12
    assert req.source_text_revision == 8
    assert len(req.directives) == 2
    assert req.directives[0].command == "research"
    assert req.directives[1].command == "actions"
    assert req.locale == "en-US"
    assert req.timezone == "America/Los_Angeles"


def test_enrichment_result_fixture():
    """Verify the shared result fixture parses correctly."""
    fixture = json.loads((FIXTURES_DIR / "note_enrichment_result.json").read_text())
    result = EnrichmentResult.from_dict(fixture)

    assert result.schema_version == 1
    assert result.title == "Q3 Planning Meeting Notes"
    assert len(result.sections) == 3
    assert result.sections[0].kind == "summary"
    assert result.sections[1].kind == "command_result"
    assert len(result.command_results) == 2
    assert result.command_results[0].directive_id == "dir-001"
    assert result.command_results[0].status == "completed"
    assert len(result.citations) == 3
    assert result.warnings == []


def test_enrichment_result_roundtrip():
    """Fixture → from_dict → to_dict → from_dict preserves structure."""
    fixture = json.loads((FIXTURES_DIR / "note_enrichment_result.json").read_text())
    result = EnrichmentResult.from_dict(fixture)
    serialized = result.to_dict()
    restored = EnrichmentResult.from_dict(serialized)

    assert restored.title == result.title
    assert restored.markdown == result.markdown
    assert len(restored.sections) == len(result.sections)
    assert len(restored.command_results) == len(result.command_results)
    assert len(restored.citations) == len(result.citations)


def test_request_validation_passes():
    """Valid request passes validation."""
    req = EnrichmentRequest(
        schema_version=1,
        note_id="test-note",
        client_run_id="test-run",
        source_drawing_revision=1,
        source_text_revision=1,
        recognized_text="Hello world",
        directives=[],
    )
    assert req.validate() == []


def test_request_validation_fails_missing_fields():
    """Missing required fields fail validation."""
    req = EnrichmentRequest(
        schema_version=1,
        note_id="",
        client_run_id="",
        source_drawing_revision=0,
        source_text_revision=0,
        recognized_text="",
    )
    errors = req.validate()
    assert len(errors) == 3
    assert any("noteId" in e for e in errors)
    assert any("clientRunId" in e for e in errors)
    assert any("recognizedText" in e for e in errors)


def test_result_validation_passes():
    """Valid result passes validation."""
    result = EnrichmentResult(
        title="Test",
        markdown="# Test",
        sections=[Section(kind="summary", title="Summary", markdown="...")],
    )
    assert result.validate() == []


def test_result_validation_fails_empty_sections():
    """Empty sections fail validation."""
    result = EnrichmentResult(
        title="Test",
        markdown="# Test",
        sections=[],
    )
    errors = result.validate()
    assert any("sections" in e for e in errors)


def test_result_validation_fails_unknown_section_kind():
    """Unknown section kind fails validation."""
    result = EnrichmentResult(
        title="Test",
        markdown="# Test",
        sections=[Section(kind="unknown", title="Bad", markdown="...")],
    )
    errors = result.validate()
    assert any("Unknown section kind" in e for e in errors)


def test_v1_command_allowlist():
    """Verify the v1 command allowlist is correct."""
    expected = {"research", "search", "talkingpoints", "summary", "actions", "questions"}
    assert V1_COMMAND_ALLOWLIST == expected


def test_allowlist_filtering():
    """Only allowlisted commands pass through."""
    from herald_connector.note_contract import V1_COMMAND_ALLOWLIST

    directives = [
        NoteDirective(id="1", command="research", arguments="topic"),
        NoteDirective(id="2", command="unknown", arguments="bad"),
        NoteDirective(id="3", command="summary"),
    ]

    allowed = [d for d in directives if d.command.lower() in V1_COMMAND_ALLOWLIST]
    assert len(allowed) == 2
    assert allowed[0].command == "research"
    assert allowed[1].command == "summary"
