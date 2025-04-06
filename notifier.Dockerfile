FROM rust:1-bookworm as builder

WORKDIR /app

RUN USER=root cargo new --bin notifier
RUN USER=root cargo new --bin bluesky_utils
RUN USER=root cargo new --bin database_schema

WORKDIR /app/notifier

COPY ./notifier/Cargo.toml /app/notifier/Cargo.toml
COPY ./bluesky_utils/Cargo.toml /app/bluesky_utils/Cargo.toml
COPY ./database_schema/Cargo.toml /app/database_schema/Cargo.toml

RUN cargo build --release
RUN rm /app/notifier/src/*.rs
RUN rm /app/bluesky_utils/src/*.rs
RUN rm /app/database_schema/src/*.rs

ADD ./notifier/. /app/notifier
ADD ./bluesky_utils/. /app/bluesky_utils
ADD ./database_schema/. /app/database_schema
ADD ./migrations/. /app/migrations

RUN rm /app/notifier/target/release/deps/notifier*

RUN cargo build --release


FROM debian:bookworm
ARG APP=/usr/src/app

RUN apt-get update \
    && apt-get install -y ca-certificates tzdata libpq5 \
    && rm -rf /var/lib/apt/lists/*

EXPOSE 8000

ENV TZ=Etc/UTC \
    APP_USER=appuser

RUN groupadd $APP_USER \
    && useradd -g $APP_USER $APP_USER \
    && mkdir -p ${APP}

COPY --from=builder /app/notifier/target/release/notifier ${APP}/notifier

RUN chown -R $APP_USER:$APP_USER ${APP}

USER $APP_USER
WORKDIR ${APP}

CMD ["./notifier"]