# Use an official Python runtime as a parent image
FROM python:3.13

RUN pip install uv

WORKDIR /app

COPY pyproject.toml /app/pyproject.toml
COPY uv.lock /app/uv.lock
COPY .python-version /app/.python-version

COPY apps/notifier/pyproject.toml /app/apps/notifier/pyproject.toml

COPY packages/ /app/packages/

RUN uv sync --no-sources --package notifier

COPY apps/notifier /app/apps/notifier

# Run the server
CMD ["uv", "run", "--no-sync", "/app/apps/notifier/run_server.py"]
