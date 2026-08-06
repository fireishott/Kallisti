"""Note enrichment request/response contract.

Schema-validated in both directions. Shared fixtures from N0.4.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class NoteDirective:
    id: str
    command: str
    arguments: str = ""
    source_range: dict[str, int] | None = None


@dataclass
class EnrichmentRequest:
    schema_version: int
    note_id: str
    client_run_id: str
    source_drawing_revision: int
    source_text_revision: int
    recognized_text: str
    directives: list[NoteDirective] = field(default_factory=list)
    locale: str = "en-US"
    timezone: str = "America/Los_Angeles"

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> EnrichmentRequest:
        directives = [
            NoteDirective(
                id=d.get("id", ""),
                command=d.get("command", ""),
                arguments=d.get("arguments", ""),
                source_range=d.get("sourceRange"),
            )
            for d in data.get("directives", [])
        ]
        return cls(
            schema_version=data.get("schemaVersion", 1),
            note_id=data.get("noteId", ""),
            client_run_id=data.get("clientRunId", ""),
            source_drawing_revision=data.get("sourceDrawingRevision", 0),
            source_text_revision=data.get("sourceTextRevision", 0),
            recognized_text=data.get("recognizedText", ""),
            directives=directives,
            locale=data.get("locale", "en-US"),
            timezone=data.get("timezone", "America/Los_Angeles"),
        )

    def validate(self) -> list[str]:
        """Validate the request. Returns list of errors (empty = valid)."""
        errors = []
        if not self.note_id:
            errors.append("noteId is required")
        if not self.client_run_id:
            errors.append("clientRunId is required")
        if not self.recognized_text:
            errors.append("recognizedText is required")
        for d in self.directives:
            if not d.command:
                errors.append(f"Directive {d.id} has empty command")
        return errors


@dataclass
class Section:
    kind: str
    title: str
    markdown: str


@dataclass
class CommandResult:
    directive_id: str
    status: str
    section_index: int | None = None


@dataclass
class Citation:
    title: str
    url: str
    accessed_at: str


@dataclass
class EnrichmentResult:
    schema_version: int = 1
    title: str = ""
    markdown: str = ""
    sections: list[Section] = field(default_factory=list)
    command_results: list[CommandResult] = field(default_factory=list)
    citations: list[Citation] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "schemaVersion": self.schema_version,
            "title": self.title,
            "markdown": self.markdown,
            "sections": [
                {"kind": s.kind, "title": s.title, "markdown": s.markdown}
                for s in self.sections
            ],
            "commandResults": [
                {
                    "directiveId": cr.directive_id,
                    "status": cr.status,
                    **({"sectionIndex": cr.section_index} if cr.section_index is not None else {}),
                }
                for cr in self.command_results
            ],
            "citations": [
                {"title": c.title, "url": c.url, "accessedAt": c.accessed_at}
                for c in self.citations
            ],
            "warnings": self.warnings,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> EnrichmentResult:
        sections = [
            Section(kind=s.get("kind", ""), title=s.get("title", ""), markdown=s.get("markdown", ""))
            for s in data.get("sections", [])
        ]
        command_results = [
            CommandResult(
                directive_id=cr.get("directiveId", ""),
                status=cr.get("status", ""),
                section_index=cr.get("sectionIndex"),
            )
            for cr in data.get("commandResults", [])
        ]
        citations = [
            Citation(title=c.get("title", ""), url=c.get("url", ""), accessed_at=c.get("accessedAt", ""))
            for c in data.get("citations", [])
        ]
        return cls(
            schema_version=data.get("schemaVersion", 1),
            title=data.get("title", ""),
            markdown=data.get("markdown", ""),
            sections=sections,
            command_results=command_results,
            citations=citations,
            warnings=data.get("warnings", []),
        )

    def validate(self) -> list[str]:
        """Validate the result. Returns list of errors (empty = valid)."""
        errors = []
        if not self.title:
            errors.append("title is required")
        if not self.markdown:
            errors.append("markdown is required")
        if not self.sections:
            errors.append("sections is required and must not be empty")
        for s in self.sections:
            if s.kind not in ("summary", "command_result", "freeform"):
                errors.append(f"Unknown section kind: {s.kind}")
        return errors


# v1 command allowlist — enforced relay-side before dispatch
V1_COMMAND_ALLOWLIST = frozenset({
    "research",
    "search",
    "talkingpoints",
    "summary",
    "actions",
    "questions",
})
