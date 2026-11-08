#!/usr/bin/env python3
"""Check a pull request description against the PR template, and sort the pull request.

    python3 pr_description.py              # in CI: the pull request of GITHUB_EVENT_PATH
    python3 pr_description.py body.md      # locally, before opening the pull request
    python3 pr_description.py body.md --base origin/main   # also checks the diff of HEAD

Run by .github/workflows/pr-description.yml on every non-draft pull request opened,
edited or updated; exits 1, with the reasons in the job summary, when the description
fails. .github/scripts/auto_merge.py imports it: a failing description is never merged,
and the sections present decide the labels (rules: dev-kit's docs/pr-description.md).

A description passes when, once its <!-- comments --> are removed:

- it has a ticket reference (`Fixes #12`, `Closes ABC-12`, `Refs owner/repo#12`, an
  issue URL…) or a `No ticket: <reason>` line (at least two words; label "no ticket");
- it has the sections `## What was done` and `## Test results`, not empty; only Test
  results may say `N/A: <reason>` (at least two words);
- every optional section present (`## Manual checks`, `## Values to validate`,
  `## Assumptions`, `## Known limitations`, `## Architecture changes`) has content:
  an optional section that does not apply is deleted, heading included;
- no placeholder is left outside code: a line made only of TODO, TBD, ... or …, or
  <n>, <ID>, <…>, <reason>;
- a diff with structural changes (structure.py, same folder) has an
  `## Architecture changes` section.

Labels: "to test" for Manual checks or a [manual] / [manuel] marker; "to acknowledge"
for Values to validate, Assumptions or Known limitations; "to review" for Architecture
changes, with each structural change as a reason (markers and placeholders inside
`code` are mentions and do not count) (auto_merge.py adds it for the risky files too).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

import structure

TO_TEST, TO_ACKNOWLEDGE, TO_REVIEW, NO_TICKET = "to test", "to acknowledge", "to review", "no ticket"
# name: (color, description), created when missing. The first three hold the merge.
LABELS = {
    TO_TEST: ("d93f0b", "Needs manual checks before merging"),
    TO_ACKNOWLEDGE: ("c5def5", "Values, assumptions or limitations to confirm before merging"),
    TO_REVIEW: ("fbca04", "Needs a careful human review before merging"),
    NO_TICKET: ("ededed", "Pull request without a ticket"),
}
HOLD_LABELS = (TO_TEST, TO_ACKNOWLEDGE, TO_REVIEW)

REQUIRED = ("What was done", "Test results")
ARCHITECTURE = "Architecture changes"
OPTIONAL = {"Manual checks": TO_TEST, "Values to validate": TO_ACKNOWLEDGE, "Assumptions": TO_ACKNOWLEDGE,
            "Known limitations": TO_ACKNOWLEDGE, ARCHITECTURE: TO_REVIEW}
NA_ALLOWED = {"Test results"}

COMMENT = re.compile(r"<!--.*?(?:-->|\Z)", re.DOTALL)
HEADING = re.compile(r"^##[ \t]+(.+?)[ \t#:]*$")
FENCE = re.compile(r"^[ \t]*(```|~~~)")
TICKET = re.compile(r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs?)\b:?[ \t]+"
                    r"(?:[\w.-]+/[\w.-]+#\d+|#\d+|(?-i:[A-Z][A-Z0-9]*-\d+)\b|https://\S+/issues/\d+|https://linear\.app/\S+)")
NO_TICKET_LINE = re.compile(r"(?im)^[ \t]*no ticket[ \t]*[:—–-][ \t]*(.*)$")
NOT_APPLICABLE = re.compile(r"(?i)^(?:n/?a|none)[ \t]*(?:\Z|[:.—–-][ \t]*(.*))", re.DOTALL)
PLACEHOLDER_LINE = re.compile(r"(?im)^[ \t]*(?:[-*+][ \t]+(?:\[[ xX]\][ \t]+)?)?(?:todo|tbd|\.\.\.|…)[ \t]*$")
PLACEHOLDER = re.compile(r"(?i)<(?:n|id|…|\.\.\.|reason)>")
CODE_BLOCK = re.compile(r"(?ms)^[ \t]*(```|~~~).*?(?:^[ \t]*\1|\Z)")
CODE_SPAN = re.compile(r"`[^`\n]*`")
MANUAL = re.compile(r"\[(manual|manuel)\]", re.IGNORECASE)


@dataclass
class Description:
    errors: list[str] = field(default_factory=list)
    labels: dict[str, list[str]] = field(default_factory=dict)  # label: reasons

    def label(self, name: str, reason: str) -> None:
        self.labels.setdefault(name, []).append(reason)


def words(text: str) -> int:
    return len(text.split())


def sections(text: str) -> dict[str, str]:
    """Level-2 sections by heading (case as in OPTIONAL / REQUIRED when it matches), outside code fences."""
    known = {name.casefold(): name for name in (*REQUIRED, *OPTIONAL)}
    found: dict[str, list[str]] = {}
    current = None
    fenced = False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
        heading = None if fenced else HEADING.match(line)
        if heading:
            title = heading.group(1).strip()
            current = known.get(title.casefold(), title)
            found.setdefault(current, [])
        elif current is not None:
            found[current].append(line)
    return {name: "\n".join(lines).strip() for name, lines in found.items()}


def check(body: str, structural: list[structure.Signal] = ()) -> Description:
    """Errors and labels of the description; structural: the structural changes of the diff."""
    result = Description()
    declared = False
    text = COMMENT.sub("", body.replace("\r\n", "\n"))
    no_ticket = NO_TICKET_LINE.search(text)
    if no_ticket:
        if words(no_ticket.group(1)) < 2:
            result.errors.append("`No ticket:` needs a reason of at least two words")
        else:
            result.label(NO_TICKET, "no ticket: " + no_ticket.group(1).strip())
    elif not TICKET.search(text):
        result.errors.append("no ticket reference (`Fixes #12`, `Fixes ABC-12`, `Refs #12`) "
                             "nor `No ticket: <reason>`")
    found = sections(text)
    for name in REQUIRED:
        if name not in found:
            result.errors.append(f"missing section `## {name}`")
    for name, content in found.items():
        if name not in REQUIRED and name not in OPTIONAL:
            continue
        if not content:
            hint = "" if name in REQUIRED else ": fill it, or delete it with its heading"
            result.errors.append(f"section `## {name}` is empty{hint}")
            continue
        not_applicable = NOT_APPLICABLE.match(content)
        if not_applicable:
            if name not in NA_ALLOWED:
                hint = "" if name in REQUIRED else ": delete the section, heading included"
                result.errors.append(f"section `## {name}` cannot be N/A{hint}")
            elif words(not_applicable.group(1) or "") < 2:
                result.errors.append(f"section `## {name}`: N/A needs a reason of at least two words")
            continue
        if name in OPTIONAL:
            result.label(OPTIONAL[name], f"description: section {name}")
            declared = declared or name == ARCHITECTURE
    if structural and not declared:
        result.errors.append(f"structural changes without a `## {ARCHITECTURE}` section: "
                             + "; ".join(str(signal) for signal in structural))
    for signal in structural if declared else ():
        result.label(TO_REVIEW, f"diff: {signal}")
    prose = CODE_SPAN.sub("", CODE_BLOCK.sub("", text))
    if PLACEHOLDER_LINE.search(prose) or PLACEHOLDER.search(prose):
        result.errors.append("placeholder left (TODO, TBD, ..., <n>, <ID>, <reason>)")
    if MANUAL.search(prose):
        result.label(TO_TEST, "description: [manual] criteria to check by hand")
    return result


def report(result: Description) -> str:
    if result.errors:
        return ("## PR description: to fix\n\nSee the PR template (.github/pull_request_template.md).\n\n"
                + "".join(f"- {error}\n" for error in result.errors))
    kinds = ", ".join(f"`{name}`" for name in result.labels) or "none (safe)"
    return f"## PR description: passes\n\nLabels from the description: {kinds}.\n"


def main() -> None:
    parser = argparse.ArgumentParser(description="Check a pull request description against the PR template.")
    parser.add_argument("body", nargs="?", help="description file; without it, the pull request of GITHUB_EVENT_PATH")
    parser.add_argument("--base", help="with a description file: also check the structural changes of HEAD since this ref")
    args = parser.parse_args()
    config = structure.load_config(Path(structure.CONFIG))
    if args.body:
        body = Path(args.body).read_text(encoding="utf-8")
        structural = structure.diff_signals(args.base, "HEAD", config) if args.base else []
    else:
        pr = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))["pull_request"]
        body = pr.get("body") or ""
        structural = structure.diff_signals(pr["base"]["sha"], pr["head"]["sha"], config)
    result = check(body, structural)
    text = report(result)
    print(text)
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write(text)
    sys.exit(1 if result.errors else 0)


if __name__ == "__main__":
    main()
