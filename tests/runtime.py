"""Run the package tests against an unmodified Maki source directory."""

import argparse
import json
import os
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path


def toml_value(value):
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, list):
        return "[" + ", ".join(map(toml_value, value)) + "]"
    if isinstance(value, dict):
        return (
            "{ "
            + ", ".join(f"{json.dumps(k)} = {toml_value(v)}" for k, v in value.items())
            + " }"
        )
    return str(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("maki_source", type=Path)
    parser.add_argument("--toolchain", default="1.99.0")
    parser.add_argument(
        "--lint", action="store_true", help="Run Clippy before the runtime tests"
    )
    args = parser.parse_args()
    source = args.maki_source.resolve()
    package = Path(__file__).resolve().parent.parent
    workspace = tomllib.loads((source / "Cargo.toml").read_text())
    dependencies = workspace["workspace"]["dependencies"]
    manifest = [
        "[package]",
        'name = "maki-essential-runtime-tests"',
        'version = "0.0.0"',
        'edition = "2024"',
        "[[test]]",
        'name = "runtime"',
        f"path = {toml_value(str(package / 'tests/runtime.rs'))}",
        "[dependencies]",
    ]
    for name in (
        "maki-agent",
        "maki-config",
        "maki-lua",
        "maki-pack",
        "maki-providers",
        "maki-storage",
    ):
        dependency = {"path": str(source / name)}
        crate = tomllib.loads((source / name / "Cargo.toml").read_text())
        # Newer Maki builds keep the raw `fire_autocmd` behind this feature.
        if "test-support" in crate.get("features", {}):
            dependency["features"] = ["test-support"]
        manifest.append(f"{name} = {toml_value(dependency)}")
    shared_dependencies = [
        "flume",
        "serde_json",
        "tokio",
        "toml",
        "serde",
        "tempfile",
        "test-case",
    ]
    for name in shared_dependencies:
        manifest.append(f"{name} = {toml_value(dependencies[name])}")
    manifest.extend(
        ["[profile.dev]", "debug = 0", '[profile.dev.package."*"]', "debug = 0"]
    )

    with tempfile.TemporaryDirectory(prefix="maki-essential-runtime-") as temporary:
        root = Path(temporary)
        (root / "Cargo.toml").write_text("\n".join(manifest) + "\n")
        shutil.copyfile(source / "Cargo.lock", root / "Cargo.lock")
        env = os.environ.copy()
        env["MAKI_ESSENTIAL_PACKAGE"] = str(package)
        for name in ("CONFIG", "DATA", "STATE", "CACHE"):
            env[f"XDG_{name}_HOME"] = str(root / name.lower())
        env.setdefault("CARGO_TARGET_DIR", str(package / ".cache/runtime"))
        cargo = ["cargo", f"+{args.toolchain}"]
        options = ["--manifest-path", str(root / "Cargo.toml")]
        if args.lint:
            subprocess.run(
                cargo
                + ["clippy", "--all-targets"]
                + options
                + ["--", "-D", "warnings"],
                env=env,
                check=True,
            )
        subprocess.run(
            cargo + ["nextest", "run"] + options,
            env=env,
            check=True,
        )


if __name__ == "__main__":
    main()
