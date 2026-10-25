# mw2-relay in a container: a static binary on an empty image.
#
#   docker build -t mw2-relay relay/
#   docker run -d --restart unless-stopped -p 3074:3074/udp -e MW2_RELAY_PASSWORD=secret mw2-relay
#
# The release workflow builds it for amd64 and arm64 and pushes it to
# ghcr.io/paulcombal/mw2-relay, tagged with the release.
FROM alpine:3.20 AS build
RUN apk add --no-cache g++
COPY protocol.h relay.cpp /src/
RUN g++ -std=c++20 -O2 -Wall -static -o /mw2-relay /src/relay.cpp && strip /mw2-relay

FROM scratch
COPY --from=build /mw2-relay /mw2-relay
USER 65534:65534
EXPOSE 3074/udp
ENTRYPOINT ["/mw2-relay"]
