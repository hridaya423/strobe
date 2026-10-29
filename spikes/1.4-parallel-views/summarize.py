import json, re, sys

path = sys.argv[1]
rows = []
header = None
for line in open(path):
    line = line.strip()
    if line.startswith('#'):
        header = dict(part.split('=') for part in line[1:].split() if '=' in part)
    elif line.startswith('{'):
        data = json.loads(line)
        rows.append((header, data))

print('| page | shape | N | fps total | fps per view | seek ms | begin frame to paint ms | convert ms | browser cores | gpu cores | renderer cores | load (1 min) |')
print('|---|---|---|---|---|---|---|---|---|---|---|---|')
for header, d in rows:
    n = d['views']
    cores = d['cores']
    shape = 'one process' if header['mode'] == 'inproc' else 'N processes'
    print('| {page} | {shape} | {n} | {fps:.0f} | {per:.0f} | {seek:.2f} | {b2p:.2f} | {conv:.2f} | {br:.2f} | {gpu:.2f} | {ren:.2f} | {load} |'.format(
        page=header['page'], shape=shape, n=n, fps=d['fps'], per=d['fps'] / n,
        seek=d['seek_ms'], b2p=d['begin_to_paint_ms'], conv=d['convert_ms'],
        br=cores.get('browser', 0), gpu=cores.get('gpu', 0), ren=cores.get('renderer', 0),
        load=header['load'].split()[0]))
