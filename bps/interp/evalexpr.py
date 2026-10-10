from __future__ import annotations

from bps import ast
from bps.errors import DecodeError


def eval_expr(expr: ast.Expr, env: dict[str, object]) -> object:
    kind = expr.kind
    if kind == "int":
        return expr.value
    if kind == "bool":
        return expr.value
    if kind == "string":
        return expr.value
    if kind == "ident":
        name = expr.name
        if name not in env:
            raise DecodeError(f"unknown identifier {name!r}")
        return env[name]
    if kind == "path":
        parts = str(expr.value).split(".")
        cur: object = env
        for p in parts:
            if not isinstance(cur, dict) or p not in cur:
                raise DecodeError(f"bad path {expr.value!r}")
            cur = cur[p]
        return cur
    if kind == "unop":
        if expr.value == "!":
            return not bool(eval_expr(expr.left, env))
        raise DecodeError(f"unknown unop {expr.value}")
    if kind == "binop":
        op = expr.value
        left = eval_expr(expr.left, env)
        right = eval_expr(expr.right, env)
        if op == "||":
            return bool(left) or bool(right)
        if op == "&&":
            return bool(left) and bool(right)
        if op == "==":
            return left == right
        if op == "!=":
            return left != right
        if op == "<":
            return int(left) < int(right)
        if op == "<=":
            return int(left) <= int(right)
        if op == ">":
            return int(left) > int(right)
        if op == ">=":
            return int(left) >= int(right)
        if op == "+":
            return int(left) + int(right)
        if op == "-":
            return int(left) - int(right)
        if op == "*":
            return int(left) * int(right)
        if op == "/":
            return int(left) // int(right)
        if op == "%":
            return int(left) % int(right)
        if op == "<<":
            return int(left) << int(right)
        if op == ">>":
            return int(left) >> int(right)
        if op == "&":
            return int(left) & int(right)
        if op == "|":
            return int(left) | int(right)
        if op == "^":
            return int(left) ^ int(right)
        raise DecodeError(f"unknown binop {op}")
    if kind == "ternary":
        cond = eval_expr(expr.left, env)
        if cond:
            return eval_expr(expr.right, env)
        return eval_expr(expr.args[0], env)
    if kind == "call":
        name = expr.name
        args = [eval_expr(a, env) for a in expr.args]
        if name == "sign_extend":
            v, bits = int(args[0]), int(args[1])
            mask = 1 << (bits - 1)
            return (v ^ mask) - mask if v & mask else v
        if name == "utf16_units":
            s = str(args[0])
            return sum(2 if ord(ch) > 0xFFFF else 1 for ch in s)
        if name == "len":
            x = args[0]
            if isinstance(x, (bytes, bytearray, str, list, dict)):
                return len(x)
            raise DecodeError("len() on unsupported type")
        if name == "min":
            return min(int(args[0]), int(args[1]))
        if name == "max":
            return max(int(args[0]), int(args[1]))
        if name == "contains":
            return str(args[1]) in str(args[0])
        raise DecodeError(f"unknown call {name!r}")
    raise DecodeError(f"unknown expr kind {kind}")


def truthy(v: object) -> bool:
    return bool(v)
