# Use an official Python runtime as a parent image
FROM python:3.13

RUN pip install uv

WORKDIR /app

COPY pyproject.toml /app/pyproject.toml
COPY uv.lock /app/uv.lock
COPY .python-version /app/.python-version

COPY apps/manual-polling/pyproject.toml /app/apps/manual-polling/pyproject.toml

COPY packages/ /app/packages/

RUN uv sync --no-sources --package manual-polling

COPY apps/manual-polling /app/apps/manual-polling

WORKDIR /app/apps/manual-polling

# Run the server
CMD ["uv", "run", "--no-sync", "run_server.py"]
