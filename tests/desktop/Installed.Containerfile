ARG BASE_IMAGE
FROM ${BASE_IMAGE}
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]
# Freeze every existing RPM: test tooling must not upgrade the image under test.
RUN rpm -qa --qf '%{NEVRA}\n' | sort > /tmp/production-rpms; \
    excluded="$(rpm -qa --qf '%{NAME}\n' | sort -u | paste -sd, -)"; \
    dnf -y --exclude="$excluded" install seatd waybar mako foot python3-gobject desktop-file-utils; \
    rpm -qa --qf '%{NEVRA}\n' | sort > /tmp/test-rpms; \
    test -z "$(comm -23 /tmp/production-rpms /tmp/test-rpms)"; \
    if ! id tester >/dev/null 2>&1; then useradd -m -u 1000 tester; fi; \
    printf 'account required pam_permit.so\nsession required pam_systemd.so\n' >/etc/pam.d/rustle-test; \
    install -d -o tester -g tester /artifacts /models; \
    mkdir -p /candidate/usr/bin; cp /usr/bin/rustle /candidate/usr/bin/rustle; \
    systemctl mask --force systemd-udevd.service systemd-udevd-control.socket systemd-udevd-kernel.socket \
      systemd-resolved.service sddm.service display-manager.service
COPY scripts/verify-install.py /opt/rustle-test/
COPY tests/desktop /opt/rustle-test/desktop
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
