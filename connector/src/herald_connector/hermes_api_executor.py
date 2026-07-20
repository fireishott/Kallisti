"""Backward-compatible imports for the pre-Herald API executor."""

from .herald_api_executor import *  # noqa: F403
from .herald_api_executor import HeraldAPIExecutor, _could_be_marker_prefix

HermesAPIExecutor = HeraldAPIExecutor
