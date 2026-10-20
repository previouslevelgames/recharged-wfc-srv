#!/usr/bin/env python3
"""Exercise the built image using a temporary Docker volume and containers."""

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ADMIN = (
    "-e", "PUBLIC_URL=https://wfc.example.com",
    "-e", "GOOGLE_CLIENT_ID=container-test.apps.googleusercontent.com",
    "-e", "GOOGLE_CLIENT_SECRET=container-test-only",
    "-e", "ADMIN_GOOGLE_EMAILS=admin@example.com",
)


def docker(*args):
    return subprocess.check_output(
        ["docker", *args], text=True,
        stderr=subprocess.STDOUT if args[0] == "logs" else None,
    ).strip()


def inspect(container):
    return json.loads(docker("inspect", container))[0]


def wait_healthy(container):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        state = inspect(container)["State"]
        if not state["Running"]:
            raise AssertionError("Container exited before it was ready")
        if state.get("Health", {}).get("Status") == "healthy":
            return
        time.sleep(0.5)
    raise AssertionError("Container did not become healthy within 60 seconds")


def wait_exit(container):
    result = subprocess.run(
        ["docker", "wait", container],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return int(result.stdout)


def sql(container, query):
    return docker(
        "exec", container, "sh", "-c",
        'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -h 127.0.0.1 '
        '-U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -v ON_ERROR_STOP=1 -c "$1"',
        "sh", query,
    )


def http_status(url):
    try:
        with urllib.request.urlopen(url, timeout=5) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, ""


def main():
    image, revision = sys.argv[1:]
    volume = "recharged-wfc-test-" + uuid.uuid4().hex
    containers = []
    docker("volume", "create", volume)

    def start(*extra):
        container = docker(
            "run", "-d", "-p", "127.0.0.1::8080",
            "--health-interval=1s", "--health-start-period=0s",
            "--mount", f"type=volume,source={volume},target=/var/lib/postgresql",
            "-e", "POSTGRES_DB=integration_db", "-e", "POSTGRES_USER=integration_user",
            "-e", "POSTGRES_PASSWORD=container-test-only",
            "-e", "GIT_COMMIT=runtime-override", *extra, image,
        )
        containers.append(container)
        return container

    try:
        container = start()
        wait_healthy(container)
        address = docker("port", container, "8080/tcp")
        with urllib.request.urlopen(f"http://{address}/status", timeout=5) as response:
            assert json.load(response) == {"status": "ok", "revision": revision}
        assert http_status(f"http://{address}/admin")[0] == 404
        assert sql(container, "SHOW listen_addresses") == "127.0.0.1"
        sql(container, "CREATE TABLE persistence_probe (value text)")
        sql(container, "INSERT INTO persistence_probe VALUES ('persisted')")
        docker("stop", "--timeout", "30", container)
        assert inspect(container)["State"]["ExitCode"] == 0
        logs = docker("logs", container)
        assert logs.rfind("database system is shut down") > logs.rfind(
            "recharged-wfc-srv listening"
        ) >= 0
        docker("rm", "-v", container)
        containers.remove(container)
        print("API revision, database credentials, and clean shutdown verified.", flush=True)

        container = start(*ADMIN)
        wait_healthy(container)
        assert sql(container, "SELECT value FROM persistence_probe") == "persisted"
        print("Database data survived container replacement.", flush=True)
        address = docker("port", container, "8080/tcp")
        status, page = http_status(f"http://{address}/admin")
        assert status == 200 and "Sign in with Google" in page
        print("The admin dashboard asks for a sign-in.", flush=True)
        # The supervisor may stop the container before docker exec returns.
        stopped = subprocess.run(
            ["docker", "exec", "--user", "postgres", container,
             "pg_ctl", "-D", "/var/lib/postgresql/18/docker", "-m", "fast", "-W", "stop"],
            check=False, stdout=subprocess.DEVNULL,
        )
        assert stopped.returncode in (0, 137, 143)
        assert wait_exit(container) != 0
        print("Unexpected PostgreSQL exit stops the container.", flush=True)

        container = start("-e", "POSTGRES_PASSWORD=incorrect-password")
        assert wait_exit(container) != 0
        print("Changed credentials cannot silently replace the persisted password.", flush=True)

        container = start("-e", "PORT=invalid")
        assert wait_exit(container) != 0
        assert "database system is shut down" in docker("logs", container)
        print("API startup failure shuts down PostgreSQL cleanly.", flush=True)

        container = start(*ADMIN[:2])
        assert wait_exit(container) != 0
        assert "GOOGLE_CLIENT_ID" in docker("logs", container)
        print("A partial admin configuration is rejected.", flush=True)

        container = start("-e", "POSTGRES_PASSWORD=")
        assert wait_exit(container) != 0
        assert "Set POSTGRES_PASSWORD" in docker("logs", container)
        print("A missing database password is rejected.", flush=True)
    except Exception:
        for container in containers:
            subprocess.run(["docker", "logs", container], check=False)
        raise
    finally:
        for container in containers:
            subprocess.run(["docker", "rm", "-fv", container], check=False,
                           stdout=subprocess.DEVNULL)
        docker("volume", "rm", volume)


if __name__ == "__main__":
    main()
