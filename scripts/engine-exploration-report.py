#!/usr/bin/env python3
"""Record replay settings and measured exploration, never inferred line coverage."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time

TERMINAL_STATES = {'passed', 'failed', 'invalid-evidence', 'insufficient-exploration'}


def command(*args):
    return subprocess.run(args, text=True, capture_output=True, check=True).stdout.strip()


def write_json(path, data):
    pending = path.with_suffix(path.suffix + '.tmp')
    pending.write_text(json.dumps(data, indent=2) + '\n')
    pending.replace(path)


def seed_files(directory):
    if not directory.is_dir():
        raise ValueError(f'Reviewed seed directory is missing: {directory}')
    files = sorted(p for p in directory.iterdir() if p.is_file())
    if not files:
        raise ValueError(f'Reviewed seed directory is empty: {directory}')
    return files


def sync_seeds(seeds, destination):
    files = seed_files(seeds)
    destination.mkdir(parents=True, exist_ok=True)
    for seed in files:
        digest = hashlib.sha256(seed.read_bytes()).hexdigest()
        # A reviewed input is identified by content, not its possibly cached name.
        # Refresh the copy even if a prior campaign modified its contents.
        shutil.copyfile(seed, destination / f'reviewed-{digest}')


def corpus(path):
    files = [p for p in path.glob('*') if p.is_file()]
    return {
        'files': len(files),
        'bytes': sum(p.stat().st_size for p in files),
        'sha256': sorted({hashlib.sha256(p.read_bytes()).hexdigest() for p in files}),
    }


def summarize(log):
    samples = []
    for line in log.splitlines():
        match = re.search(r'#(\d+)\s+(INITED|NEW|REDUCE|DONE).*?cov:\s*(\d+)\s+ft:\s*(\d+)', line)
        if match:
            samples.append({
                'executions': int(match[1]), 'phase': match[2],
                'coverage': int(match[3]), 'features': int(match[4]),
            })
    initial = next((s for s in samples if s['phase'] == 'INITED'), None)
    final = samples[-1] if samples else None
    return {
        'initial': initial,
        'final': final,
        'stats': {name: int(value) for name, value in re.findall(r'stat::([a-z_]+):\s*(\d+)', log)},
        'executions_after_initialization': final['executions'] - initial['executions'] if initial and final else None,
        'coverage_growth': final['coverage'] - initial['coverage'] if initial and final else None,
        'feature_growth': final['features'] - initial['features'] if initial and final else None,
    }


def evidence_status(exploration, exit_code):
    if exit_code:
        return 'failed', 'Fuzzer exited unsuccessfully; inspect the raw log.'
    initial, final = exploration['initial'], exploration['final']
    delta = exploration['executions_after_initialization']
    if initial is None or final is None or final['phase'] != 'DONE' or delta is None or delta < 0:
        return 'invalid-evidence', 'Missing, malformed or inconsistent completed exploration evidence.'
    if delta == 0:
        return 'insufficient-exploration', 'No inputs explored after initialization; increase the budget.'
    return 'passed', None


def semantics(output):
    counts, cases = {}, 0
    for path in output.glob('semantics.*.jsonl'):
        for line in path.read_text().splitlines():
            cases += 1
            for name, count in json.loads(line).items():
                if name.startswith(('command:', 'selected-command:')):
                    name = re.split(r'[({ ]', name)[0]
                counts[name] = counts.get(name, 0) + count
    return cases, counts


def calibrate(args):
    try:
        seeds = seed_files(args.seeds)
    except ValueError as error:
        write_json(args.output / 'calibration.json', {'status': 'failed', 'reason': str(error), 'seeds': []})
        raise SystemExit(str(error)) from error
    results = []
    for seed in seeds:
        started = time.monotonic()
        try:
            result = subprocess.run(
                [str(args.binary.resolve()), '-runs=1', '-seed=1', f'-timeout={args.timeout}', str(seed.resolve())],
                text=True, capture_output=True, timeout=args.timeout + 10,
            )
            code, log = result.returncode, result.stdout + result.stderr
        except subprocess.TimeoutExpired:
            code, log = 124, 'Seed exceeded calibration watchdog\n'
        results.append({'seed': seed.name, 'seconds': time.monotonic() - started, 'exit_code': code})
        (args.output / f'calibration-{seed.name}.log').write_text(log)
    failed = any(r['exit_code'] for r in results)
    durations = [r['seconds'] for r in results]
    write_json(args.output / 'calibration.json', {
        'status': 'failed' if failed else 'passed',
        'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        'seeds': results,
        'median_seconds': statistics.median(durations),
        'max_seconds': max(durations),
        'timeout_seconds': args.timeout,
    })
    if failed:
        raise SystemExit('Seed calibration failed; inspect logs.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='operation', required=True)
    for op in ['begin', 'finish', 'calibrate', 'properties', 'incomplete', 'sync-seeds']:
        p = sub.add_parser(op)
        if op != 'sync-seeds':
            p.add_argument('--output', type=Path, required=True)
        if op in ['begin', 'finish', 'sync-seeds']:
            p.add_argument('--corpus', type=Path, required=True)
        if op in ['begin', 'calibrate', 'sync-seeds']:
            p.add_argument('--seeds', type=Path, required=True)
        if op == 'begin':
            p.add_argument('--target', required=True)
            p.add_argument('--features', default='default')
            for name in ['seconds', 'timeout', 'max-len', 'seed']:
                p.add_argument('--' + name, type=int, required=True)
        elif op in ['finish', 'incomplete']:
            p.add_argument('--exit-code', type=int, required=True)
            if op == 'incomplete':
                p.add_argument('--stage', required=True)
        elif op == 'calibrate':
            p.add_argument('--binary', type=Path, required=True)
            p.add_argument('--timeout', type=int, required=True)
    args = parser.parse_args()
    if args.operation == 'sync-seeds':
        sync_seeds(args.seeds, args.corpus)
        return
    args.output.mkdir(parents=True, exist_ok=True)
    if args.operation == 'begin':
        if any(args.output.iterdir()):
            raise SystemExit('Refusing to reuse a nonempty campaign directory.')
        write_json(args.output / 'report.json', {'schema': 2, 'status': 'running'})
        settings = {k: v for k, v in vars(args).items() if k not in ['output', 'corpus', 'seeds']}
        write_json(args.output / 'replay.json', {
            'schema': 2, 'settings': settings,
            'revision': command('git', 'rev-parse', 'HEAD'),
            'dirty': bool(command('git', 'status', '--porcelain')),
            'platform': platform.platform(), 'rustc': command('rustc', '-Vv'),
            'cargo': command('cargo', '-V'), 'cargo_fuzz': command('cargo', 'fuzz', '--version'),
            'toolchain': os.environ.get('RUSTUP_TOOLCHAIN') or command('rustup', 'show', 'active-toolchain'),
            'sanitizer_environment': {name: os.environ.get(name) for name in ['ASAN_OPTIONS', 'LSAN_OPTIONS', 'UBSAN_OPTIONS']},
            'lock_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [Path('Cargo.lock'), Path('fuzz/Cargo.lock')]},
            'reviewed_seeds': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in seed_files(args.seeds)},
            'started': time.time(), 'corpus': corpus(args.corpus),
        })
    elif args.operation == 'calibrate':
        calibrate(args)
    elif args.operation == 'properties':
        cases, counts = semantics(args.output)
        write_json(args.output / 'report.json', {
            'schema': 2, 'revision': command('git', 'rev-parse', 'HEAD'),
            'rustc': command('rustc', '-Vv'), 'cargo': command('cargo', '-V'),
            'nextest': command('cargo', 'nextest', '--version'),
            'settings': {name: os.environ.get(name) for name in ['PROPTEST_CASES', 'ENGINE_PROOF_CASES', 'PROPTEST_RNG_SEED', 'ENGINE_FEATURES']},
            'semantic_cases': cases, 'semantic_observations': counts,
        })
    elif args.operation == 'incomplete':
        path = args.output / 'report.json'
        current = json.loads(path.read_text()) if path.exists() else {}
        if current.get('status') not in TERMINAL_STATES:
            write_json(path, {'schema': 2, 'status': 'failed', 'stage': args.stage,
                              'exit_code': args.exit_code, 'reason': 'Campaign stopped before completed exploration reporting.'})
    else:
        before = json.loads((args.output / 'replay.json').read_text())
        after = corpus(args.corpus)
        cases, counts = semantics(args.output)
        exploration = summarize((args.output / 'fuzzer.log').read_text())
        status, reason = evidence_status(exploration, args.exit_code)
        data = {
            'schema': 2, 'status': status, 'reason': reason, 'exit_code': args.exit_code,
            'wall_seconds': time.time() - before['started'], 'corpus': after,
            'new_unique_inputs': len(set(after['sha256']) - set(before['corpus']['sha256'])),
            'exploration': exploration, 'semantic_cases': cases, 'semantic_observations': counts,
        }
        write_json(args.output / 'report.json', data)
        print(json.dumps(data, indent=2))
        if status != 'passed':
            raise SystemExit(args.exit_code or reason)


if __name__ == '__main__':
    main()
