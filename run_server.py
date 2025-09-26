#!/usr/bin/env python3
import os
import shlex
import subprocess
from pathlib import Path
from typing import Optional

import typer
from rich import box
from rich.console import Console
from rich.table import Table

app = typer.Typer(add_completion=False)
console = Console()

DEFAULT_IMAGE = "quiche-shaped"
DEFAULT_PORT = 4433

def _ensure_dir(p: Path) -> Path:
    p.mkdir(parents=True, exist_ok=True)
    return p

def _percent(v: str) -> str:
    if not v.endswith("%"):
        raise typer.BadParameter("must end with % (e.g., 0%, 1%, 0.5%)")
    try:
        float(v[:-1])
    except ValueError:
        raise typer.BadParameter("not a number before %")
    return v

def _rate(v: str) -> str:
    # very light validation; tc understands kbit/mbit/gbit
    if not any(v.endswith(suf) for suf in ("kbit", "mbit", "gbit")):
        raise typer.BadParameter("use kbit/mbit/gbit, e.g. 5mbit")
    return v

def _time(v: str) -> str:
    # tc understands ms/s
    if not (v.endswith("ms") or v.endswith("s")):
        raise typer.BadParameter("use ms or s, e.g. 80ms")
    return v

@app.command()
def run(
    image: str = typer.Option(DEFAULT_IMAGE, help="Docker image to run"),
    port: int = typer.Option(DEFAULT_PORT, help="UDP port to expose on the host"),
    shape: bool = typer.Option(False, help="Enable traffic shaping inside container"),
    rate: str = typer.Option("5mbit", callback=_rate, help="Rate limit (e.g., 5mbit)"),
    burst: str = typer.Option("32kbit", help="Burst for TBF/NETEM rate"),
    lat: str = typer.Option("80ms", callback=_time, help="Base latency"),
    jit: str = typer.Option("20ms", callback=_time, help="Jitter"),
    loss: str = typer.Option("0%", callback=_percent, help="Packet loss percentage"),
    ingress: bool = typer.Option(False, help="Also shape ingress (IFB if available)"),
    qlogs: Optional[Path] = typer.Option(
        None,
        "--qlogs",
        help="Host folder to store qlogs (mounts to /qlogs in container). If omitted, no mount.",
    ),
    rust_log: str = typer.Option("info", help="RUST_LOG level in the container"),
    rust_backtrace: int = typer.Option(0, help="RUST_BACKTRACE in the container"),
    detach: bool = typer.Option(False, "--detach", "-d", help="Run container in background"),
    interactive: bool = typer.Option(True, "--interactive/--no-interactive", "-it/"),
    dry_run: bool = typer.Option(False, help="Print docker command and exit"),
    verbose: bool = typer.Option(False, help="Print extra info"),
):
    """
    Run quiche-server in Docker with optional NETEM shaping, using a friendly CLI.
    """

    # Build docker command
    cmd = ["docker", "run", "--rm"]
    if interactive and not detach:
        cmd += ["-it"]
    if detach:
        cmd += ["-d"]

    cmd += [
        "--init",
        "--cap-add", "NET_ADMIN",
        "-p", f"{port}:{port}/udp",
        "-e", f"SHAPE={'on' if shape else 'off'}",
        "-e", f"RATE={rate}",
        "-e", f"BURST={burst}",
        "-e", f"LAT={lat}",
        "-e", f"JIT={jit}",
        "-e", f"LOSS={loss}",
        "-e", f"INGRESS={'1' if ingress else '0'}",
        "-e", f"RUST_LOG={rust_log}",
        "-e", f"RUST_BACKTRACE={rust_backtrace}",
    ]

    # Optional qlog mount
    if qlogs is not None:
        host_dir = _ensure_dir(qlogs.resolve())
        # Avoid Git-Bash path mangling by ensuring a native path is passed to Docker
        cmd += ["-v", f"{str(host_dir)}:/qlogs"]

    cmd.append(image)
    # Rely on image ENTRYPOINT+CMD to start quiche-server with --cert/--key paths

    # Pretty preview
    table = Table(title="quiche-server run", box=box.SIMPLE_HEAVY)
    table.add_column("Setting", style="bold cyan", no_wrap=True)
    table.add_column("Value", style="white")
    table.add_row("image", image)
    table.add_row("port (udp)", str(port))
    table.add_row("shape", "on" if shape else "off")
    table.add_row("rate", rate)
    table.add_row("burst", burst)
    table.add_row("latency", lat)
    table.add_row("jitter", jit)
    table.add_row("loss", loss)
    table.add_row("ingress", "yes" if ingress else "no")
    table.add_row("qlogs mount", str(qlogs.resolve()) if qlogs else "(none)")
    table.add_row("RUST_LOG", rust_log)
    table.add_row("detach", "yes" if detach else "no")
    console.print(table)

    console.print("[bold]docker command:[/bold]")
    console.print(" ".join(shlex.quote(p) for p in cmd))

    if dry_run:
        raise typer.Exit(0)

    # Sanity check: image exists
    try:
        subprocess.run(
            ["docker", "image", "inspect", image],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        console.print(f"[red]Image not found:[/red] {image}")
        console.print("Build it first, e.g.:  [bold]docker build -t quiche-shaped .[/bold]")
        raise typer.Exit(1)

    # Run it
    env = os.environ.copy()
    # Prevent MSYS from rewriting /qlogs if the user launches this via Git Bash
    env.setdefault("MSYS2_ARG_CONV_EXCL", "QLOGDIR;*")
    if verbose:
        console.print(f"[dim]MSYS2_ARG_CONV_EXCL={env['MSYS2_ARG_CONV_EXCL']}[/dim]")

    try:
        subprocess.run(cmd, check=True, env=env)
    except subprocess.CalledProcessError as e:
        console.print(f"[red]docker exited with code {e.returncode}[/red]")
        raise typer.Exit(e.returncode)

if __name__ == "__main__":
    app()
