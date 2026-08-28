# syntax=docker/dockerfile:1
# Runtime image for klepto serve (Docker + macOS `container`).
# Expects a prebuilt Linux binary at .oci/klepto (staged by scripts/oci.sh).

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    tmux \
    ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL https://omp.sh/install | sh \
    && ln -sf /root/.local/bin/omp /usr/local/bin/omp || true

COPY .oci/klepto /usr/local/bin/klepto
RUN chmod +x /usr/local/bin/klepto

ENV KLEPTO_LISTEN=0.0.0.0:7420
ENV KLEPTO_IN_OCI=1
ENV HOME=/home/klepto
RUN useradd --create-home --shell /bin/bash --uid 1000 klepto \
    && mkdir -p /home/klepto/.klepto /home/klepto/.omp/agent /home/klepto/.local/bin \
    && if [ -x /root/.local/bin/omp ]; then cp /root/.local/bin/omp /usr/local/bin/omp; fi \
    && chown -R klepto:klepto /home/klepto

USER klepto
WORKDIR /home/klepto
EXPOSE 7420

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:7420/v1/health" || exit 1

ENTRYPOINT ["klepto"]
CMD ["serve", "--listen", "0.0.0.0:7420"]
