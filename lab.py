#!/usr/bin/env python3
"""取得と計測と結果整理"""
import argparse
from collections import Counter
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch_main():
    manifest = json.loads((ROOT / "assets.lock.json").read_text())
    for asset in manifest["assets"]:
        path = ROOT / asset["path"]
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists() or digest(path) != asset["sha256"]:
            request = urllib.request.Request(asset["url"], headers={"User-Agent": "shaping-lab"})
            with urllib.request.urlopen(request, timeout=120) as response:
                data = response.read()
            if hashlib.sha256(data).hexdigest() != asset["sha256"]:
                raise RuntimeError(f"SHA-256 mismatch: {asset['path']}")
            temp = path.with_suffix(path.suffix + ".tmp")
            temp.write_bytes(data)
            temp.replace(path)
        if "extract" in asset:
            destination = ROOT / asset["extract"]
            stamp = destination / ".archive-sha256"
            if not stamp.exists() or stamp.read_text() != asset["sha256"]:
                if destination.exists():
                    raise RuntimeError(f"Existing source directory has a different archive version: {destination}")
                with tarfile.open(fileobj=io.BytesIO(path.read_bytes()), mode="r:gz") as archive:
                    for member in archive.getmembers():
                        if not member.isfile():
                            continue
                        if not member.name.startswith(asset["prefix"]):
                            raise RuntimeError("Invalid archive path")
                        relative = Path(member.name[len(asset["prefix"]):])
                        if relative.is_absolute() or ".." in relative.parts:
                            raise RuntimeError("Invalid archive path")
                        output = destination / relative
                        output.parent.mkdir(parents=True, exist_ok=True)
                        with archive.extractfile(member) as source, output.open("wb") as target:
                            shutil.copyfileobj(source, target)
                stamp.write_text(asset["sha256"])
        print(asset["path"])


def capture(root, output, env):
    result = subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root, env=env, check=True, capture_output=True, text=True)
    metadata = json.loads(result.stdout)
    packages = []
    for package in sorted(metadata["packages"], key=lambda p:(p["name"],p["version"])):
        item = {key:package[key] for key in ["name", "version", "source", "manifest_path"]}
        if package["source"] is None:
            directory = Path(package["manifest_path"]).parent
            digest = hashlib.sha256()
            paths = [Path("Cargo.toml"), Path("build.rs")]
            paths.extend(p.relative_to(directory) for p in (directory / "src").rglob("*") if p.is_file())
            for relative in sorted(set(paths)):
                path = directory / relative
                if not path.is_file():
                    continue
                data = path.read_bytes()
                digest.update(str(relative).encode())
                digest.update(len(data).to_bytes(8, "little"))
                digest.update(data)
            item["source_sha256"] = digest.hexdigest()
            for key,args in [("commit",["rev-parse","HEAD"]),("status",["status","--porcelain"])]:
                result = subprocess.run(["git", "-C", str(directory), *args], capture_output=True, text=True)
                item[key] = result.stdout.strip() if result.returncode == 0 else None
        packages.append(item)
    (output / "dependencies.json").write_text(json.dumps(packages, indent=2) + "\n")


def summarize_main():
    parser = argparse.ArgumentParser(description="Summarize Criterion results")
    parser.add_argument("directory", type=Path)
    directory = parser.parse_args().directory
    rows = []
    for path in sorted((directory / "criterion").rglob("new/estimates.json")):
        estimate = json.loads(path.read_text())["mean"]
        bench = json.loads((path.parent / "benchmark.json").read_text())
        interval = estimate["confidence_interval"]
        rows.append([bench["full_id"], estimate["point_estimate"], interval["lower_bound"], interval["upper_bound"], interval["confidence_level"]])
    with (directory / "summary.csv").open("w", newline="") as file:
        writer = csv.writer(file)
        writer.writerow(["benchmark", "mean_ns", "ci_low_ns", "ci_high_ns", "confidence_level"])
        writer.writerows(rows)
    print(f"Results: {directory}")
    print(f"Summary: {directory / 'summary.csv'}")
    print(f"Report:  {directory / 'criterion/report/index.html'}")


