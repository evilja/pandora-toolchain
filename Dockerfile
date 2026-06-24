# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS build
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
 && apt-get install -y --no-install-recommends ca-certificates ffmpeg curl \
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
