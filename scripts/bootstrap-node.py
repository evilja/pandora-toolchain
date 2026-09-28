#!/usr/bin/env python3
"""Install a coordinator's Pandora Mini binaries without a Rust toolchain.

Run from the repository root after configuring DB/config/global/environment/env.pandora.
The checkout is retained for migrations; this script never compiles it.
"""

import hashlib
import json
import os
import platform
import subprocess
import sys
import urllib.request
import urllib.error
import urllib.parse
from pathlib import Path

NAMES = {"pndc", "pnmpeg", "pnp2p", "pncurl", "pnass"}
ROOT = Path("DB/bin/pandora")
LIMIT = 512 * 1024 * 1024


def fail(message):
    raise SystemExit(f"bootstrap-node: {message}")


def settings():
    path = Path("DB/config/global/environment/env.pandora")
    if not path.is_file():
        fail(f"configure {path} first")
    values = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "|pntools|" in line:
            key, value = line.split("|pntools|", 1)
            values[key.strip()] = value.strip()
    url = values.get("link_coordinator_url", "").rstrip("/")
    token = values.get("link_node_token", "")
    if not url or not token:
        fail("link_coordinator_url and link_node_token are required")
    if not url.startswith("https://"):
        fail("a HTTPS coordinator URL is required for binary bootstrap")
    return url, token


def request(url, token):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, request, file, code, message, headers, target):
            return None

    headers = {
        "Authorization": f"Bearer {token}",
        "User-Agent": "Pandora-Mini/4.0",
        "Accept": "application/json, application/octet-stream",
    }
    try:
        return urllib.request.build_opener(NoRedirect).open(
            urllib.request.Request(url, headers=headers), timeout=300,
        )
    except urllib.error.HTTPError as error:
        body = error.read(512).decode("utf-8", "replace").strip()
        content_type = error.headers.get("Content-Type", "")
        if error.code == 403 and "requires a link token" in body:
            fail("HTTP 403 from Pandora: link_node_token is valid but is not a node link token")
        if error.code == 401:
            fail("HTTP 401 from Pandora: link_node_token is missing, revoked, or invalid")
        if error.code == 403 and (error.headers.get("cf-mitigated") or "html" in content_type.lower()):
            fail("HTTP 403 from Cloudflare or a proxy; allow the coordinator's /api/v1/link/release and /api/v1/link/binaries routes for this node")
        detail = " ".join(body.split())[:200] if "html" not in content_type.lower() else "HTML response"
        fail(f"HTTP {error.code} fetching {urllib.parse.urlsplit(url).path}: {detail or 'no response detail'}")


def compatible(bundle):
    target = f"{sys.platform}-{platform.machine()}"
    if bundle["target"] != target:
        fail(f"package is for {bundle['target']}; this node is {target}")
    local = platform.libc_ver()
    if local[0] != "glibc":
        fail("this node does not report glibc")
    def version(value):
        parts = value.split(".")
        return tuple(int(part) for part in parts[:2])
    if version(local[1]) < version(bundle["glibc"]):
        fail(f"glibc {local[1]} is older than package minimum {bundle['glibc']}")
    features = set()
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("flags") and ":" in line:
                features.update(line.split(":", 1)[1].split())
                break
    aliases = {"sse4.1": "sse4_1", "sse4.2": "sse4_2", "bmi1": "bmi1"}
    missing = [item for item in bundle["cpu_features"] if aliases.get(item, item) not in features]
    if missing:
        fail(f"CPU lacks package features: {', '.join(missing)}")
    files = bundle["files"]
    if len(files) != 5 or {item["name"] for item in files} != NAMES:
        fail("package does not contain the five required executables")


def main():
    if sys.platform != "linux":
        fail("binary bootstrap currently supports Linux nodes")
    url, token = settings()
    with request(f"{url}/api/v1/link/release", token) as response:
        release = json.load(response)
    bundle = release.get("binaries")
    if not bundle:
        fail("coordinator has no binary package for its running release")
    compatible(bundle)
    commit = release["commit"]
    if not (len(commit) == 40 and all(char in "0123456789abcdefABCDEF" for char in commit)):
        fail("coordinator advertised an invalid commit")
    if bundle["commit"] != commit:
        fail("package commit differs from release")
    checkout = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    if checkout != commit:
        fail(f"checkout is at {checkout[:12]}, coordinator is at {commit[:12]}; update the checkout first")
    tag = f"{release['build']}-{commit}"
    releases = ROOT / "releases"
    releases.mkdir(parents=True, exist_ok=True)
    stage = releases / f".bootstrap-{tag}-{os.getpid()}"
    stage.mkdir()
    try:
        for entry in bundle["files"]:
            name, size, digest = entry["name"], entry["bytes"], entry["sha256"]
            if name not in NAMES or not 0 < size <= LIMIT or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
                fail("invalid package file descriptor")
            path = stage / name
            hasher = hashlib.sha256()
            received = 0
            with request(f"{url}/api/v1/link/binaries/{digest}/{name}", token) as response, path.open("xb") as output:
                while chunk := response.read(65536):
                    received += len(chunk)
                    if received > size:
                        fail(f"{name} exceeds its declared size")
                    hasher.update(chunk)
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
            if received != size or hasher.hexdigest() != digest:
                fail(f"{name} failed SHA-256 or size verification")
            path.chmod(0o755)
            result = subprocess.run([str(path), "--link-binary-info"], capture_output=True, text=True, check=True)
            info = json.loads(result.stdout)
            if info.get("commit") != commit or info.get("encoder_identity") != bundle["encoder_identity"]:
                fail(f"{name} has a different build or x264 identity")
        destination = releases / tag
        if destination.exists():
            fail(f"{destination} already exists; inspect it before retrying")
        stage.rename(destination)
        current = ROOT / "current"
        previous = ROOT / "previous"
        if current.is_symlink():
            next_previous = ROOT / "previous.next"
            next_previous.symlink_to(os.readlink(current))
            next_previous.replace(previous)
        pending = ROOT / "pending.json"
        pending.write_text(json.dumps({"build": release["build"], "commit": commit, "encoder_identity": bundle["encoder_identity"]}))
        next_current = ROOT / "current.next"
        next_current.symlink_to(Path("releases") / tag)
        next_current.replace(current)
        print(f"Installed build {release['build']} ({commit[:12]}). Start it with ./start-node.sh")
    finally:
        if stage.exists():
            import shutil
            shutil.rmtree(stage)


if __name__ == "__main__":
    main()