def sample_summary_main():
    parser = argparse.ArgumentParser(description="Summarize macOS CPU samples")
    parser.add_argument("directory", type=Path)
    directory = parser.parse_args().directory
    lines = (directory / "sample.txt").read_text().splitlines()
    inclusive = Counter()
    own = Counter()
    section = ""
    for line in lines:
        if line == "Call graph:":
            section = "graph"
        elif line.startswith("Total number in stack"):
            section = ""
        elif line.startswith("Sort by top of stack"):
            section = "self"
        elif line.startswith("Binary Images:") or line.startswith("Binary images"):
            section = ""
        elif section == "graph":
            match = re.match(r"^[\s+!:|]*(\d+) (.*?)  \(in ([^)]+)\)", line)
            if match:
                count, symbol, module = match.groups()
                inclusive[(symbol, module)] += int(count)
        elif section == "self":
            match = re.match(r"^\s+(.*?)  \(in ([^)]+)\)\s+(\d+)$", line)
            if match:
                symbol, module, count = match.groups()
                own[(symbol, module)] += int(count)
    with (directory / "hotspots.csv").open("w", newline="") as file:
        writer = csv.writer(file)
        writer.writerow(["symbol", "module", "self_samples", "inclusive_samples"])
        for key in sorted(inclusive.keys() | own.keys(), key=lambda k:(-own[k], -inclusive[k], k)):
            writer.writerow([*key, own[key], inclusive[key]])
    print(f"Hotspots: {directory / 'hotspots.csv'}")


def diff_shapes_main():
    parser = argparse.ArgumentParser(description="Compare saved shape JSON files")
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    args = parser.parse_args()
    left, right = [json.loads(p.read_text()) for p in [args.left, args.right]]
    for key in ["corpus", "fonts"]:
        a, b = left["metadata"][key], right["metadata"][key]
        if key == "corpus":
            a, b = a["spec"], b["spec"]
        if a != b:
            sys.exit(f"Input mismatch: {key}")
    if [c["case"] for c in left["cases"]] != [c["case"] for c in right["cases"]]:
        sys.exit("Case selection mismatch")
    differences = []
    for a, b in zip(left["cases"], right["cases"]):
        for run in range(max(len(a["runs"]), len(b["runs"]))):
            ar = a["runs"][run] if run < len(a["runs"]) else []
            br = b["runs"][run] if run < len(b["runs"]) else []
            if run >= min(len(a["runs"]), len(b["runs"])):
                differences.append({"case":a["case"],"run":run,"error":"Missing run"})
            for index in range(max(len(ar),len(br))):
                av = ar[index] if index < len(ar) else None
                bv = br[index] if index < len(br) else None
                if av != bv:
                    differences.append({"case":a["case"],"run":run,"glyph":index,"left":av,"right":bv})
    print(json.dumps({"equal":not differences,"differences":differences},indent=2))
    sys.exit(2 if differences else 0)


