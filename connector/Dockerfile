FROM python:3.11-slim

# Install procps for ps command (connector needs it)
RUN apt-get update && apt-get install -y --no-install-recommends procps && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy source and config
COPY src/ src/
COPY pyproject.toml .

# Install
RUN pip install --no-cache-dir .

# Create state directory
RUN mkdir -p /state

ENV HERMES_MOBILE_CONNECTOR_HOME=/state

CMD ["hermes-mobile", "run"]
