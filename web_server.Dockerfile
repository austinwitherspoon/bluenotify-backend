FROM rust:1-bookworm as builder

WORKDIR /app

RUN USER=root cargo new --bin web_server
RUN USER=root cargo new --bin bluesky_utils
RUN USER=root cargo new --bin database_schema
RUN USER=root cargo new --bin user_settings

WORKDIR /app/web_server

COPY ./web_server/Cargo.toml /app/web_server/Cargo.toml
COPY ./database_schema/Cargo.toml /app/database_schema/Cargo.toml

RUN cargo build --release
RUN rm /app/web_server/src/*.rs
RUN rm /app/database_schema/src/*.rs

ADD ./web_server/. /app/web_server
ADD ./database_schema/. /app/database_schema
ADD ./migrations/. /app/migrations

RUN rm /app/web_server/target/release/deps/web_server*

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

COPY --from=builder /app/web_server/target/release/web_server ${APP}/web_server

RUN chown -R $APP_USER:$APP_USER ${APP}

USER $APP_USER
WORKDIR ${APP}

CMD ["./web_server"]