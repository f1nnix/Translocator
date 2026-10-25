#!/bin/sh
# Throwaway Linux servers for the SSH plugin's end-to-end tests (Docker).
#   scripts/ssh-test-servers.sh up            build the images, start the containers
#   scripts/ssh-test-servers.sh test [ARGS]   cargo test -p tl-ssh against each server
#   scripts/ssh-test-servers.sh down          remove the containers and images
#
# debian (127.0.0.1:2201): GNU coreutils, findutils and tar; bash login shell.
# alpine (127.0.0.1:2202): BusyBox ash, find, stat and tar.
#
# Both have a user "tester" with your uid and gid, key-only ssh login and a
# password for sudo. $dir/tmp is mounted at the same absolute path inside the
# containers and the tests create their temp folders there, so they can check
# results on this machine's disk. ssh gets its own key and known_hosts in
# $dir and ignores ~/.ssh/config, so nothing in ~/.ssh is written.
set -eu
cd "$(dirname "$0")/.."

dir=${TL_SSH_TEST_DIR:-/tmp/tl-ssh-e2e}
mkdir -p "$dir/tmp"
dir=$(cd "$dir" && pwd -P) # /tmp → /private/tmp on macOS
password=tl-sudo-secret
# name:port:dialect the probe must detect
servers="debian:2201:gnu alpine:2202:statc"
extra="-F /dev/null -o 'UserKnownHostsFile=$dir/known_hosts' -o GlobalKnownHostsFile=/dev/null \
-o StrictHostKeyChecking=yes -o IdentitiesOnly=yes"

dockerfile() {
  case $1 in
    debian) cat <<'EOF'
FROM debian:trixie-slim
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server sudo \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i 's/^session\s\+required\s\+pam_loginuid.so/session optional pam_loginuid.so/' /etc/pam.d/sshd
ARG UID GID PASSWORD
RUN (getent group "$GID" >/dev/null || groupadd -g "$GID" tester) \
 && useradd -m -u "$UID" -g "$GID" -s /bin/bash tester
EOF
      ;;
    alpine) cat <<'EOF'
FROM alpine:3.22
RUN apk add --no-cache openssh-server sudo
ARG UID GID PASSWORD
RUN g=$(awk -F: -v g="$GID" '$3 == g { print $1 }' /etc/group) \
 && if [ -z "$g" ]; then g=tester; addgroup -g "$GID" tester; fi \
 && adduser -D -u "$UID" -G "$g" -s /bin/ash tester
EOF
      ;;
  esac
  cat <<'EOF'
RUN echo "tester:$PASSWORD" | chpasswd \
 && echo 'tester ALL=(ALL:ALL) ALL' > /etc/sudoers.d/tester \
 && chmod 440 /etc/sudoers.d/tester \
 && ssh-keygen -A
CMD h=/home/tester/.ssh && mkdir -p $h /run/sshd && echo "$AUTHORIZED_KEY" > $h/authorized_keys \
 && chown -R tester $h && chmod 700 $h && chmod 600 $h/authorized_keys \
 && exec /usr/sbin/sshd -D -e -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no
EOF
}

field() { echo "$1" | cut -d: -f"$2"; }

up() {
  [ -f "$dir/key" ] || ssh-keygen -q -t ed25519 -N '' -C tl-ssh-test -f "$dir/key"
  : > "$dir/known_hosts"
  for s in $servers; do
    name=$(field "$s" 1) port=$(field "$s" 2)
    echo "building tl-ssh-test:$name"
    dockerfile "$name" | docker build -q -t "tl-ssh-test:$name" --build-arg UID="$(id -u)" \
      --build-arg GID="$(id -g)" --build-arg PASSWORD="$password" - >/dev/null
    docker rm -f "tl-ssh-$name" >/dev/null 2>&1 || true
    docker run -d --name "tl-ssh-$name" -p "127.0.0.1:$port:22" -v "$dir/tmp:$dir/tmp" \
      -e AUTHORIZED_KEY="$(cat "$dir/key.pub")" "tl-ssh-test:$name" >/dev/null
    hostkey=$(docker exec "tl-ssh-$name" cat /etc/ssh/ssh_host_ed25519_key.pub | cut -d' ' -f1,2)
    echo "[127.0.0.1]:$port $hostkey" >> "$dir/known_hosts"
  done
  for s in $servers; do
    name=$(field "$s" 1) port=$(field "$s" 2) tries=0
    until eval "ssh $extra -o BatchMode=yes -i \"\$dir/key\" -p $port tester@127.0.0.1 true" 2>/dev/null; do
      tries=$((tries + 1))
      if [ $tries -ge 50 ]; then echo "$name: sshd did not come up" >&2; exit 1; fi
      sleep 0.2
    done
    echo "$name: ssh tester@127.0.0.1 -p $port"
  done
}

run_tests() {
  failed=""
  for s in $servers; do
    name=$(field "$s" 1) port=$(field "$s" 2)
    echo "== $name"
    TMPDIR="$dir/tmp" TL_SSH_TEST_HOST=tester@127.0.0.1 TL_SSH_TEST_PORT=$port \
      TL_SSH_TEST_KEY="$dir/key" TL_SSH_TEST_EXTRA="$extra" TL_SSH_TEST_SUDO_PASSWORD="$password" \
      TL_SSH_TEST_DIALECT=$(field "$s" 3) cargo test -p tl-ssh --test fake_ssh "$@" \
      || failed="$failed $name"
  done
  if [ -n "$failed" ]; then echo "failed on:$failed" >&2; exit 1; fi
}

down() {
  for s in $servers; do
    name=$(field "$s" 1)
    docker rm -f "tl-ssh-$name" >/dev/null 2>&1 || true
    docker rmi "tl-ssh-test:$name" >/dev/null 2>&1 || true
  done
}

case ${1:-} in
  up) up ;;
  test) shift; run_tests "$@" ;;
  down) down ;;
  *) sed -n '3,5p' "$0" >&2; exit 2 ;;
esac
