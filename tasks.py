import os
import sys

from invoke import Collection, task

HAS_COLORS: bool = (
    sys.stderr.isatty() or os.getenv("CLICOLOR_FORCE")
) and not os.getenv("NO_COLOR")


def colored(msg: object, /, *, code: str) -> str:
    return f"\x1b[{code}m{msg}\x1b[0m" if HAS_COLORS else str(msg)


@task
def test(ctx):
    check(ctx, format=False)
    ctx.run("cargo nextest run --workspace", pty=True)
    run_format(ctx, check=True)


@task(name="format")
def run_format(ctx, check=False):
    print("INFO:", "Checking" if check else "Fixing", "formatting")
    maybe_check = " --check" if check else ""
    maybe_fix = " --fix" if not check else ""
    ctx.run("cargo +nightly fmt --all" + maybe_check)
    ctx.run("tombi format" + maybe_check)
    # python
    ctx.run("ruff format" + maybe_check)
    ctx.run("ruff check --select=I" + maybe_fix)  # isort
    typos(ctx, fix=False)


@task
def clippy(ctx):
    ctx.run("cargo clippy --workspace --all-targets", pty=True)


@task
def check(ctx, format=True, lint=True):
    clippy(ctx)
    ctx.run("cargo doc --workspace --no-deps", pty=True)
    if format:
        run_format(ctx, check=True)


TYPOS_VER = "1.48"  # pinned to avoid update breakage


@task
def typos(ctx, fix=False):
    maybe_write = " --write-changes" if fix else ""
    ctx.run(f"uvx typos@{TYPOS_VER}{maybe_write}")


ns = Collection(test, run_format, clippy, check, typos)
ns.configure(
    {
        "run": {
            "echo": True,
            "env": {"CLICOLOR_FORCE": "1" if HAS_COLORS else "0"},
        }
    }
)
