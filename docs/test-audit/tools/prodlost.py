"""Lines lost in production code only (drop lines inside #[cfg(test)] modules and test files)."""
import json, sys, os
out = json.load(open(sys.argv[1]))
run = out['log'].replace('.log', '.json')
base = json.load(open(sys.argv[2]))

def lines(j):
    res = {}
    for d in j['data']:
        for f in d['files']:
            s = set()
            for seg in f['segments']:
                if seg[2] > 0 and seg[3]:
                    s.add(seg[0])
            res[f['filename']] = s
    return res

a, w = lines(base), lines(json.load(open(run)))
total = 0; per = {}
for fn, covered in a.items():
    if '/tests/' in fn or fn.endswith('theme_tests.rs') or fn.endswith('tests.rs') or not os.path.exists(fn):
        continue
    src = open(fn).read().split('\n')
    cut = next((i + 1 for i, l in enumerate(src) if l.strip() == '#[cfg(test)]'), 10 ** 9)
    lost = [l for l in covered - w.get(fn, set()) if l < cut]
    if lost:
        per[fn.split('/crates/')[-1]] = len(lost); total += len(lost)
print(json.dumps({'reported_lines_lost': out['lines_lost'], 'reported_pp': out['pct_points_lost'],
                  'prod_lines_lost': total, 'prod_pp': round(100 * total / out['baseline_lines'], 2),
                  'failed': out['failed'], 'per_file': dict(sorted(per.items(), key=lambda x: -x[1]))}, indent=1))
