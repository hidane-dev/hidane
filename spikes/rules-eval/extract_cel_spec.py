"""Extracts the cel-spec conformance cases that matter for Firestore Rules into cases/cel-spec.json.

Usage: python3 -I extract_cel_spec.py <dir with cel-spec tests/simple/testdata/*.textproto>

The files come from https://github.com/google/cel-spec (tag v0.25.3, Apache-2.0). A case is
kept when its expression only uses what the Rules language has: no uint, no protobuf messages,
no type values or conversions Rules lacks, no macros, no optional syntax. Every skipped case is
counted with its reason.
"""

import base64
import json
import math
import re
import sys

FILES = ["basic", "integer_math", "fp_math", "comparisons", "logic", "string", "lists",
         "conversions", "fields", "parse", "plumbing"]

# --- A small reader for the protobuf text format, enough for SimpleTestFile ---------------------

ESCAPES = {"n": 10, "t": 9, "r": 13, "a": 7, "b": 8, "f": 12, "v": 11,
           "\\": 92, "'": 39, '"': 34, "?": 63}


def tokens(text):
    i = 0
    while i < len(text):
        c = text[i]
        if c.isspace():
            i += 1
        elif c == "#":
            while i < len(text) and text[i] != "\n":
                i += 1
        elif c in "{}[]:<>,;":
            yield c
            i += 1
        elif c in "'\"":
            out = bytearray()
            i += 1
            while text[i] != c:
                if text[i] == "\\":
                    i += 1
                    e = text[i]
                    if e in ESCAPES:
                        out.append(ESCAPES[e])
                        i += 1
                    elif e in "01234567":
                        j = i
                        while j < i + 3 and text[j] in "01234567":
                            j += 1
                        out.append(int(text[i:j], 8))
                        i = j
                    elif e == "x":
                        j = i + 1
                        while j < i + 3 and text[j] in "0123456789abcdefABCDEF":
                            j += 1
                        out.append(int(text[i + 1:j], 16))
                        i = j
                    elif e in "uU":
                        n = 4 if e == "u" else 8
                        out += chr(int(text[i + 1:i + 1 + n], 16)).encode()
                        i += 1 + n
                    else:
                        raise ValueError(f"escape \\{e}")
                else:
                    out += text[i].encode()
                    i += 1
            i += 1
            yield ("str", bytes(out))
        else:
            j = i
            while j < len(text) and not text[j].isspace() and text[j] not in "{}[]:<>,;#'\"":
                j += 1
            yield ("word", text[i:j])
            i = j


def parse_message(toks, end=None):
    """Returns a list of (field, value) pairs; a value is bytes, a word or a nested list.
    `toks` is reversed, so pop() takes the next token."""
    fields = []
    while toks:
        t = toks.pop()
        if t == end:
            return fields
        if t in (",", ";"):
            continue
        if t == "[":  # [type.googleapis.com/...] inside an Any
            name = ""
            while (t := toks.pop()) != "]":
                name += t[1]
        else:
            name = t[1]
        t = toks.pop()
        if t == ":":
            t = toks.pop()
        if t in ("{", "<"):
            fields.append((name, parse_message(toks, "}" if t == "{" else ">")))
        else:
            value = t[1]
            # Adjacent string literals concatenate.
            while t[0] == "str" and toks and isinstance(toks[-1], tuple) and toks[-1][0] == "str":
                value += toks.pop()[1]
            fields.append((name, value))
    return fields


def parse_file(path):
    toks = list(tokens(open(path, encoding="utf-8").read()))
    toks.reverse()
    return parse_message(toks)


def get(fields, name):
    return [v for k, v in fields if k == name]


def one(fields, name, default=None):
    v = get(fields, name)
    return v[0] if v else default


# --- Conversion of ExprValue / Value to the spike's tagged JSON --------------------------------

class Skip(Exception):
    pass


def value(fields):
    (kind, v), = fields
    if kind == "int64_value":
        return {"int": str(int(v))}
    if kind == "double_value":
        f = float(v.replace("infinity", "inf"))
        if math.isnan(f):
            return {"double": "NaN"}
        if math.isinf(f):
            return {"double": "Infinity" if f > 0 else "-Infinity"}
        return {"double": f}
    if kind == "string_value":
        return {"string": v.decode("utf-8")}
    if kind == "bytes_value":
        return {"bytes": base64.b64encode(v).decode()}
    if kind == "bool_value":
        return {"bool": v == "true"}
    if kind == "null_value":
        return {"null": None}
    if kind == "list_value":
        return {"list": [value(x) for x in get(v, "values")]}
    if kind == "map_value":
        return {"map": [[value(one(e, "key")), value(one(e, "value"))] for e in get(v, "entries")]}
    raise Skip(f"value:{kind}")


UNSUPPORTED = [
    (r"\b\d+[uU]\b|\b0x[0-9a-fA-F]+[uU]\b", "uint"),
    (r"\buint\b", "uint"),
    (r"\b(dyn|type)\b", "type values"),
    (r"\b(int|bool|string|bytes|double|list|map|null_type)\b(?!\s*\()", "type values"),
    (r"\bdouble\(", "double()"),
    (r"\bbytes\(", "bytes()"),
    (r"google\.protobuf|TestAllTypes|NestedTestAllTypes|cel\.expr|\bproto[23]?\b", "protobuf"),
    (r"\.(all|exists|exists_one|map|filter)\(|\bhas\(", "macros"),
    (r"\boptional\b|\.\?|\[\?|\?\.", "optional"),
    (r"\b(timestamp|duration)\(", "timestamp/duration"),
    (r"`", "escaped identifiers"),
    (r"\.\s*[A-Z]\w*\s*\{|^\s*[A-Z]\w*\s*\{", "messages"),
]


def main():
    src = sys.argv[1]
    cases, skipped = [], {}

    def skip(reason):
        skipped[reason] = skipped.get(reason, 0) + 1

    for f in FILES:
        for section in get(parse_file(f"{src}/{f}.textproto"), "section"):
            sname = one(section, "name").decode()
            for test in get(section, "test"):
                name = one(test, "name").decode()
                expr = one(test, "expr").decode("utf-8")
                keys = {k for k, _ in test}
                if keys & {"type_env", "container", "check_only", "typed_result",
                           "unknown", "any_unknowns", "disable_macros"}:
                    skip("needs a type checker, container or unknowns")
                    continue
                reason = next((r for p, r in UNSUPPORTED if re.search(p, expr)), None)
                if reason:
                    skip(reason)
                    continue
                try:
                    bindings = {}
                    for b in get(test, "bindings"):
                        bindings[one(b, "key").decode()] = value(one(one(b, "value"), "value"))
                    if "value" in keys:
                        expect = value(one(test, "value"))
                    elif keys & {"eval_error", "any_eval_errors"}:
                        err = one(test, "eval_error") or one(test, "any_eval_errors")
                        msgs = [one(e, "message", b"").decode() for e in get(err, "errors")]
                        expect = {"error": "; ".join(msgs)}
                    else:
                        expect = {"bool": True}  # SimpleTest's default result
                except Skip as e:
                    skip(str(e))
                    continue
                cases.append({"file": f, "section": sname, "name": name, "expr": expr,
                              "bindings": bindings, "expect": expect})

    out = {
        "source": "https://github.com/google/cel-spec/tree/v0.25.3/tests/simple/testdata (Apache-2.0)",
        "files": FILES,
        "kept": len(cases),
        "skipped": dict(sorted(skipped.items(), key=lambda kv: -kv[1])),
        "cases": cases,
    }
    json.dump(out, sys.stdout, indent=1, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
