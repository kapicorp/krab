# Container image of a released krab binary (docs/specs/cli-and-release.md, CLI-63).
# The release workflow unpacks the Linux archives of the same run into
# bin/amd64/krab and bin/arm64/krab before it builds this file.
ARG DEBIAN=docker.io/library/debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# helm inputs call the helm binary. The release archive is checked against
# the checksum get.helm.sh publishes for it.
FROM ${DEBIAN} AS helm
ARG TARGETARCH
ARG HELM_VERSION=4.3.2
ARG HELM_SHA256_amd64=d1c0278d46beab79582d7e50f5b3dc206944ba8e577cde618513fd07daa53983
ARG HELM_SHA256_arm64=df31cb61680ef728f82c770e5280dd565617bee6b039d37e85c677288be4bbb0
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && curl -fsSLo /tmp/helm.tar.gz "https://get.helm.sh/helm-v${HELM_VERSION}-linux-${TARGETARCH}.tar.gz" \
    && eval "sum=\$HELM_SHA256_${TARGETARCH}" \
    && echo "${sum}  /tmp/helm.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/helm.tar.gz -C /tmp "linux-${TARGETARCH}/helm" \
    && mv "/tmp/linux-${TARGETARCH}/helm" /helm

FROM ${DEBIAN}
ARG TARGETARCH

# git for git dependencies, gpg for the gpg ref backend, python3 for kadet
# components and Python resolvers. The venv holds what kadet evaluation needs
# (pyenv.rs BASELINE), installed at build time: KRAB_PYTHON makes krab use it
# as it is, so nothing is installed at run time and the root filesystem can
# stay read-only.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git gnupg python3 python3-venv \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv /opt/krab/python \
    && /opt/krab/python/bin/pip install --no-cache-dir --disable-pip-version-check kadet==0.3.2 jinja2==3.1.6 \
    && groupadd --gid 999 krab \
    && useradd --uid 999 --gid 999 --create-home --shell /usr/sbin/nologin krab

COPY --from=helm /helm /usr/local/bin/helm
COPY --chmod=0755 bin/${TARGETARCH}/krab /usr/local/bin/krab
COPY LICENSE README.md /usr/share/doc/krab/

ENV KRAB_PYTHON=/opt/krab/python/bin/python

# Argo CD runs config management plugins as user 999.
USER 999:999
WORKDIR /home/krab
ENTRYPOINT ["krab"]
