# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS build
ARG PNX264_SOURCE_URL=https://github.com/evilja/x264-pandora/archive/2ecc6f52ab6946962667146d3d69dbff42e881f9.tar.gz
ARG PNX264_SOURCE_SHA256=97355f37274264d40a72f69f67c0dd0a036abea13ecf6cf8e61a7af65a9ba80e
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates build-essential nasm \
 && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /tmp/pnx264-source /opt/pnx264 \
 && curl --fail --location --retry 3 "$PNX264_SOURCE_URL" -o /tmp/pnx264-source.tar.gz \
 && echo "$PNX264_SOURCE_SHA256  /tmp/pnx264-source.tar.gz" | sha256sum --check - \
 && tar -xzf /tmp/pnx264-source.tar.gz -C /tmp/pnx264-source --strip-components=1 \
 && test "$(grep -c '^#define X264_PANDORA_PLAN_ONLY 1$' /tmp/pnx264-source/x264.h)" -eq 1 \
 && ! grep -n 'plan-only: lookahead_threads' /tmp/pnx264-source/encoder/encoder.c /tmp/pnx264-source/x264.h \
 && cd /tmp/pnx264-source \
 && ./configure \
      --prefix=/opt/pnx264 \
      --enable-static \
      --disable-shared \
      --disable-cli \
      --disable-opencl \
      --bit-depth=all \
 && make -j"$(nproc)" \
 && make install \
 && test -f /opt/pnx264/include/x264.h \
 && test -f /opt/pnx264/lib/libx264.a \
 && ! nm -u /opt/pnx264/lib/libx264.a | grep -q '__isoc23_' \
 && rm -rf /tmp/pnx264-source /tmp/pnx264-source.tar.gz
ENV PNX264_INCLUDE_DIR=/opt/pnx264/include
ENV PNX264_LIB_DIR=/opt/pnx264/lib
ENV PNX264_STATIC=1
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=pandora-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pandora-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=pandora-target,target=/src/target \
    cargo build --release --bins \
    && mkdir -p /out \
    && cp target/release/pndc target/release/pnmpeg target/release/pnp2p target/release/pncurl target/release/pnass /out/

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates ffmpeg curl fontconfig \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
ENV PANDORA_GITSYNC_REPO=/repo
ENV PANDORA_GITSYNC_REQUEST=/app/DB/gitsync.request
COPY --from=build /out/pndc   /usr/local/bin/pndc
COPY --from=build /out/pnmpeg  /usr/local/bin/pnmpeg
COPY --from=build /out/pnp2p   /usr/local/bin/pnp2p
COPY --from=build /out/pncurl  /usr/local/bin/pncurl
COPY --from=build /out/pnass   /usr/local/bin/pnass
# DB/ (database, env.pandora, api.pandora tokens) comes from a mounted volume.
EXPOSE 8787
CMD ["pndc"]
