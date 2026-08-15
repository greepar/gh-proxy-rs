ARG TARGETARCH
FROM alpine:3.22
ARG TARGETARCH
RUN apk add --no-cache ca-certificates \
    && adduser -S -D -H gh-proxy
COPY dist/${TARGETARCH}/gh-proxy /usr/local/bin/gh-proxy
USER gh-proxy
EXPOSE 1555
ENTRYPOINT ["/usr/local/bin/gh-proxy"]
