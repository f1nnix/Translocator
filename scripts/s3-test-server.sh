#!/bin/sh
# Starts MinIO in Docker for the tl-s3 end-to-end tests and prints the
# environment they need. Without that environment the tests do nothing, so
# a plain `cargo test` still works.
#
#   eval "$(scripts/s3-test-server.sh)"
#   cargo test -p tl-s3
#   scripts/s3-test-server.sh --stop
#
# The credentials below only ever reach this throwaway container.
set -eu

name=tl-minio
port=${TL_S3_TEST_PORT:-9000}
user=translocator
password=translocator-secret

if [ "${1:-}" = "--stop" ]; then
    docker rm -f "$name" >/dev/null 2>&1 || true
    echo "stopped $name" >&2
    exit 0
fi

if ! docker inspect "$name" >/dev/null 2>&1; then
    docker run -d --name "$name" -p "127.0.0.1:$port:9000" \
        -e MINIO_ROOT_USER="$user" -e MINIO_ROOT_PASSWORD="$password" \
        minio/minio:latest server /data >/dev/null
elif [ "$(docker inspect -f '{{.State.Running}}' "$name")" != "true" ]; then
    docker start "$name" >/dev/null
fi

# Wait for it to answer before handing the caller an endpoint.
i=0
until curl -fsS -o /dev/null "http://127.0.0.1:$port/minio/health/live" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -gt 60 ]; then
        echo "MinIO did not become healthy; try: docker logs $name" >&2
        exit 1
    fi
    sleep 0.5
done

echo "export TL_S3_TEST_ENDPOINT=http://127.0.0.1:$port"
echo "export TL_S3_TEST_KEY=$user"
echo "export TL_S3_TEST_SECRET=$password"
# Small enough that a 12 MB file really goes up in several parts; S3 wants
# at least 5 MiB per part.
echo "export TL_S3_MULTIPART_THRESHOLD=6291456"
echo "export TL_S3_PART_SIZE=5242880"
