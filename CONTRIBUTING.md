# Contributing to Hermes iOS

Thanks for your interest in contributing! This project is an independent iOS companion app for the [Hermes Agent](https://github.com/NousResearch/hermes-agent) framework, with a self-hosted relay and connector architecture.

## Getting Started

1. Read the [README](README.md) for architecture overview
2. Read [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for env var reference
3. Set up local development:
   - **Relay:** `cd relay && pip install -e . && uvicorn app.main:app`
   - **Connector:** `cd connector && pip install -e . && hermes-mobile setup`
   - **iOS App:** Open `Herald.xcodeproj` in Xcode 26+

## Development Requirements

- **iOS App:** Xcode 26+, iOS 26 SDK, Swift 6.2
- **Relay:** Python 3.12+
- **Connector:** Python 3.11+
- **Hermes Agent:** Installed locally with MCP support

## Running Tests

```bash
# Connector (78 tests)
cd connector && pip install -e ".[dev]" && pytest tests/

# Relay (44 tests)
cd relay && pip install -e ".[dev]" && pytest tests/

# iOS (Xcode)
xcodebuild test -project Herald.xcodeproj -scheme Herald \
  -destination 'platform=iOS Simulator,name=iPhone 17'
```

## Pull Request Guidelines

- Keep PRs focused — one feature or fix per PR
- Include tests for new functionality
- Run the full test suite before submitting
- Update docs if you change env vars, APIs, or MCP tools
- Don't commit secrets, personal paths, or hardcoded URLs

## Code Style

- **Swift:** Follow existing patterns (protocol-oriented services, `@MainActor` isolation, strict concurrency)
- **Python:** Standard formatting, type hints on public APIs, docstrings on MCP tools

## Architecture Notes

The codebase has three independent pieces that communicate over the network:

```
iOS App ──HTTP/SSE──▶ Relay ◀──WebSocket──▶ Connector ──▶ Hermes Agent
```

- **Relay** is stateless and deployable anywhere (Fly.io, Railway, your own server)
- **Connector** runs alongside the Hermes Agent on the user's machine
- **iOS App** connects to the relay via the URL configured during onboarding

Changes to wire protocols (WebSocket messages, SSE events, REST endpoints) require coordination across all three. Changes within a single component (UI, MCP tools, sensor storage) can be developed independently.

## Reporting Issues

Please use GitHub Issues. Include:
- Which component (iOS app, relay, connector)
- Steps to reproduce
- Relevant logs (strip personal info)
- Your deployment setup (self-hosted relay URL, Hermes version)
