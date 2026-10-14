FROM debian:bookworm-slim

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/sh yukinal \
    && passwd -d yukinal \
    && install -d -o yukinal -g yukinal -m 0700 /home/yukinal/.ssh \
    && mkdir -p /run/sshd \
    && ssh-keygen -A

COPY sshd_config /etc/ssh/sshd_config
COPY fake-systemctl /usr/local/bin/systemctl
RUN chmod 0755 /usr/local/bin/systemctl

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
