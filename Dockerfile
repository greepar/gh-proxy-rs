ARG TARGETARCH
FROM debian:bookworm-slim
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home gh-proxy
COPY dist/${TARGETARCH}/gh-proxy /usr/local/bin/gh-proxy
USER gh-proxy
EXPOSE 1555
ENTRYPOINT ["/usr/local/bin/gh-proxy"]
