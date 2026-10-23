# A RustDesk peer for end-to-end tests: the official Linux build under Xvfb
# with an xterm and PulseAudio playing a tone, configured for the test
# hbbs/hbbr by start.sh. The .deb is downloaded unless a rustdesk.deb is
# placed next to this file. RustDesk captures sound in its connection
# manager, a Flutter program (hence EGL) that it starts with sudo once a
# user is logged in, which `loginctl` here says root is.
FROM ubuntu:24.04
ARG RUSTDESK_VERSION=1.5.0
ENV DEBIAN_FRONTEND=noninteractive
COPY . /ctx/
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl xvfb x11-utils xterm openbox \
      xclip xdotool dbus-x11 libayatana-appindicator3-1 procps pulseaudio pulseaudio-utils \
      sudo libegl1 libegl-mesa0 libgl1-mesa-dri \
 && if [ ! -f /ctx/rustdesk.deb ]; then curl -fsSL -o /ctx/rustdesk.deb \
      "https://github.com/rustdesk/rustdesk/releases/download/${RUSTDESK_VERSION}/rustdesk-${RUSTDESK_VERSION}-x86_64.deb"; fi \
 && apt-get install -y --no-install-recommends /ctx/rustdesk.deb \
 && install -m 755 /ctx/start.sh /start.sh \
 && install -m 755 /ctx/loginctl /usr/bin/loginctl \
 && printf 'default-server = unix:/tmp/pulse/native\nautospawn = no\n' >> /etc/pulse/client.conf \
 && rm -rf /var/lib/apt/lists/* /ctx
CMD ["/start.sh"]
