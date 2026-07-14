from invoke import task


@task
def format(ctx):
    ctx.run("cargo +nightly fmt")
    ctx.run("ruff format")
