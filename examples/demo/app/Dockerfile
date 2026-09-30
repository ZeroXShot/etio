FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
# `opentelemetry-bootstrap` installs the instrumentations matching the
# installed libraries (Flask, requests, Redis, logging, system metrics).
RUN pip install --no-cache-dir -r requirements.txt \
    && opentelemetry-bootstrap -a requirements > /tmp/instrumentations.txt \
    && pip install --no-cache-dir -r /tmp/instrumentations.txt \
    && pip install --no-cache-dir "opentelemetry-instrumentation-system-metrics>=0.48b0"
COPY service.py loadgen.py ./
RUN useradd --uid 10001 --no-create-home app
USER 10001
CMD ["opentelemetry-instrument", "python", "service.py"]
