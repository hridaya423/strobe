import collections
import sys
import xml.etree.ElementTree as ET

names = {}
weights = {}
rows = []
for _, el in ET.iterparse(sys.argv[1]):
    if el.tag == "frame":
        if "id" in el.attrib:
            names[el.attrib["id"]] = el.attrib.get("name", "?")
    elif el.tag == "row":
        stack = []
        for frame in el.iter("frame"):
            key = frame.attrib.get("id") or frame.attrib.get("ref")
            stack.append(names.get(key, "?"))
        weight = el.find("weight")
        if weight is not None and weight.text:
            weights[weight.attrib["id"]] = int(weight.text)
        value = weights.get(weight.attrib.get("id") or weight.attrib.get("ref"), 1000000) if weight is not None else 1000000
        rows.append((stack, value))

callback_marks = ("on_accelerated_paint",)
forbidden = ("memcpy", "memmove", "bzero", "IOSurfaceLock", "CVPixelBufferLockBaseAddress", "map_async", "copy_texture_to_buffer", "get_mapped_range")
in_callback = [(s, w) for s, w in rows if any(m in f for f in s for m in callback_marks)]
total_callback = sum(w for _, w in in_callback)
print(f"samples (all threads) {len(rows)}, samples with the paint callback on the stack: {len(in_callback)} ({total_callback / 1e6:.1f} ms of CPU)")
hits = collections.Counter()
for stack, weight in in_callback:
    for frame in stack:
        for word in forbidden:
            if word in frame:
                hits[(word, frame)] += weight
print("forbidden CPU pixel-copy frames under the callback:", dict(hits) if hits else "none")
leaf = collections.Counter()
for stack, weight in in_callback:
    leaf[stack[0] if stack else "?"] += weight
print("top leaf functions under the callback (ms of CPU):")
for name, weight in leaf.most_common(12):
    print(f"  {weight / 1e6:8.2f}  {name[:110]}")

for stack, weight in in_callback:
    if any(word in f for f in stack for word in forbidden):
        print("--- sample with a copy-like frame, innermost first:")
        for frame in stack[:12]:
            print("   ", frame[:120])
