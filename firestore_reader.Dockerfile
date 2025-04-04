FROM rust:1-bookworm as builder

WORKDIR /app

RUN USER=root cargo new --bin firestore_reader
RUN USER=root cargo new --bin user_settings
RUN USER=root cargo new --bin database_schema

WORKDIR /app/firestore_reader

COPY ./firestore_reader/Cargo.toml /app/firestore_reader/Cargo.toml
COPY ./user_settings/Cargo.toml /app/user_settings/Cargo.toml
COPY ./database_schema/Cargo.toml /app/database_schema/Cargo.toml

RUN cargo build --release
RUN rm /app/firestore_reader/src/*.rs
RUN rm /app/user_settings/src/*.rs
RUN rm /app/database_schema/src/*.rs

ADD ./firestore_reader/. /app/firestore_reader
ADD ./user_settings/. /app/user_settings
ADD ./database_schema/. /app/database_schema
ADD ./migrations/. /app/migrations

RUN rm /app/firestore_reader/target/release/deps/firestore_reader*

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

COPY --from=builder /app/firestore_reader/target/release/firestore_reader ${APP}/firestore_reader

RUN chown -R $APP_USER:$APP_USER ${APP}

USER $APP_USER
WORKDIR ${APP}

CMD ["./firestore_reader"]