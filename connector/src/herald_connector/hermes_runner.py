"""Backward-compatible imports for the pre-Herald connector API."""

from dataclasses import dataclass

from .herald_runner import *  # noqa: F403
from .herald_runner import HeraldCLIExecutor, HeraldConversationMessage


@dataclass(frozen=True)
class ConnectorHermesSettings:
    hermes_command: str
    hermes_workdir: str | None
    hermes_provider: str | None
    hermes_model: str | None
    hermes_toolsets: str | None
    hermes_source: str
    hermes_history_limit: int

    @property
    def herald_command(self):
        return self.hermes_command

    @property
    def herald_workdir(self):
        return self.hermes_workdir

    @property
    def herald_provider(self):
        return self.hermes_provider

    @property
    def herald_model(self):
        return self.hermes_model

    @property
    def herald_toolsets(self):
        return self.hermes_toolsets

    @property
    def herald_source(self):
        return self.hermes_source

    @property
    def herald_history_limit(self):
        return self.hermes_history_limit


HermesCLIExecutor = HeraldCLIExecutor
HermesConversationMessage = HeraldConversationMessage
