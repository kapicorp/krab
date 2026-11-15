# Container image of a released krab binary (docs/specs/cli-and-release.md, CLI-63).
# The release workflow unpacks the Linux archives of the same run into
# bin/amd64/krab and bin/arm64/krab before it builds this file.
FROM docker.io/library/debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

ARG TARGETARCH

# git for git dependencies, gpg for the gpg ref backend
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git gnupg \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 999 krab \
    && useradd --uid 999 --gid 999 --create-home --shell /usr/sbin/nologin krab

COPY --chmod=0755 bin/${TARGETARCH}/krab /usr/local/bin/krab
COPY LICENSE README.md /usr/share/doc/krab/

# Argo CD runs config management plugins as user 999.
USER 999:999
WORKDIR /home/krab
ENTRYPOINT ["krab"]
