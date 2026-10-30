import glob
import json
import os
import sys

out, dest = sys.argv[1], sys.argv[2]
projects = [l.strip() for l in open(os.path.join(os.path.dirname(__file__), "projects.txt")) if l.strip()]
metrics = ["flip_mean", "flip_p999", "frac_over_8", "frac_over_32"]
workers = range(1, 11)


def worst(path):
    data = json.load(open(path))
    return {m: {"value": data["worst"][m]["value"], "frame": data["worst"][m]["frame"]} for m in metrics}


def spread(base, suffix):
    per = {}
    for w in workers:
        path = f"{base}/w{w}{suffix}"
        if os.path.exists(path):
            per[w] = worst(path)
    return per


def bar_of(per):
    bar = {}
    for m in metrics:
        w = max(per, key=lambda k: per[k][m]["value"])
        bar[m] = {**per[w][m], "workers": w}
    return bar


def primary_suffix(project):
    return ".raw.json" if project == "hf-hyperframes-launch" else ".json"


result = {
    "metrics": metrics,
    "scoring": "vrf-bench score --frames every:5 against bench/refs/<id>/frames; synth-*-hf REF frames are RGBA with alpha 0 where only the html/body background shows, so they are composited over that background first (composite.sh); hf-hyperframes-launch is scored raw (rule as stated) with the composited score kept beside it",
    "projects": {},
    "ours": {},
}
for project in projects:
    base = f"{out}/bar/{project}"
    per = spread(base, primary_suffix(project))
    if not per:
        result["projects"][project] = {"workers_measured": [], "workers_missing": list(workers)}
        continue
    entry = {
        "workers_measured": sorted(per),
        "workers_missing": [w for w in workers if w not in per],
        "bar": bar_of(per),
        "per_worker": per,
    }
    if project == "hf-hyperframes-launch":
        composited = spread(base, ".json")
        if composited:
            entry["composited_over_body_background"] = {
                "workers_measured": sorted(composited),
                "bar": bar_of(composited),
                "per_worker": composited,
            }
    result["projects"][project] = entry

for mode in ("reference", "fast"):
    for path in sorted(glob.glob(f"{out}/ours/{mode}/*.json")):
        name = os.path.basename(path)
        if name.endswith(".raw.json") or name.endswith(".report.json"):
            continue
        project = name[:-5]
        raw = path[:-5] + ".raw.json"
        record = {"worst": worst(raw if os.path.exists(raw) else path)}
        if os.path.exists(raw):
            record["composited_over_body_background"] = worst(path)
        report = json.load(open(path[:-5] + ".report.json"))
        record["views"] = report.get("views")
        record["run"] = report.get("run")
        entry = result["projects"].get(project, {})
        bar = entry.get("bar")
        if bar:
            record["against_bar"] = {m: record["worst"][m]["value"] / bar[m]["value"] if bar[m]["value"] else None for m in metrics}
            record["within_bar"] = all(record["worst"][m]["value"] <= bar[m]["value"] for m in metrics)
            record["bar_complete"] = len(entry["workers_measured"]) == len(list(workers))
        result["ours"].setdefault(mode, {})[project] = record

json.dump(result, open(dest, "w"), indent=1)
