# Use an official Python runtime as a parent image
FROM python:3.13

RUN pip install uv

WORKDIR /app

COPY pyproject.toml /app/pyproject.toml
COPY uv.lock /app/uv.lock
COPY .python-version /app/.python-version

COPY apps/jetstream-reader/pyproject.toml /app/apps/jetstream-reader/pyproject.toml

COPY packages/ /app/packages/

RUN uv sync --no-sources --package jetstream-reader

COPY apps/jetstream-reader /app/apps/jetstream-reader

WORKDIR /app/apps/jetstream-reader

# Run the server
CMD ["uv", "run", "--no-sync", "run_server.py"]
