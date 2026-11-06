#!/bin/sh
# Starts a WebDAV server in Docker for the tl-webdav end-to-end tests and
# prints the environment they need. Without that environment the tests do
# nothing, so a plain `cargo test` still works.
#
#   eval "$(scripts/webdav-test-server.sh)"
#   cargo test -p tl-webdav
#   scripts/webdav-test-server.sh --stop
#
# rclone's server is the one used here because it answers Depth: infinity,
# COPY and MOVE, which is what the plugin leans on. The credentials below
# only ever reach this throwaway container.
set -eu

name=tl-webdav
volume=tl-webdav-data
port=${TL_WEBDAV_TEST_PORT:-8080}
user=tluser
password=tlpass

if [ "${1:-}" = "--stop" ]; then
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker volume rm "$volume" >/dev/null 2>&1 || true
    echo "stopped $name" >&2
    exit 0
fi

if ! docker inspect "$name" >/dev/null 2>&1; then
    docker run -d --name "$name" -p "127.0.0.1:$port:8080" -v "$volume:/data" \
        rclone/rclone:latest serve webdav /data \
        --addr :8080 --user "$user" --pass "$password" >/dev/null
elif [ "$(docker inspect -f '{{.State.Running}}' "$name")" != "true" ]; then
    docker start "$name" >/dev/null
fi

# Wait for it to answer before handing the caller a URL.
i=0
until curl -fsS -o /dev/null -u "$user:$password" "http://127.0.0.1:$port/" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -gt 60 ]; then
        echo "the WebDAV server did not come up; try: docker logs $name" >&2
        exit 1
    fi
    sleep 0.5
done

echo "export TL_WEBDAV_TEST_URL=http://127.0.0.1:$port"
echo "export TL_WEBDAV_TEST_USER=$user"
echo "export TL_WEBDAV_TEST_PASSWORD=$password"
