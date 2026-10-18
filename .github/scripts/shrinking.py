"""Lines a list file gained against master. Accepted-findings lists use it to
make sure they only shrink."""

import collections
import os
import subprocess


def added_lines(root, path):
    base = os.environ.get("LINT_BASE", "master")
    subprocess.run(["git", "rev-parse", "--verify", "--quiet", base], cwd=root, check=True,
                   stdout=subprocess.DEVNULL)
    before = subprocess.run(["git", "show", f"{base}:{path}"], cwd=root, capture_output=True,
                            text=True)
    if before.returncode != 0:
        # The list is new on this branch, so everything in it is its baseline.
        return []
    baseline = before.stdout
    for old, new in renamed_files(root, base):
        baseline = baseline.replace(old, new)
    gained = (collections.Counter((root / path).read_text().splitlines())
              - collections.Counter(baseline.splitlines()))
    return sorted(gained.elements())


def renamed_files(root, base):
    diff = subprocess.run(["git", "diff", "--name-status", "--find-renames", "--diff-filter=R",
                           base], cwd=root, capture_output=True, text=True, check=True)
    for line in diff.stdout.splitlines():
        _, old, new = line.split("\t")
        yield old, new
