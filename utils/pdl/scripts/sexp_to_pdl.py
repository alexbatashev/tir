#!/usr/bin/env python3
"""One-off conversion of the s-expression axiom theory to PDL.

Reads an axiom file on stdin (a `(theory ...)` form or a bare sequence of
`(axiom ...)` forms) and writes PDL rules on stdout. Delete once
`core/defs/isel.pdl` and the backend rule files are committed.
"""

import re
import sys


def tokenize(text):
    text = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith(";"))
    return re.findall(r"\(|\)|[^\s()]+", text)


def parse(tokens, pos=0):
    if tokens[pos] != "(":
        return tokens[pos], pos + 1
    items, pos = [], pos + 1
    while tokens[pos] != ")":
        item, pos = parse(tokens, pos)
        items.append(item)
    return items, pos + 1


def forms(text):
    tokens = tokenize(text)
    pos, out = 0, []
    while pos < len(tokens):
        form, pos = parse(tokens, pos)
        out.append(form)
    return out


class Axiom:
    def __init__(self, form):
        self.name = form[1]
        self.sections = {}
        for section in form[2:]:
            self.sections.setdefault(section[0], []).extend(section[1:])
        self.widths = {}
        self.vars = {}
        for entry in self.sections.get("vars", []) + self.sections.get("consts", []):
            self.vars[entry[0]] = entry[1]
        self.consts = {e[0] for e in self.sections.get("consts", [])}
        for width in list(self.vars.values()) + self.sections.get("root", []):
            if not width.isdigit():
                self.widths[width] = width.upper()

    def width(self, name):
        return self.widths.get(name, name)

    def expr(self, e):
        """An integer expression over widths: `(- w n)`, `(ones e)`, atoms."""
        if isinstance(e, str):
            return self.width(e)
        head, rest = e[0], e[1:]
        if head == "-":
            return f"{self.expr(rest[0])} - {self.expr(rest[1])}"
        if head == "ones":
            return f"ones({self.expr(rest[0])})"
        raise ValueError(f"{self.name}: bad width expression {e}")

    def term(self, e, side, typed):
        if isinstance(e, str):
            if e == "root":
                return "root"
            if e in self.vars and e not in typed:
                typed.add(e)
                width = self.width(self.vars[e])
                kind = "const" if e in self.consts else "int"
                return f"{e}: {kind}<{width}>"
            if e in self.vars or (side == "rhs" and e in self.widths.values()):
                return e
            if e in self.widths:
                return self.widths[e]
            return e if not re.fullmatch(r"-?\d+", e) else e
        head, rest = e[0], e[1:]
        if head == "keep":
            return f"keep {self.term(rest[0], side, typed)}"
        if head == "const":
            return f"const<{self.expr(rest[1])}>({self.expr(rest[0])})"
        if head in ("-", "ones"):
            return self.expr(e)
        operands = ", ".join(self.term(o, side, typed) for o in rest)
        return f"#{head}({operands})"

    def guard(self, g):
        negated = g[0] == "not"
        if negated:
            g = g[1]
        head, rest = g[0], g[1:]
        if head in ("fits", "ufits"):
            return f"{'!' if negated else ''}{head}({rest[0]}, {rest[1]})"
        operator = {"<": "<", "=": "=="}[head]
        return f"{self.expr(rest[0])} {operator} {self.expr(rest[1])}"

    def render(self):
        typed = set()
        lhs = self.term(self.sections["lhs"][0], "lhs", typed)
        root = self.sections["root"][0]
        # A materialize rule's bare constant binder already carries the root width.
        if isinstance(self.sections["lhs"][0], list):
            lhs = f"{lhs} : int<{self.width(root)}>"
        rhs = self.term(self.sections["rhs"][0], "rhs", typed)
        text = f"rule {self.name}: {lhs}\n  => {rhs}"
        guards = self.sections.get("where", [])
        if guards:
            text += "\n  where " + ", ".join(self.guard(g) for g in guards)
        if self.sections.get("phase"):
            text += "\n  phase post-saturation"
        return text + ";"


def main():
    text = sys.stdin.read()
    top = forms(text)
    axioms = []
    for form in top:
        axioms.extend(form[1:] if form[0] == "theory" else [form])
    for axiom in axioms:
        print(Axiom(axiom).render())
        print()


if __name__ == "__main__":
    main()