def bench_main():
    parser = argparse.ArgumentParser(description="Save a Criterion run with its provenance")
    parser.add_argument("label")
    parser.add_argument("--baseline")
    parser.add_argument("--suite", choices=["shaping", "stages", "stream", "batch"], default="shaping")
    parser.add_argument("--features", default="")
    parser.add_argument("--split-runs", action="store_true")
    parser.add_argument("--batch-size", type=int, default=64)
    parser.add_argument("--batch-sizes", default="1,32,128,512,2048")
    parser.add_argument("--workers", default="1,2,4,8")
    parser.add_argument("--min-runs-per-worker", type=int, default=32)
    parser.add_argument("--case")
    parser.add_argument("--corpus", default="corpus/default.toml")
    parser.add_argument("--backends", default="harfbuzz,local,optimized")
    parser.add_argument("--modes", default="reuse")
    parser.add_argument("--plan", choices=["cached", "rebuild", "native"], default="cached")
    parser.add_argument("--seconds", type=float, default=3)
    parser.add_argument("--samples", type=int, default=50)
    args = parser.parse_args()
    for label in [args.label, args.baseline]:
        if label is not None and not re.fullmatch(r"[A-Za-z0-9_-]+", label):
            parser.error("Labels may contain only letters, digits, '-' and '_'")
    if not 0 < args.seconds <= 86400 or args.samples < 10:
        parser.error("seconds must be in (0, 86400] and samples must be at least 10")
    if not 1 <= args.batch_size <= 1024:
        parser.error("batch-size must be in [1, 1024]")
    try:
        batch_sizes = [int(value) for value in args.batch_sizes.split(',')]
        workers = [int(value) for value in args.workers.split(',')]
    except ValueError:
        parser.error("batch-sizes and workers must be comma-separated integers")
    if not batch_sizes or any(not 1 <= value <= 65536 for value in batch_sizes):
        parser.error("batch-sizes must be in [1, 65536]")
    if not workers or any(not 1 <= value <= 64 for value in workers):
        parser.error("workers must be in [1, 64]")
    if args.min_runs_per_worker < 1:
        parser.error("min-runs-per-worker must be positive")
    if args.suite != "shaping" and (args.baseline or args.plan != "cached" or args.modes != "reuse"):
        parser.error("Selected suite requires cached plans and reuse mode without a baseline")
    if args.suite != "stages" and args.split_runs:
        parser.error("split-runs requires --suite stages")
    output = ROOT / "results" / args.label
    if output.exists():
        parser.error(f"Output already exists: {output}")
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("SHAPING_") and key != "SHAPING_HARFBUZZ_SOURCE":
            env.pop(key)
    env.update(SHAPING_CORPUS=args.corpus, SHAPING_BACKENDS=args.backends,
               SHAPING_MODES=args.modes, SHAPING_PLAN=args.plan, SHAPING_OUTPUT=str(output))
    env["SHAPING_SUITE"] = args.suite
    env.update(SHAPING_BATCH_SIZE=str(args.batch_size), SHAPING_SPLIT_RUNS=str(int(args.split_runs)))
    env.update(SHAPING_BATCH_SIZES=args.batch_sizes, SHAPING_WORKERS=args.workers,
               SHAPING_MIN_RUNS_PER_WORKER=str(args.min_runs_per_worker))
    if args.case:
        env["SHAPING_CASE"] = args.case
    baseline = ROOT / "results" / args.baseline if args.baseline else None
    if baseline and not (baseline / "metadata.json").is_file():
        parser.error(f"Baseline not found: {baseline}")
    output.mkdir(parents=True)
    capture(ROOT, output, env)
    if baseline:
        shutil.copytree(baseline / "criterion", output / "criterion")
        env["SHAPING_BASELINE_METADATA"] = str(baseline / "metadata.json")
    command = ["cargo", "bench", "--locked", "--bench", "shaping"]
    if args.features:
        command.extend(["--features", args.features])
    command.extend(["--",
               "--sample-size", str(args.samples), "--measurement-time", str(args.seconds)])
    command.extend(["--baseline", args.baseline] if baseline else ["--save-baseline", args.label])
    invocation = {"runner_sha256":digest(ROOT / "lab.py"),"command":command,"arguments":vars(args),"status":"running"}
    (output / "invocation.json").write_text(json.dumps(invocation, indent=2) + "\n")
    with (output / "console.txt").open("w") as log:
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in process.stdout:
            print(line, end="", flush=True)
            log.write(line)
        code = process.wait()
    invocation.update(status="complete" if code == 0 else "failed", exit_code=code)
    (output / "invocation.json").write_text(json.dumps(invocation, indent=2) + "\n")
    if code == 0:
        # 比較後の結果も次の基準として利用
        for latest in (output / "criterion").rglob("new/estimates.json"):
            destination = latest.parent.parent / args.label
            if not destination.exists():
                shutil.copytree(latest.parent, destination)
        subprocess.run([sys.executable, str(ROOT / "lab.py"), "summarize", str(output)], check=True)
    sys.exit(code)


