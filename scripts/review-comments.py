#!/usr/bin/env python3

import sys
from pathlib import Path


import re

ISSUE_RE = re.compile(r"(?<!\w)#\d+\b")


def find_comments(text):
    """Find groups of // comments containing a GitHub issue number."""

    lines = text.splitlines(keepends=True)
    comments = []

    in_block = False
    in_string = False
    in_raw_string = False
    raw_hashes = 0

    line = 0

    while line < len(lines):
        current = lines[line]
        j = 0

        while j < len(current):
            if in_block:
                end = current.find("*/", j)
                if end == -1:
                    j = len(current)
                    continue
                in_block = False
                j = end + 2
                continue

            if in_raw_string:
                terminator = '"' + ('#' * raw_hashes)
                end = current.find(terminator, j)
                if end == -1:
                    j = len(current)
                    continue
                in_raw_string = False
                j = end + len(terminator)
                continue

            if in_string:
                if current[j] == '\\':
                    j += 2
                    continue
                if current[j] == '"':
                    in_string = False
                j += 1
                continue

            # Raw string: r"...", r#"..."#, etc.
            if current[j] == 'r':
                k = j + 1
                while k < len(current) and current[k] == '#':
                    k += 1

                if k < len(current) and current[k] == '"':
                    in_raw_string = True
                    raw_hashes = k - (j + 1)
                    j = k + 1
                    continue

            if current[j] == '"':
                in_string = True
                j += 1
                continue

            # Character literal.
            if current[j] == "'":
                j += 1
                while j < len(current):
                    if current[j] == '\\':
                        j += 2
                    elif current[j] == "'":
                        j += 1
                        break
                    else:
                        j += 1
                continue

            if current.startswith("/*", j):
                in_block = True
                j += 2
                continue

            if current.startswith("//", j):
                # Ignore /// and //!
                if j + 2 < len(current) and current[j + 2] in "/!":
                    j += 3
                    continue

                # Found a normal // comment.
                start_line = line
                end_line = line

                # Include consecutive // comment lines.
                while end_line + 1 < len(lines):
                    next_line = lines[end_line + 1]
                    stripped = next_line.lstrip()

                    if (
                        stripped.startswith("//")
                        and not stripped.startswith("///")
                        and not stripped.startswith("//!")
                    ):
                        end_line += 1
                    else:
                        break

                # Only show comments containing #<number>.
                comment = "".join(lines[start_line:end_line + 1])

                if ISSUE_RE.search(comment):
                    comments.append((start_line, end_line))

                line = end_line
                break

            j += 1

        line += 1

    return comments

def process_file(path):
    text = path.read_text()
    comments = find_comments(text)

    if not comments:
        return False

    lines = text.splitlines(keepends=True)
    to_delete = []

    for start, end in comments:
        comment = "".join(lines[start:end + 1])

        print("\n" + "=" * 80)
        print(f"{path}:{start + 1}")
        print("-" * 80)
        print(comment, end="")
        print("-" * 80)

        while True:
            answer = input("[d]elete / [k]eep / [q]uit: ").strip().lower()

            if answer in ("d", "delete"):
                to_delete.append((start, end))
                break

            if answer in ("k", "keep", ""):
                break

            if answer in ("q", "quit"):
                return None

            print("Please enter d, k, or q.")

    # Delete selected comments from bottom to top so line numbers stay valid.
    for start, end in reversed(to_delete):
        del lines[start:end + 1]

    new_text = "".join(lines)

    if new_text != text:
        path.write_text(new_text)
        return True

    return False


def main():
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")

    if not root.is_dir():
        print(f"Not a directory: {root}")
        sys.exit(1)

    files = sorted(root.rglob("*.rs"))

    print(f"Found {len(files)} Rust files.")

    for path in files:
        print(f"\nProcessing {path}")

        result = process_file(path)

        if result is None:
            print("\nStopped.")
            return

    print("\nDone.")


if __name__ == "__main__":
    main()
