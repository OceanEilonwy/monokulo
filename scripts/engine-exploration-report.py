#!/usr/bin/env python3
"""Record exploration inputs/tool versions and summarize libFuzzer evidence.

Coverage counts are libFuzzer instrumentation counters, not source line coverage.
Semantic counts are asserted scenario observations, not branch coverage.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time


def command(*args):
    result = subprocess.run(args, text=True, capture_output=True, check=True)
    return result.stdout.strip()


def corpus(path):
    files = [p for p in Path(path).glob('*') if p.is_file()]
    return {"files": len(files), "bytes": sum(p.stat().st_size for p in files),
            "sha256": sorted({hashlib.sha256(p.read_bytes()).hexdigest() for p in files})}


def summarize(log):
    samples = []
    for line in log.splitlines():
        match = re.search(r'#(\d+)\s+(INITED|NEW|REDUCE|DONE).*?cov:\s*(\d+)\s+ft:\s*(\d+)', line)
        if match:
            samples.append({"executions": int(match[1]), "phase": match[2],
                            "coverage": int(match[3]), "features": int(match[4])})
    stats = {name: int(value) for name, value in re.findall(r'stat::([a-z_]+):\s*(\d+)', log)}
    initial = next((s for s in samples if s['phase'] == 'INITED'), None)
    final = samples[-1] if samples else None
    explored = final["executions"] - initial["executions"] if initial and final else None
    return {"executions_after_initialization": explored, "initial": initial, "final": final, "stats": stats,
            "coverage_growth": final['coverage'] - initial['coverage'] if initial and final else None,
            "feature_growth": final['features'] - initial['features'] if initial and final else None}


def semantics(output):
    counts, cases = {}, 0
    for path in output.glob('semantics.*.jsonl'):
        for line in path.read_text().splitlines():
            record = json.loads(line); cases += 1
            for name, count in record.items():
                if name.startswith('command:'): name = re.split(r'[({ ]', name)[0]
                counts[name] = counts.get(name, 0) + count
    return cases, counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='operation', required=True)
    for op in ['begin', 'finish', 'calibrate', 'properties']:
        p = sub.add_parser(op)
        p.add_argument('--output', type=Path, required=True)
        if op in ['begin','finish']: p.add_argument('--corpus', type=Path, required=True)
        if op == 'begin':
            p.add_argument('--target', required=True); p.add_argument('--features', default='default')
            for name in ['seconds', 'timeout', 'max-len', 'seed']:
                p.add_argument('--' + name, type=int, required=True)
        elif op == 'finish':
            p.add_argument('--exit-code', type=int, required=True)
        elif op == 'calibrate':
            p.add_argument('--binary', type=Path, required=True)
            p.add_argument('--seeds', type=Path, required=True)
            p.add_argument('--timeout', type=int, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.operation == 'begin':
        settings = {k: v for k, v in vars(args).items() if k not in ['output', 'corpus']}
        data = {"schema": 1, "settings": settings, "revision": command('git', 'rev-parse', 'HEAD'),
                "dirty": bool(command('git', 'status', '--porcelain')), "platform": platform.platform(),
                "rustc": command('rustc', '-Vv'), "cargo": command('cargo', '-V'),
                "cargo_fuzz": command('cargo', 'fuzz', '--version'),
                "toolchain": os.environ.get('RUSTUP_TOOLCHAIN', command('rustup', 'show', 'active-toolchain')),
                "lock_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [Path("Cargo.lock"), Path("fuzz/Cargo.lock")]},
                "started": time.time(), "corpus": corpus(args.corpus)}
        (args.output / 'replay.json').write_text(json.dumps(data, indent=2) + '\n')
    elif args.operation == 'calibrate':
        results = []
        for seed in sorted(args.seeds.glob('*')):
            if not seed.is_file(): continue
            started = time.monotonic()
            try:
                result = subprocess.run([str(args.binary.resolve()), '-runs=1', '-seed=1',
                                         f'-timeout={args.timeout}', str(seed.resolve())],
                                        text=True, capture_output=True, timeout=args.timeout + 10)
                code, log = result.returncode, result.stdout + result.stderr
            except subprocess.TimeoutExpired:
                code, log = 124, 'Seed exceeded calibration watchdog\n'
            results.append({"seed": seed.name, "seconds": time.monotonic() - started, "exit_code": code})
            (args.output / f'calibration-{seed.name}.log').write_text(log)
        durations = [r['seconds'] for r in results]
        data = {"binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(), "seeds": results, "median_seconds": statistics.median(durations) if durations else None,
                "max_seconds": max(durations) if durations else None, "timeout_seconds": args.timeout}
        (args.output / 'calibration.json').write_text(json.dumps(data, indent=2) + '\n')
        if any(r['exit_code'] for r in results): raise SystemExit('seed calibration failed; inspect logs')
    elif args.operation == 'properties':
        cases, counts = semantics(args.output)
        data = {"schema": 1, "revision": command('git','rev-parse','HEAD'),
                "rustc": command('rustc','-Vv'), "cargo": command('cargo','-V'),
                "nextest": command('cargo','nextest','--version'),
                "settings": {name: os.environ.get(name) for name in ['PROPTEST_CASES','ENGINE_PROOF_CASES','PROPTEST_RNG_SEED','ENGINE_FEATURES']},
                "semantic_cases": cases, "semantic_observations": counts}
        (args.output / 'report.json').write_text(json.dumps(data, indent=2) + '\n')
    else:
        before = json.loads((args.output / 'replay.json').read_text())
        after = corpus(args.corpus)
        cases, counts = semantics(args.output)
        data = {"schema": 1, "exit_code": args.exit_code, "wall_seconds": time.time() - before['started'],
                "corpus": after, "new_unique_inputs": len(set(after['sha256']) - set(before['corpus']['sha256'])),
                "exploration": summarize((args.output / 'fuzzer.log').read_text()),
                "semantic_cases": cases, "semantic_observations": counts}
        (args.output / 'report.json').write_text(json.dumps(data, indent=2) + '\n')
        print(json.dumps(data, indent=2))
        if args.exit_code == 0 and data['exploration']['executions_after_initialization'] == 0:
            raise SystemExit('No inputs explored after seed initialization; increase the campaign budget.')


if __name__ == '__main__': main()