def profile_main():
    parser = argparse.ArgumentParser(description="Capture a CPU profile of a shaping workload")
    parser.add_argument("label")
    parser.add_argument("--case", required=True)
    parser.add_argument("--backend", choices=["harfbuzz", "local", "optimized"], required=True)
    parser.add_argument("--suite", choices=["shaping", "batch"], default="shaping")
    parser.add_argument("--batch-size", type=int, default=8192)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--min-runs-per-worker", type=int, default=32)
    parser.add_argument("--mode", choices=["reuse", "fresh-buffer", "fresh-font"], default="reuse")
    parser.add_argument("--plan", choices=["cached", "rebuild", "native"], default="cached")
    parser.add_argument("--corpus", default="corpus/default.toml")
    parser.add_argument("--features", default="")
    parser.add_argument("--seconds", type=int, default=20)
    parser.add_argument("--tool", choices=["sample", "perf"], default="sample" if sys.platform == "darwin" else "perf")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.label):
        parser.error("Invalid output label")
    if not 2 <= args.seconds <= 86400:
        parser.error("seconds must be in [2, 86400]")
    if not 1 <= args.batch_size <= 65536 or not 1 <= args.workers <= 64 or args.min_runs_per_worker < 1:
        parser.error("Invalid batch size, worker count or minimum runs per worker")
    if args.suite == "batch" and (args.backend != "optimized" or args.mode != "reuse" or args.plan != "cached"):
        parser.error("Batch profiling requires optimized, reuse and cached plans")
    if not shutil.which(args.tool):
        parser.error(f"Profiler not found: {args.tool}")
    output = ROOT / "results" / args.label
    if output.exists():
        parser.error(f"Output already exists: {output}")
    env = os.environ.copy()
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C force-frame-pointers=yes").strip()
    build = ["cargo", "build", "--locked", "--profile", "profiling"]
    if args.features:
        build.extend(["--features", args.features])
    if args.suite == "batch":
        build = ["cargo", "bench", "--locked", "--profile", "profiling", "--bench", "shaping",
                 "--no-run", "--message-format=json"] + (["--features", args.features] if args.features else [])
        result = subprocess.run(build, cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True)
        messages = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        binary = next(Path(m["executable"]) for m in reversed(messages)
                      if m.get("reason") == "compiler-artifact" and m.get("target", {}).get("name") == "shaping" and m.get("executable"))
        env.update(SHAPING_SUITE="batch", SHAPING_CASE=args.case, SHAPING_CORPUS=args.corpus,
                   SHAPING_BATCH_SIZES=str(args.batch_size), SHAPING_WORKERS=str(args.workers),
                   SHAPING_MIN_RUNS_PER_WORKER=str(args.min_runs_per_worker), SHAPING_OUTPUT=str(output))
    else:
        subprocess.run(build, cwd=ROOT, env=env, check=True)
        target = Path(env.get("CARGO_TARGET_DIR", str(ROOT / "target")))
        if not target.is_absolute():
            target = ROOT / target
        binary = target / "profiling" / "shape-lab"
    output.mkdir(parents=True)
    capture(ROOT, output, env)
    command = [str(binary), "--corpus", args.corpus, "--case", args.case, "--plan", args.plan,
               "profile", "--backend", args.backend, "--mode", args.mode,
               "--seconds", str(args.seconds), "--out", str(output / "workload.json")]
    if args.suite == "batch":
        command = [str(binary), "--profile-time", str(args.seconds), "--noplot", "--bench"]
    invocation = {"runner_sha256":digest(ROOT / "lab.py"),"arguments":vars(args),"command":command,"status":"running"}
    path = output / "invocation.json"
    path.write_text(json.dumps(invocation, indent=2) + "\n")
    with (output / "console.txt").open("w") as log:
        if args.tool == "sample":
            process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=log)
            try:
                result = subprocess.run(["sample", str(process.pid), str(args.seconds - 1), "1", "-file", str(output / "sample.txt")], stdout=log, stderr=log)
                code = process.wait() or result.returncode
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait()
        else:
            code = subprocess.run(["perf", "record", "-g", "--call-graph", "dwarf", "-o", str(output / "perf.data"), "--", *command], cwd=ROOT, env=env, stdout=log, stderr=log).returncode
    if code == 0 and args.suite == "batch":
        metadata = json.loads((output / "metadata.json").read_text())
        if (metadata.get("suite") != "batch" or metadata.get("workers") != [args.workers]
                or metadata.get("batch_sizes") != [args.batch_size]
                or [case["id"] for case in metadata.get("cases", [])] != [args.case]):
            print("Profile workload differs from the requested batch settings", file=sys.stderr)
            code = 1
    invocation.update(status="complete" if code == 0 else "failed", exit_code=code)
    path.write_text(json.dumps(invocation, indent=2) + "\n")
    print(f"Results: {output}")
    if code == 0 and args.tool == "sample":
        subprocess.run([sys.executable, str(ROOT / "lab.py"), "sample-summary", str(output)], check=True)
    if code:
        print(f"Profiler failed; see {output / 'console.txt'}", file=sys.stderr)
    sys.exit(code)


