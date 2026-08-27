# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS build
ARG PNX264_RELEASE_URL
ARG PNX264_RELEASE_SHA256
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN test -n "$PNX264_RELEASE_URL" \
 && test -n "$PNX264_RELEASE_SHA256" \
 && mkdir -p /opt/pnx264 \
 && curl --fail --location --retry 3 "$PNX264_RELEASE_URL" -o /tmp/pnx264.tar.gz \
 && echo "$PNX264_RELEASE_SHA256  /tmp/pnx264.tar.gz" | sha256sum --check - \
 && tar -xzf /tmp/pnx264.tar.gz -C /opt/pnx264 \
 && test -f /opt/pnx264/include/x264.h \
 && test -f /opt/pnx264/lib/libx264.a \
 && rm /tmp/pnx264.tar.gz
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
