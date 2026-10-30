# mw2-relay in a container: a static binary on an empty image.
#
#   docker build -t mw2-relay relay/
#   docker run -d --restart unless-stopped -p 3074:3074/udp -e MW2_RELAY_PASSWORD=secret mw2-relay
#
# The image holds playlists.info, what Find Game offers; another file mounted
# in the container and named with --playlists replaces it.
#
# The release workflow builds it for amd64 and pushes it to
# ghcr.io/paulcombal/mw2-relay, tagged with the release.
FROM rust:1.97-alpine AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked && cp target/release/mw2-relay /mw2-relay

FROM scratch
COPY --from=build /mw2-relay /mw2-relay
COPY playlists.info /playlists.info
USER 65534:65534
EXPOSE 3074/udp
ENTRYPOINT ["/mw2-relay", "--playlists", "/playlists.info"]
