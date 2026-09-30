#!/usr/bin/env python3
"""Signal when the reviewed upstream Firecrawl pin has a newer stable tag."""

from __future__ import annotations

import argparse
import json
import os
import re
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PIN_FILE = ROOT / "upstream" / "firecrawl.json"
ISSUE_TITLE = "Dependency watch: upstream Firecrawl release"
TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")


def token() -> str:
    return os.environ.get("GITHUB_TOKEN", "").strip()


def github_json(path: str, *, method: str = "GET", body: dict | None = None):
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "firecrawl-local-dependency-watch",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token():
        headers["Authorization"] = f"Bearer {token()}"
    req = urllib.request.Request(
        f"https://api.github.com{path}", data=data, method=method, headers=headers
    )
    with urllib.request.urlopen(req, timeout=20) as response:
        return json.load(response)


def semver(tag: str) -> tuple[int, int, int]:
    match = TAG_RE.fullmatch(tag)
    if not match:
        raise ValueError(f"not a stable Firecrawl tag: {tag}")
    return tuple(int(part) for part in match.groups())


def latest_tag(repository: str) -> tuple[str, str]:
    tags = github_json(f"/repos/{repository}/tags?per_page=100")
    candidates = []
    for item in tags:
        tag = str(item.get("name") or "")
        if TAG_RE.fullmatch(tag):
            candidates.append((semver(tag), tag))
    if not candidates:
        raise RuntimeError("No stable Firecrawl semver tags found")
    _, tag = max(candidates)
    return tag, f"https://github.com/{repository}/releases/tag/{tag}"


def find_issue(repository: str) -> dict | None:
    issues = github_json(f"/repos/{repository}/issues?state=all&per_page=100")
    return next(
        (
            issue
            for issue in issues
            if "pull_request" not in issue and issue.get("title") == ISSUE_TITLE
        ),
        None,
    )


def sync_issue(repository: str, *, current: str, latest: str, url: str) -> None:
    issue = find_issue(repository)
    outdated = semver(latest) > semver(current)
    body = (
        "A newer stable upstream Firecrawl tag is available.\n\n"
        f"- reviewed wrapper pin: `{current}`\n"
        f"- latest stable upstream tag: `{latest}`\n"
        f"- upstream: {url}\n\n"
        "Signal only: promotion requires the wrapper's candidate scan, security "
        "gate and regression tests. This watcher never deploys or merges."
    )
    if outdated:
        if issue is None:
            github_json(
                f"/repos/{repository}/issues",
                method="POST",
                body={"title": ISSUE_TITLE, "body": body},
            )
        else:
            github_json(
                f"/repos/{repository}/issues/{issue['number']}",
                method="PATCH",
                body={"body": body, "state": "open"},
            )
    elif issue is not None and issue.get("state") == "open":
        github_json(
            f"/repos/{repository}/issues/{issue['number']}",
            method="PATCH",
            body={"body": body + "\n\nStatus: reviewed pin is current.", "state": "closed"},
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sync-issue", action="store_true")
    args = parser.parse_args()

    pin = json.loads(PIN_FILE.read_text(encoding="utf-8"))
    repository = str(pin.get("repository") or "")
    current = str(pin.get("tag") or "")
    commit = str(pin.get("commit") or "")
    if repository != "firecrawl/firecrawl":
        raise RuntimeError("Unexpected upstream repository in pin file")
    semver(current)
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise RuntimeError("Invalid upstream commit pin")

    latest, url = latest_tag(repository)
    result = {
        "repository": repository,
        "current": current,
        "commit": commit,
        "latest": latest,
        "outdated": semver(latest) > semver(current),
        "latest_url": url,
    }
    print(json.dumps(result, indent=2, sort_keys=True))

    if args.sync_issue:
        wrapper_repo = os.environ.get("GITHUB_REPOSITORY", "").strip()
        if not wrapper_repo or not token():
            raise RuntimeError("GITHUB_REPOSITORY and GITHUB_TOKEN are required")
        sync_issue(wrapper_repo, current=current, latest=latest, url=url)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
