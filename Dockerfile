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

# ffmpeg compiled for the CPU this image is built on. FFMPEG_NATIVE=1 runs scripts/build-ffmpeg.sh
# here — ffmpeg, x264, x265 and libass from pinned sources with -march=native — and the runtime
# stage below then carries that pair instead of Debian's generic build. The stage is built on the
# same bookworm base as the runtime so the glibc and the freetype/fontconfig/harfbuzz/fribidi it
# links against are the ones the runtime image has.
#
# Build the image ON the machine that will run it: -march=native means this host's CPU, and an
# image built on one box may not start on another. docker-compose.yml passes FFMPEG_NATIVE=1, so
# the watcher-driven rebuild after a /gitsync does this without anyone logging in; a plain
# `docker build` defaults to 0, where the stage does nothing and the runtime keeps Debian's ffmpeg.
# The build layer is cached on the script's content, so a `/gitsync`-triggered image rebuild only
# recompiles ffmpeg when scripts/build-ffmpeg.sh itself changed.
FROM debian:bookworm-slim AS ffmpeg-native
ARG FFMPEG_NATIVE=0
RUN if [ "$FFMPEG_NATIVE" = "1" ]; then \
      apt-get update \
   && apt-get install -y --no-install-recommends \
        ca-certificates build-essential cmake pkg-config curl xz-utils git nasm \
        libfreetype-dev libfontconfig-dev libharfbuzz-dev libfribidi-dev zlib1g-dev \
   && rm -rf /var/lib/apt/lists/*; \
    fi
COPY scripts/build-ffmpeg.sh /tmp/build-ffmpeg.sh
RUN mkdir -p /opt/ffmpeg-native \
 && if [ "$FFMPEG_NATIVE" = "1" ]; then \
      PANDORA_FFMPEG_OUT=/opt/ffmpeg-native PANDORA_FFMPEG_WORK=/tmp/ffmpeg-work bash /tmp/build-ffmpeg.sh \
   && rm -rf /tmp/ffmpeg-work; \
    fi

FROM debian:bookworm-slim AS runtime
ARG FFMPEG_NATIVE=0
# FFMPEG_TOOLCHAIN=1 adds what `pndc --build-ffmpeg` / `/build-ffmpeg` need to compile inside the
# running container, into the mounted DB/bin where the result survives image rebuilds. It is
# separate from FFMPEG_NATIVE — an image can carry a baked native pair and no compiler, or the
# other way round — and off by default because it grows the image by a few hundred MB.
ARG FFMPEG_TOOLCHAIN=0
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl fontconfig \
 && if [ "$FFMPEG_NATIVE" = "1" ]; then \
      apt-get install -y --no-install-recommends libfreetype6 libfontconfig1 libharfbuzz0b libfribidi0 zlib1g libstdc++6; \
    else \
      apt-get install -y --no-install-recommends ffmpeg; \
    fi \
 && if [ "$FFMPEG_TOOLCHAIN" = "1" ]; then \
      apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config xz-utils git nasm \
        libfreetype-dev libfontconfig-dev libharfbuzz-dev libfribidi-dev zlib1g-dev; \
    fi \
 && rm -rf /var/lib/apt/lists/*
# Empty unless FFMPEG_NATIVE=1; the record lands where `pndc` looks for it at startup.
COPY --from=ffmpeg-native /opt/ffmpeg-native/ /opt/ffmpeg-native/
RUN if [ -x /opt/ffmpeg-native/ffmpeg ]; then \
      install -m 755 /opt/ffmpeg-native/ffmpeg /opt/ffmpeg-native/ffprobe /usr/local/bin/ \
   && mkdir -p /usr/local/share/pandora \
   && install -m 644 /opt/ffmpeg-native/ffmpeg.build /usr/local/share/pandora/ffmpeg.build \
   && /usr/local/bin/ffmpeg -version | head -1; \
    fi \
 && rm -rf /opt/ffmpeg-native
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
