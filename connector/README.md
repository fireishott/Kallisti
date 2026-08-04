# Kallisti Connector

The host-side bridge between the Kallisti iOS app and your local Hermes agent. It owns the WebSocket connection to the gateway, executes Hermes work on the host, exposes phone context through MCP, and keeps the host available when no terminal window is open.

## What the connector does

- Connects to the Hermes gateway over an authenticated WebSocket
- Runs Hermes jobs on the host through the CLI or configured API runtime
- Registers the local MCP server in `~/.hermes/config.yaml`
- Stores sensor data in `~/.hermes-mobile/state/sensors.db`
- Handles native speech (STT/TTS) via the Hermes speech stack
- Installs a background service on macOS and WSL2

## Prerequisites

- Python 3.11+
- A working Hermes installation (`hermes --version` should succeed)
- ffmpeg (for TTS audio normalization)

## Install

```bash
cd connector
python -m venv .venv
source .venv/bin/activate
pip install -e .[dev]
```

## Setup

### 1. Run the setup wizard

```bash
kallisti setup
```

The wizard will:
- Validate your Hermes installation
- Configure the gateway connection
- Register MCP tools
- Install the background service

### 2. Pair your phone

```bash
kallisti pair-phone
```

This prints an ASCII QR code and a short manual code. Open Kallisti on your iPhone and scan or enter the code.

### 3. Start the service

```bash
kallisti service install
kallisti service start
```

## Speech (Talk mode)

Kallisti uses Hermes's native speech stack — faster-whisper for STT and edge-tts for TTS. Both run on the host with zero API keys.

Configure in your Hermes profile (`~/.hermes/profiles/<profile>/config.yaml`):

```yaml
stt:
  enabled: true
  provider: local
tts:
  provider: edge
  edge:
    voice: en-US-AvaMultilingualNeural
```

## Commands

```bash
kallisti setup              # Interactive setup wizard
kallisti pair-phone         # Generate pairing code
kallisti run                # Start the connector (foreground)
kallisti service install    # Install background service
kallisti service start      # Start background service
kallisti service status     # Check service status
kallisti service logs       # View service logs
kallisti configure-mcp      # Register MCP tools
kallisti status             # Show connector status
```

## Architecture

```
┌─────────────┐     HTTP/WS      ┌──────────────┐
│   Kallisti   │ ◄──────────────► │  Connector   │
│   (iOS App)  │                  │  (Python)    │
└─────────────┘                  └──────┬───────┘
                                        │
                                 ┌──────▼───────┐
                                 │ Hermes Agent │
                                 │  (Server)    │
                                 └──────────────┘
```

The connector runs an HTTP facade (port 8010) for the iOS app and a WebSocket server (port 8765) for the Hermes gateway. Both are fronted by Caddy in production.
