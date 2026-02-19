import subprocess
import argparse
import os
import re


ABS_TMDL_PATH_RE = re.compile(r"/[^\s,\"']*\.tmdl")


def _stable_path_regex(path):
    candidate = path.replace("\\", "/")
    script_abs = os.path.abspath(__file__).replace("\\", "/")
    common = os.path.commonpath([os.path.abspath(path), script_abs]).replace("\\", "/")

    if common and common != "/" and candidate.startswith(common):
        suffix = candidate[len(common):]
        if suffix.startswith("/"):
            return "{{.*}}" + suffix

    return candidate


def _normalize_paths_for_check(line):
    return ABS_TMDL_PATH_RE.sub(lambda m: _stable_path_regex(m.group(0)), line)


def process_file(input_filename):
    with open(input_filename, "r") as f:
        lines = f.readlines()

    run_lines = [line for line in lines if line.startswith("// RUN:")]
    for line in run_lines:
        command_line = (
            line.replace("%S", os.path.dirname(input_filename))
            .split("// RUN:")[1]
            .strip()
            .split("|")[0]
            .strip()
            .split()[1:]
        )

    output = subprocess.check_output(["./target/debug/tmdlc"] + command_line)

    modified_lines = [
        line
        for line in lines
        if not line.startswith("// CHECK")
        and not line.startswith("// This file was generated")
        and not line.strip() == ''
    ]

    check_next = False

    for line in output.splitlines():
        string = line.decode("utf-8")
        string = _normalize_paths_for_check(string)
        if string.startswith("#"):
            string = string[1:]
        if string.strip() == '' or "#" in string:
            check_next = False
        else:
            if check_next:
                modified_lines.append("// CHECK-NEXT: " + string)
            else:
                modified_lines.append("// CHECK: " + string)
                check_next = True

    # Write modified lines back to file
    with open(input_filename, "w") as f:
        f.write("// This file was generated with ./utils/scripts/update_tmdlc_checks.py. Do not modify CHECKs manually.\n\n")
        for line in modified_lines:
            f.write(line)
            f.write("\n")


parser = argparse.ArgumentParser()
parser.add_argument("input", help="Input test file")
args = parser.parse_args()

process_file(args.input)