def output(command, cwd=None):
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def upstream_main():
    parser = argparse.ArgumentParser(description="Run the pinned upstream benchmark without source changes")
    parser.add_argument("label")
    parser.add_argument("--filter", default="")
    parser.add_argument("--seconds", type=float)
    parser.add_argument("--samples", type=int)
    parser.add_argument("--cxxflags", default="")
    parser.add_argument("--harfbuzz", choices=["bundled", "lab", "lab-bundled"], default="bundled")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.label):
        parser.error("Invalid output label")
    if args.seconds is not None and not 0 < args.seconds <= 86400:
        parser.error("seconds must be in (0, 86400]")
    if args.samples is not None and args.samples < 10:
        parser.error("samples must be at least 10")
    source = ROOT / "vendor/harfrust-upstream"
    if not (source / "harfrust/benches/shaping.rs").is_file():
        parser.error("Run python3 lab.py fetch first")
    result = ROOT / "results" / args.label
    if result.exists():
        parser.error(f"Output already exists: {result}")
    lock = ROOT / "upstream.lock"
    shutil.copyfile(lock, source / "Cargo.lock")
    env = os.environ.copy()
    env.update(CARGO_TARGET_DIR=str(ROOT / "target/upstream"),
               CRITERION_HOME=str(result / "criterion"),
               HARFBUZZ_SYS_NO_PKG_CONFIG="1", CC_ENABLE_DEBUG_OUTPUT="1")
    # 通常実行では追加フラグなし
    env["CXXFLAGS"] = args.cxxflags
    lab_metadata = None
    if args.harfbuzz != "bundled":
        lab_env = os.environ.copy()
        lab_env["CXXFLAGS"] = args.cxxflags
        lab_env["CARGO_TARGET_DIR"] = str(ROOT / "target")
        lab_env["SHAPING_HARFBUZZ_SOURCE"] = str(ROOT / "vendor/harfbuzz")
        if args.harfbuzz == "lab-bundled":
            deps = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=source, text=True))
            wrapper = next(p for p in deps["packages"] if p["name"] == "harfbuzz_rs")
            lab_env["SHAPING_HARFBUZZ_SOURCE"] = str(Path(wrapper["manifest_path"]).parent / "harfbuzz")
        build = subprocess.run(["cargo", "build", "--release", "--locked", "--message-format=json"],
                               cwd=ROOT, env=lab_env, check=True, stdout=subprocess.PIPE, text=True)
        messages = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
        native_build = next(m for m in messages if m["reason"] == "build-script-executed" and "shaping-lab" in m["package_id"])
        archive = Path(native_build["out_dir"]) / "libshaping_harfbuzz.a"
        link_directory = ROOT / "target/upstream-native" / digest(archive)
        link_directory.mkdir(parents=True, exist_ok=True)
        linked_archive = link_directory / archive.name
        shutil.copyfile(archive, linked_archive)
        bridge = link_directory / "pkg-config"
        shutil.copyfile(ROOT / "lab.py", bridge)
        bridge.chmod(0o755)
        lab_metadata = json.loads(subprocess.check_output(
            [str(ROOT / "target/release/shape-lab"), "doctor"], cwd=ROOT, text=True))
        env.pop("HARFBUZZ_SYS_NO_PKG_CONFIG", None)
        env["PKG_CONFIG"] = str(bridge)
        env["SHAPING_PC_ARCHIVE"] = str(linked_archive)
        env["SHAPING_PC_INCLUDE"] = str(ROOT / lab_metadata["build"]["harfbuzz_include"])
        env["SHAPING_PC_VERSION"] = lab_metadata["backends"]["harfbuzz"]
        lab_metadata["archive_sha256"] = digest(archive)
        lab_metadata["link_archive"] = str(linked_archive)
    command = ["cargo", "bench", "--locked", "-p", "harfrust", "--bench", "shaping", "--"]
    if args.filter:
        command.append(args.filter)
    if args.seconds is not None:
        command.extend(["--measurement-time", str(args.seconds)])
    if args.samples is not None:
        command.extend(["--sample-size", str(args.samples)])
    result.mkdir(parents=True)
    metadata = subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1"],
                              cwd=source, env=env, check=True, capture_output=True, text=True)
    (result / "dependencies.json").write_text(metadata.stdout)
    dependencies = json.loads(metadata.stdout)
    wrapper = next(p for p in dependencies["packages"] if p["name"] == "harfbuzz_rs")
    native = Path(wrapper["manifest_path"]).parent / "harfbuzz"
    inputs = [source / "Cargo.toml", source / "harfrust/Cargo.toml", source / "harfrust/benches/shaping.rs"]
    inputs.extend(p for p in (source / "harfrust/benches").rglob("*") if p.is_file())
    inputs.extend((source / "harfrust/src").rglob("*.rs"))
    build_env = {k:v for k,v in env.items() if k.startswith(("CARGO_PROFILE_", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CXXFLAGS", "CFLAGS")) or k in ("CC", "CXX", "AR", "HARFBUZZ_SYS_NO_PKG_CONFIG")}
    record = {
        "upstream_commit": json.loads((ROOT / "assets.lock.json").read_text())["harfrust_upstream_commit"],
        "cargo_lock_sha256": digest(lock), "command": command, "arguments": vars(args),
        "runner_sha256": digest(Path(__file__)), "link_bridge_sha256": digest(ROOT / "lab.py"),
        "inputs": {str(p.relative_to(source)):digest(p) for p in sorted(set(inputs))},
        "harfbuzz_rs": wrapper["source"], "harfbuzz_commit": output(["git", "rev-parse", "HEAD"], native),
        "harfbuzz_version_header": (native / "src/hb-version.h").read_text(),
        "rustc": output(["rustc", "-Vv"]), "cxx": output([env.get("CXX", "c++"), "--version"]),
        "host": output(["uname", "-a"]), "build_env": build_env,
        "sampling": {"mode":"flat", "warmup_seconds":0.1, "measurement_seconds":args.seconds or 0.5, "samples":args.samples or 10},
        "status":"running",
        "local_harfbuzz": lab_metadata,
    }
    path = result / "metadata.json"
    path.write_text(json.dumps(record, indent=2) + "\n")
    with (result / "console.txt").open("w") as log:
        process = subprocess.Popen(command, cwd=source / "harfrust", env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in process.stdout:
            log.write(line)
            print(line, end="", flush=True)
        code = process.wait()
    build_dir = Path(env["CARGO_TARGET_DIR"]) / "release/build"
    record["native_builds"] = []
    for archive in ([] if args.harfbuzz != "bundled" else build_dir.glob("harfbuzz_rs-*/out/libembedded_harfbuzz.a")):
        log_path = archive.parent.parent / "output"
        native_log = log_path.read_text() if log_path.exists() else ""
        symbols = output(["nm", "-u", str(archive)])
        record["native_builds"].append({"archive":str(archive), "sha256":digest(archive),
            "assertion_symbol": None if symbols is None else bool(re.search(r"__assert_(?:rtn|fail)", symbols)),
            "build_output":native_log})
    record.update(status="complete" if code == 0 else "failed", exit_code=code)
    path.write_text(json.dumps(record, indent=2) + "\n")
    if code == 0:
        subprocess.run([sys.executable, str(ROOT / "lab.py"), "summarize", str(result)], check=True)
    sys.exit(code)


def pkg_config_main():
    args = sys.argv[1:]
    if not args or args[-1] != "harfbuzz":
        sys.exit("Only the local harfbuzz package is supported")
    if args[:-1] == ["--modversion"]:
        print(os.environ["SHAPING_PC_VERSION"])
    elif set(args[:-1]) <= {"--libs", "--cflags", "--static"}:
        flags = []
        if "--libs" in args:
            flags.extend(["-L" + str(Path(os.environ["SHAPING_PC_ARCHIVE"]).parent),
                          "-lshaping_harfbuzz", "-lc++" if sys.platform == "darwin" else "-lstdc++"])
        if "--cflags" in args:
            flags.append("-I" + os.environ["SHAPING_PC_INCLUDE"])
        print(" ".join(shlex.quote(flag) for flag in flags))
    else:
        sys.exit("Unsupported pkg-config arguments: " + " ".join(args))


if __name__ == "__main__":
    if Path(__file__).name == "pkg-config":
        pkg_config_main()
    else:
        commands = {
            "fetch": fetch_main, "bench": bench_main, "profile": profile_main,
            "upstream": upstream_main, "summarize": summarize_main,
            "sample-summary": sample_summary_main, "diff": diff_shapes_main,
        }
        parser = argparse.ArgumentParser(description="Shaping experiments")
        parser.add_argument("command", choices=commands)
        command = parser.parse_args(sys.argv[1:2]).command
        del sys.argv[1]
        if command == "fetch":
            argparse.ArgumentParser(description="Fetch pinned sources and fonts").parse_args()
        commands[command]()
