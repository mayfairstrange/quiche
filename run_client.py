#!/usr/bin/env python3
import os
import shlex
import subprocess
from typing import Optional

import typer
from rich.console import Console

app = typer.Typer(add_completion=False)
console = Console()

DEFAULT_IMAGE = "quiche-shaped"
DEFAULT_URL = "https://host.docker.internal:4433/"

@app.command()
def run(
    url: str = typer.Argument(DEFAULT_URL, help="Target URL (HTTP/3 over QUIC)"),
    image: str = typer.Option(DEFAULT_IMAGE, help="Docker image with quiche-client"),
    no_verify: bool = typer.Option(True, help="Skip TLS verification (for self-signed certs)"),
    qlogs: Optional[str] = typer.Option(
        None, "--qlogs", help="Host directory to store qlog traces (mounts to /qlogs)"
    ),
    extra: Optional[str] = typer.Option(
        None, "--extra", help="Extra args to pass to quiche-client"
    ),
    dry_run: bool = typer.Option(False, "--dry-run", help="Only print the docker command"),
):
    """
    Run quiche-client in Docker, simplified.
    """

    cmd = ["docker", "run", "--rm", "-it"]

    # Optional qlogs mount
    if qlogs:
        host_dir = os.path.abspath(qlogs)
        os.makedirs(host_dir, exist_ok=True)
        cmd += ["-e", "QLOGDIR=/qlogs", "-v", f"{host_dir}:/qlogs"]

    # Image + quiche-client binary
    cmd.append(image)
    cmd.append("quiche-client")

    if no_verify:
        cmd.append("--no-verify")

    # Append URL
    cmd.append(url)

    # Extra args (if user wants to tweak)
    if extra:
        cmd += shlex.split(extra)

    console.print("[bold]docker command:[/bold]")
    console.print(" ".join(shlex.quote(p) for p in cmd))

    if dry_run:
        raise typer.Exit()

    # Run
    subprocess.run(cmd, check=True)

if __name__ == "__main__":
    app()
