FROM rust:1-bookworm as builder

WORKDIR /app

RUN USER=root cargo new --bin jetstream_reader
RUN USER=root cargo new --bin bluesky_utils

WORKDIR /app/jetstream_reader

COPY ./jetstream_reader/Cargo.toml /app/jetstream_reader/Cargo.toml
COPY ./bluesky_utils/Cargo.toml /app/bluesky_utils/Cargo.toml

RUN cargo build --release
RUN rm /app/jetstream_reader/src/*.rs
RUN rm /app/bluesky_utils/src/*.rs

ADD ./jetstream_reader/. /app/jetstream_reader
ADD ./bluesky_utils/. /app/bluesky_utils

RUN rm /app/jetstream_reader/target/release/deps/jetstream_reader*

RUN cargo build --release


FROM debian:bookworm
ARG APP=/usr/src/app

RUN apt-get update \
    && apt-get install -y ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

EXPOSE 8000

ENV TZ=Etc/UTC \
    APP_USER=appuser

RUN groupadd $APP_USER \
    && useradd -g $APP_USER $APP_USER \
    && mkdir -p ${APP}

COPY --from=builder /app/jetstream_reader/target/release/jetstream_reader ${APP}/jetstream_reader

RUN chown -R $APP_USER:$APP_USER ${APP}

USER $APP_USER
WORKDIR ${APP}

CMD ["./jetstream_reader"]