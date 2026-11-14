"""Backward-compatible imports for the pre-Herald API executor."""

from .herald_api_executor import *  # noqa: F403
from .herald_api_executor import HeraldAPIExecutor

HermesAPIExecutor = HeraldAPIExecutor
