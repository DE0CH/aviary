FROM opencloudops/nginx@sha256:8ca0c90c2f537fef6195ba8f5ebe6d6c00fe0a0c545f25ce8acd418a831a19a4

LABEL maintainer="OpenCloud GmbH <info@opencloud.eu>" \
  org.opencontainers.image.title="OpenCloud Web" \
  org.opencontainers.image.vendor="OpenCloud GmbH" \
  org.opencontainers.image.authors="OpenCloud GmbH" \
  org.opencontainers.image.description="OpenCloud Web - User interface for OpenCloud" \
  org.opencontainers.image.licenses="AGPL-3.0" \
  org.opencontainers.image.documentation="https://github.com/opencloud-eu/web" \
  org.opencontainers.image.url="https://hub.docker.com/r/opencloud-eu/web" \
  org.opencontainers.image.source="https://github.com/opencloud-eu/web"

RUN rm -f /var/lib/nginx/html/*

ADD docker/overlay /
ADD dist/ /var/lib/nginx/html

EXPOSE 8080

USER nginx

STOPSIGNAL SIGTERM

CMD ["nginx", "-g", "daemon off;"]
WORKDIR /var/lib/nginx/html
