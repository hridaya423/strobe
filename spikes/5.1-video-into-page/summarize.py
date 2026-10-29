import re, sys, glob, os

LOGS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../docs/spikes/raw/5.1")


def parse(path):
    text = open(path).read()
    row = {"tag": os.path.basename(path)[:-4]}
    m = re.search(r"^frames (\d+) \(timed (\d+)\), id mismatches (\d+), worst patch error ([\d.]+)", text, re.M)
    if not m:
        return None
    row["frames"], row["timed"], row["mismatch"], row["patch"] = int(m[1]), int(m[2]), int(m[3]), float(m[4])
    for key in ("prep", "seek", "paint", "post", "total", "host cpu"):
        m = re.search(rf"^  {key}\s+mean\s+([\d.]+)\s+p50\s+([\d.]+)\s+p99\s+([\d.]+)\s+max\s+([\d.]+)", text, re.M)
        row[key] = tuple(float(v) for v in m.groups())
    m = re.search(r"child cpu\s+([\d.]+)", text)
    row["child"] = float(m[1])
    m = re.search(r"full-frame vs ffmpeg decode over (\d+) frames: mean abs error r ([\d.]+) g ([\d.]+) b ([\d.]+), max (\d+), pixels over 2: ([\d.]+)%, over 4: ([\d.]+)%, over 8: ([\d.]+)%", text)
    row["full"] = tuple(m.groups()) if m else None
    m = re.search(r"^uptime: .*load averages?: (.*)$", text, re.M)
    row["load"] = m[1] if m else "?"
    m = re.search(r"^decode: (\d+) frames in ([\d.]+) s = ([\d.]+) fps", text, re.M)
    row["decode"] = float(m[3]) if m else None
    return row


def main():
    rows = [r for r in (parse(p) for p in sorted(glob.glob(LOGS + "/*.txt"))) if r]
    prefixes = sys.argv[1:] or [""]
    for prefix in prefixes:
        print(f"\n### {prefix or 'all'}\n")
        print("| run | frames | id mismatches | worst patch err | prep | seek | paint | post | total mean | total p99 | host cpu | child cpu | full-frame mean abs err (r,g,b), max, >2 / >8 | load |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for r in rows:
            if not r["tag"].startswith(prefix):
                continue
            full = "-"
            if r["full"]:
                _, er, eg, eb, mx, o2, o4, o8 = r["full"]
                full = f"{er}, {eg}, {eb}; {mx}; {o2}% / {o8}%"
            print(f"| {r['tag']} | {r['frames']} | {r['mismatch']} | {r['patch']:.1f} | {r['prep'][0]:.2f} | {r['seek'][0]:.2f} | {r['paint'][0]:.2f} | {r['post'][0]:.2f} | {r['total'][0]:.2f} | {r['total'][2]:.2f} | {r['host cpu'][0]:.2f} | {r['child']:.2f} | {full} | {r['load']} |")


main()
