FROM python:3.12-slim

WORKDIR /app

COPY pyproject.toml ./
COPY app ./app

RUN pip install --no-cache-dir .

EXPOSE 8000

CMD ["uvicorn", "app.main:app", \
     "--host", "0.0.0.0", \
     "--port", "8000", \
     "--log-level", "info", \
     "--ws-max-size", "52428800", \
     "--timeout-keep-alive", "75", \
     "--timeout-graceful-shutdown", "20", \
     "--ws-ping-interval", "20", \
     "--ws-ping-timeout", "20", \
     "--limit-concurrency", "400", \
     "--proxy-headers", \
     "--forwarded-allow-ips", "*"]
