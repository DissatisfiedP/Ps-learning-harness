# REPL driver for the `repl` built-in plugin (crates/harness/src/plugins/native/repl.rs).
# One JSON request per stdin line: {"op": "eval" | "check" | "syntax", "code": "..."}.
# Printed output goes to stdout as it happens; each request ends with
# RES + one JSON line, then END on its own line.
import ast
import io
import json
import sys
import traceback

RES = "\x1eLHRES\x1e"
END = "\x1eLHEND\x1e"
proto = sys.stdin
# The learner's code must not read the protocol stream.
sys.stdin = io.StringIO("")
ns = {"__name__": "__main__", "__builtins__": __builtins__}


def reply(payload):
    sys.stdout.flush()
    sys.stderr.flush()
    sys.__stdout__.write(RES + json.dumps(payload) + "\n" + END + "\n")
    sys.__stdout__.flush()


def error(e):
    tb = e.__traceback__.tb_next if e.__traceback__ else None
    return "".join(traceback.format_exception(type(e), e, tb)).rstrip()


for line in proto:
    try:
        req = json.loads(line)
    except ValueError:
        continue
    op = req.get("op")
    code = req.get("code", "")
    try:
        if op == "syntax":
            compile(code, "<check>", "eval")
            reply({"ok": True})
        elif op == "check":
            value = eval(compile(code, "<check>", "eval"), ns)
            reply({"ok": True, "value": repr(value), "truthy": bool(value)})
        else:
            tree = ast.parse(code, "<repl>", "exec")
            last = None
            if tree.body and isinstance(tree.body[-1], ast.Expr):
                last = ast.Expression(tree.body.pop().value)
            exec(compile(tree, "<repl>", "exec"), ns)
            value = None
            if last is not None:
                result = eval(compile(last, "<repl>", "eval"), ns)
                if result is not None:
                    ns["_"] = result
                    value = repr(result)
            reply({"ok": True, "value": value})
    except SystemExit:
        reply({"ok": False, "error": "exit() does nothing here; the session stays open"})
    except BaseException as e:  # noqa: BLE001 - every error is the learner's to read
        reply({"ok": False, "error": error(e)})
