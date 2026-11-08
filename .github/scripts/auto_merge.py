#!/usr/bin/env python3
"""Merge a low-risk pull request by itself, or label it "to test", "to acknowledge" or "to review".

Run by .github/workflows/auto-merge.yml on every non-draft pull request opened or
updated, and again when the pull request's "Verify" workflow completes. It reads the
pull request's diff and description and:

- when the description fails the check of pr_description.py (same folder), does
  nothing: the "PR description" check is red, and editing the description runs the
  decision again; a structural change (structure.py) without an "Architecture changes"
  section fails it;
- when the pull request was opened by the repository owner from a branch of the
  repository itself, every changed file passes and the description asks nothing of
  Julien, rebase-merges the pull request and deletes its branch;
- otherwise adds the labels and lists the reasons in the job summary: "to review"
  for the files below, an "Architecture changes" section and the structural changes it
  declares, "to test" and
  "to acknowledge" from the description (see pr_description.py). A pull request
  opened by anyone else, or from a fork, gets "to review" whatever its files: on a
  public repository anyone can open one, and the job holds a write token. The informational
  label "no ticket" never holds the merge.

When the repository has a .github/workflows/verify.yml, a pull request whose files all
pass is merged only once the latest Verify run of its head commit has succeeded: while
the run is missing or in progress the script waits for its completion event, and after
a failure it leaves the pull request open (the next push runs Verify again).

A file passes unless:

- it is deleted (or changes type);
- it is under .github/ or .githooks/, or it is an AGENTS.md or CLAUDE.md;
- it is agent instructions or configuration: a SKILL.md, a hooks/hooks.json, a
  .claude/settings*.json, or a file under a .claude-plugin/ folder;
- it is a test file or a Markdown file and a line was removed or changed, except a
  whole comment line replaced by whole comment lines (`#`, `//`, `///`, lines of
  `/* */`; `<!-- -->` in Markdown);
- it is an existing test file and an added line switches a test off (SKIP_MARKER:
  unittest, pytest, NUnit, xUnit, JUnit, Rust and JavaScript skips, or a bare
  `return`, which the diff cannot tell from any other). Other added lines are allowed.

A pull request that already carries one of the three labels is never merged.

Test files: a path under a `tests/` folder, or a file named `test_*`, `*_test.*`,
`*Test.cs`, `*Tests.cs` or `*.spec.*`. More patterns go in .github/auto-merge.json:
{"test_patterns": ["e2e/", "*.feature", "src/fixtures/*.json"]} — a pattern ending in
`/` is a folder name anywhere in the path, a pattern with a `/` is matched against the
whole path, any other against the file name.

The workflow runs on pull_request_target: this script and its configuration come from
the base branch, so a pull request cannot change the check that judges it, and the
pull request's code is never run.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from fnmatch import fnmatchcase
from pathlib import Path

import pr_description
import structure
from pr_description import HOLD_LABELS, LABELS, TO_REVIEW
from structure import CONFIG, git, load_config, matches, test_patterns

VERIFY_WORKFLOW = ".github/workflows/verify.yml"
ALWAYS_REVIEW_FOLDERS = (".github/", ".githooks/")
ALWAYS_REVIEW_NAMES = {"AGENTS.md", "CLAUDE.md"}
AGENT_CONFIG = ("SKILL.md", "*/SKILL.md", "hooks/hooks.json", "*/hooks/hooks.json",
                ".claude/settings*.json", "*/.claude/settings*.json", ".claude-plugin/*", "*/.claude-plugin/*")
SKIP_MARKER = re.compile(r"""
    \bunittest\.skip | \bSkipTest\b | \.skipTest\( | \bpytest\.(mark\.)?skip   # Python
  | \[\s*Ignore\b | \bSkip\s*=                                              # NUnit, xUnit
  | @Disabled\b | @Ignore\b                                                 # JUnit
  | \#\[ignore\b                                                             # Rust
  | \b(it|test|describe|context)\.skip\b | \bx(it|test|describe|context)\s*\(  # JavaScript
  | ^\s*return\s*;?\s*$                                                     # a test cut short
""", re.VERBOSE)
CODE_COMMENT = ("#", "//", "/*", "*")


@dataclass
class Hunk:
    removed: list[str] = field(default_factory=list)
    added: list[str] = field(default_factory=list)


@dataclass
class FileChange:
    path: str
    status: str  # A, M, D or T, as given by git diff --name-status --no-renames
    hunks: list[Hunk] = field(default_factory=list)
    binary: bool = False


def is_test_file(path: str, patterns: list[str]) -> bool:
    return any(matches(path, p) for p in patterns)


def is_comment(line: str, markdown: bool) -> bool:
    text = line.strip()
    if markdown:
        return text.startswith("<!--") and text.endswith("-->")
    return text.startswith(CODE_COMMENT)


def parse_hunks(diff: str) -> tuple[list[Hunk], bool]:
    """Hunks of a `git diff --unified=0` of one file, and whether the file is binary."""
    hunks: list[Hunk] = []
    binary = False
    in_hunk = False
    for line in diff.splitlines():
        if line.startswith("@@"):
            hunks.append(Hunk())
            in_hunk = True
        elif not in_hunk:
            binary = binary or line.startswith(("Binary files ", "GIT binary patch"))
        elif line.startswith("-"):
            hunks[-1].removed.append(line[1:])
        elif line.startswith("+"):
            hunks[-1].added.append(line[1:])
    return hunks, binary


def file_reasons(change: FileChange, patterns: list[str]) -> list[str]:
    path = change.path
    name = path.rsplit("/", 1)[-1]
    if change.status == "D":
        return [f"{path}: deleted"]
    if change.status not in ("A", "M"):
        return [f"{path}: changed type"]
    if path.startswith(ALWAYS_REVIEW_FOLDERS) or path == CONFIG:
        return [f"{path}: CI, hooks or auto-merge configuration"]
    if name in ALWAYS_REVIEW_NAMES:
        return [f"{path}: agent instructions"]
    if any(fnmatchcase(path, pattern) for pattern in AGENT_CONFIG):
        return [f"{path}: agent instructions or configuration"]
    markdown = name.lower().endswith(".md")
    if not markdown and not is_test_file(path, patterns):
        return []
    kind = "Markdown file" if markdown else "test file"
    if change.status == "A":
        return []
    if change.binary:
        return [f"{path}: binary {kind} changed"]
    for hunk in change.hunks:
        if not hunk.removed:
            continue
        old_comments = all(is_comment(line, markdown) for line in hunk.removed)
        new_comments = sum(is_comment(line, markdown) for line in hunk.added)
        if not old_comments or new_comments < len(hunk.removed):
            return [f"{path}: existing lines of a {kind} removed or changed"]
    if not markdown:
        skips = [line.strip() for hunk in change.hunks for line in hunk.added if SKIP_MARKER.search(line)]
        if skips:
            return [f"{path}: test skip marker added: {skips[0]}"]
    return []


def review_reasons(files: list[FileChange], config: dict) -> list[str]:
    """Why the changed files need a careful review; empty when they can be merged by themselves."""
    patterns = test_patterns(config)
    reasons = []
    for change in files:
        reasons += file_reasons(change, patterns)
    if not files:
        reasons.append("no changed file")
    return reasons


def origin_reasons(pr: dict, repo: str) -> list[str]:
    """Why the pull request's origin needs a review: an author other than the owner, or a fork."""
    owner = repo.split("/", 1)[0]
    author = (pr.get("user") or {}).get("login") or "unknown"
    head_repo = (pr["head"].get("repo") or {}).get("full_name")
    reasons = []
    if author.lower() != owner.lower():
        reasons.append(f"opened by {author}, not by the repository owner {owner}")
    if head_repo is None or head_repo.lower() != repo.lower():
        reasons.append(f"head branch in {head_repo or 'a deleted repository'}, not in {repo}")
    return reasons


def decide(pr: dict, repo: str, files: list[FileChange], description: pr_description.Description,
           config: dict) -> dict[str, list[str]]:
    """Labels to add, with their reasons; none of HOLD_LABELS means the pull request can be merged."""
    labels = {name: list(reasons) for name, reasons in description.labels.items()}
    reasons = origin_reasons(pr, repo) + review_reasons(files, config)
    if reasons:
        labels.setdefault(TO_REVIEW, []).extend(reasons)
    return labels


def changed_files(base: str, head: str, cwd: Path | None = None) -> list[FileChange]:
    """Files changed by head since its merge base with base."""
    files = []
    for status, path in structure.name_status(base, head, cwd):
        diff = git("diff", "--unified=0", "--no-renames", "--no-color", "--no-ext-diff", "--no-textconv",
                   f"{base}...{head}", "--", path, cwd=cwd)
        hunks, binary = parse_hunks(diff)
        files.append(FileChange(path, status, hunks, binary))
    return files


def verify_state(runs: list[dict]) -> str:
    """State of the Verify runs of the head commit: "waiting", "success" or "failure"."""
    if not runs:
        return "waiting"
    latest = max(runs, key=lambda run: run["created_at"])
    if latest["status"] != "completed":
        return "waiting"
    return "success" if latest["conclusion"] == "success" else "failure"


def gh(*args: str) -> str | None:
    """Output of `gh api`, or None when it fails."""
    result = subprocess.run(["gh", "api", *args], capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        print(f"gh api {' '.join(args)}: {result.stderr.strip()}", file=sys.stderr)
        return None
    return result.stdout


def gh_api(*args: str) -> bool:
    return gh(*args) is not None


def gh_json(*args: str):
    output = gh(*args)
    return None if output is None else json.loads(output)


def event_pull_request(event: dict, repo: str) -> dict | None:
    """The pull request to judge: the event's own, or the one a completed Verify run belongs to."""
    if "pull_request" in event:
        return event["pull_request"]
    run = event["workflow_run"]
    for ref in run.get("pull_requests") or []:
        if ref["head"]["sha"] == run["head_sha"]:
            pr = gh_json(f"repos/{repo}/pulls/{ref['number']}")
            if pr and pr["state"] == "open" and pr["head"]["sha"] == run["head_sha"]:
                return pr
    return None


def summary(text: str) -> None:
    print(text)
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write(text + "\n")


def add_labels(repo: str, number: int, labels: dict[str, list[str]]) -> None:
    """Add the labels (created when missing) and list their reasons in the job summary."""
    if not labels:
        return
    for name in labels:
        color, description = LABELS[name]
        gh_api("-X", "POST", f"repos/{repo}/labels", "-f", f"name={name}",
               "-f", f"color={color}", "-f", f"description={description}")
    labelled = gh_api("-X", "POST", f"repos/{repo}/issues/{number}/labels",
                      *[arg for name in labels for arg in ("-f", f"labels[]={name}")])
    summary("".join(f"## {name.capitalize()}\n\n" + "".join(f"- {reason}\n" for reason in reasons) + "\n"
                    for name, reasons in labels.items()))
    if not labelled:
        sys.exit(f"could not add the labels {', '.join(labels)}")


def main() -> None:
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
    repo = os.environ["GITHUB_REPOSITORY"]
    pr = event_pull_request(event, repo)
    if pr is None:
        print("No open pull request at this commit: nothing to do.")
        return
    if pr.get("draft"):
        print("Draft pull request: nothing to do.")
        return
    number = pr["number"]
    base, head = pr["base"]["sha"], pr["head"]["sha"]
    git("fetch", "--no-tags", "--quiet", "origin", f"+refs/pull/{number}/head")
    config = load_config(Path(CONFIG))
    description = pr_description.check(pr.get("body") or "", structure.diff_signals(base, head, config))
    if description.errors:
        summary("## Not merged\n\nThe description fails the PR description check; editing it runs "
                "the decision again.\n\n" + "".join(f"- {error}\n" for error in description.errors))
        return
    labels = decide(pr, repo, changed_files(base, head), description, config)
    add_labels(repo, number, labels)
    if any(name in labels for name in HOLD_LABELS):
        return
    held = [name for name in HOLD_LABELS if name in {label["name"] for label in pr.get("labels", [])}]
    if held:
        summary(f"## Not merged\n\nThe pull request carries the label {held[0]}.")
        return
    if Path(VERIFY_WORKFLOW).is_file():
        runs = gh_json(f"repos/{repo}/actions/workflows/verify.yml/runs?head_sha={head}&event=pull_request")
        if runs is None:
            sys.exit("could not read the Verify runs")
        state = verify_state(runs.get("workflow_runs", []))
        if state == "waiting":
            summary("## Waiting for Verify\n\nEvery changed file passes the low-risk rules; "
                    "the pull request is merged once Verify passes.")
            return
        if state == "failure":
            summary("## Not merged\n\nVerify failed on the head commit; a new push runs it again.")
            return
    if not gh_api("-X", "PUT", f"repos/{repo}/pulls/{number}/merge", "-f", "merge_method=rebase", "-f", f"sha={head}"):
        add_labels(repo, number, {TO_REVIEW: ["rebase merge refused (conflict, or the branch moved)"]})
        return
    summary("## Merged\n\nEvery changed file passes the low-risk rules.")
    if pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == repo:
        gh_api("-X", "DELETE", f"repos/{repo}/git/refs/heads/{pr['head']['ref']}")


if __name__ == "__main__":
    main()
