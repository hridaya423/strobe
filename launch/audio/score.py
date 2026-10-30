"""Strobe launch score: 150 BPM, beat = 0.4 s, 16ths = 0.1 s, key of E. Synths and drums come from the Lexis raw pipeline
(Surge XT via pedalboard, VSCO percussion); run with that folder as the working directory:
cd ~/honey/React/smartterminal/lexis-motion-design-vid/raw && python3 <repo>/launch/audio/score.py
"""
import os, re, subprocess, sys
import numpy as np
import pedalboard as pb

sys.path.insert(0, os.getcwd())
import synth as S
from surge import make, render, SR
from sampler import perc

HERE = os.path.dirname(os.path.abspath(__file__))
DUR = 12.0
N = int(SR * DUR)
B = lambda b: b * 0.4
rng = np.random.default_rng(5)


def fit(y):
    y = y[:N]
    return np.pad(y, ((0, N - len(y)), (0, 0)))


def fx(y, *plugs):
    return pb.Pedalboard(list(plugs))(y.T.astype(np.float32), SR).T.astype(np.float64)


def sweep(y, pts, res=0.25):
    lf = pb.LadderFilter(mode=pb.LadderFilter.Mode.LPF24, cutoff_hz=1000, resonance=res, drive=1.0)
    out = np.zeros_like(y)
    xs, ys = zip(*pts)
    for i in range(0, len(y), 512):
        lf.cutoff_hz = float(np.exp(np.interp(i / SR, xs, np.log(ys))))
        out[i:i + 512] = lf.process(y[i:i + 512].T.astype(np.float32), SR, reset=False).T
    return out


CH = {"Em": (40, [64, 67, 71, 76], 52), "C": (36, [64, 67, 72, 76], 48), "D": (38, [62, 66, 69, 74], 50),
      "E": (40, [64, 68, 71, 76], 52), "C#m": (37, [64, 68, 73, 76], 49), "A": (33, [64, 69, 73, 76], 45),
      "B": (35, [63, 66, 71, 75], 47), "Emaj9": (28, [64, 68, 71, 75, 78], 52)}
PROG = [(0, 4, "Em"), (4, 6, "C"), (6, 8, "D"), (8, 10, "E"), (10, 12, "C#m"), (12, 14, "A"), (14, 15, "B"),
        (15, 17, "C#m"), (17, 18, "B"), (18, 20, "E"), (20, 21, "C#m"), (21, 22, "B")]
LAND = B(26)

pulse_p = make(a_osc_1_type="Classic", a_osc_1_unison_voices=5, a_osc_1_unison_detune=16, a_osc_2_mute=False, a_osc_2_type="Classic",
               a_osc_2_octave=-1, a_osc_2_volume=-9, a_osc_2_width_1=30,
               a_filter_1_type="LP 24 dB", a_filter_1_cutoff=700.0, a_filter_1_resonance=22.0, a_filter_1_feg_mod_amount=40,
               a_filter_eg_attack=0.0, a_filter_eg_decay=0.12, a_filter_eg_sustain=8.0, a_filter_eg_release=0.06,
               a_amp_eg_attack=0.0, a_amp_eg_decay=0.18, a_amp_eg_sustain=30.0, a_amp_eg_release=0.07)
pad_p = make(a_osc_1_type="Classic", a_osc_1_unison_voices=7, a_osc_1_unison_detune=24, a_osc_2_mute=False, a_osc_2_type="Classic",
             a_osc_2_octave=1, a_osc_2_unison_voices=5, a_osc_2_unison_detune=18, a_osc_2_volume=-8,
             a_filter_1_type="LP 12 dB", a_filter_1_cutoff=2600.0, a_filter_1_resonance=8.0,
             a_amp_eg_attack=0.12, a_amp_eg_decay=1.0, a_amp_eg_sustain=100.0, a_amp_eg_release=0.5)
braam_p = make(a_osc_1_type="Classic", a_osc_1_unison_voices=9, a_osc_1_unison_detune=32, a_osc_2_mute=False, a_osc_2_type="Classic",
               a_osc_2_octave=-1, a_osc_2_unison_voices=5, a_osc_2_unison_detune=20, a_waveshaper_type="Soft", a_waveshaper_drive=10.0,
               a_filter_1_type="LP 24 dB", a_filter_1_cutoff=420.0, a_filter_1_resonance=30.0, a_filter_1_feg_mod_amount=46,
               a_filter_eg_attack=0.03, a_filter_eg_decay=0.7, a_filter_eg_sustain=20.0, a_filter_eg_release=0.5,
               a_amp_eg_attack=0.004, a_amp_eg_decay=1.0, a_amp_eg_sustain=60.0, a_amp_eg_release=0.7)
sub_p = make(a_osc_1_type="Sine", a_osc_2_mute=False, a_osc_2_type="Classic", a_osc_2_volume=-18, a_filter_1_type="LP 24 dB",
             a_filter_1_cutoff=180.0, a_amp_eg_attack=0.006, a_amp_eg_decay=0.5, a_amp_eg_sustain=70.0, a_amp_eg_release=0.08)
line_p = make(a_osc_1_type="Classic", a_osc_1_unison_voices=7, a_osc_1_unison_detune=14, a_filter_1_type="LP 12 dB", a_filter_1_cutoff=2400.0,
              a_amp_eg_attack=0.15, a_amp_eg_decay=1.0, a_amp_eg_sustain=100.0, a_amp_eg_release=0.4)
pluck_p = make(a_osc_1_type="String", a_filter_1_type="LP 12 dB", a_filter_1_cutoff=5200.0,
               a_amp_eg_attack=0.0, a_amp_eg_decay=0.7, a_amp_eg_sustain=0.0, a_amp_eg_release=0.5)

grid = lambda b0, b1, step=0.25: [b0 + i * step for i in range(int(round((b1 - b0) / step)))]
pulse_n, pad_n, sub_n, braam_n, line_n, pluck_n = ([] for _ in range(6))


def motif(b0, b1, r, vel, up=False, step=0.25):
    for k, b in enumerate(grid(b0, b1, step)):
        m = [r, r, r + 7, r + 12][k % 4] + (12 if up and k % 2 else 0)
        pulse_n.append((B(b), m, step * 0.4 * 0.8, vel(b) if callable(vel) else vel))


for b0, b1, c in PROG:
    root, tones, pr = CH[c]
    if b1 <= 4:
        motif(b0, b1, pr, lambda b: 0.5 + 0.35 * b / 4, up=b0 >= 2)
    elif b0 >= 18:
        motif(b0, b1, pr, 0.95, up=True, step=0.25 if b0 < 21 else 0.125)
    else:
        motif(b0, b1, pr, 0.9 if b0 >= 8 else 0.8, up=b0 >= 8)
    if b0 >= 4:
        pad_n += [(B(b0) - 0.01, m, B(b1 - b0) + 0.05, 0.6) for m in tones]
        sub_n += [(B(b), root + 12, 0.36, 0.95) for b in range(b0, b1)] if b0 >= 8 else [(B(b0), root + 12, B(b1 - b0) - 0.02, 0.9)]
pad_n += [(0.1, m, B(4) - 0.15, 0.4) for m in (52, 59, 64, 67)]
for b, c in ((8, "E"), (12, "A"), (15, "C#m"), (18, "E")):
    root = CH[c][0]
    braam_n += [(B(b), m, 0.8, 0.95) for m in (root, root + 7, root + 12, root + 16 if c != "C#m" else root + 15)]
line_n += [(B(8), 83, B(4) - 0.05, 0.6), (B(12), 81, B(3) - 0.05, 0.65), (B(15), 80, B(3) - 0.05, 0.7), (B(18), 83, B(4) - 0.05, 0.75)]

root, tones, _ = CH["Emaj9"]
pad_n += [(LAND - 0.02, m, DUR - LAND - 0.25, 1.0) for m in tones + [52, 59]]
braam_n += [(LAND, m, DUR - LAND - 0.3, 1.0) for m in (28, 40, 47, 52, 56)]
sub_n.append((LAND, 40, 1.4, 1.0))
line_n += [(LAND, 88, DUR - LAND - 0.3, 0.9), (LAND, 83, DUR - LAND - 0.3, 0.6)]
pluck_n += [(LAND + s * 0.05, m, 1.2, 0.8) for s, m in enumerate((76, 80, 83, 87, 88, 92))]
pluck_n += [(B(23.5) + k * 0.05, 76 + [0, 4, 7, 11, 12, 16, 19, 23, 24, 28, 31][k], 0.3, 0.35 + 0.04 * k) for k in range(11)]

print("rendering synths...", flush=True)
pulse = fit(render(pulse_p, pulse_n, DUR))
pulse = sweep(pulse, [(0, 280), (B(3.9), 2200), (B(4), 600), (B(7.3), 5000), (B(8), 7000), (B(18), 8000), (B(22) - 0.03, 9000),
                      (B(22), 400), (DUR, 400)], res=0.32)
pulse = fx(pulse, pb.Reverb(room_size=0.55, wet_level=0.12, dry_level=0.9))
pad = fx(fit(render(pad_p, pad_n, DUR)), pb.Chorus(rate_hz=0.3, depth=0.3, mix=0.35), pb.Reverb(room_size=0.9, wet_level=0.32, dry_level=0.75))
sub = fit(render(sub_p, sub_n, DUR))
braam = fx(fit(render(braam_p, braam_n, DUR)), pb.Reverb(room_size=0.9, wet_level=0.3, dry_level=0.9))
line = fx(fit(render(line_p, line_n, DUR)), pb.Reverb(room_size=0.95, wet_level=0.4, dry_level=0.8))
pluck = fx(fit(render(pluck_p, pluck_n, DUR)), pb.Delay(delay_seconds=0.3, feedback=0.25, mix=0.15), pb.Reverb(room_size=0.8, wet_level=0.28, dry_level=0.85))
print("synths done", flush=True)

D_ = "VSCO 1 Percussion/drums/"
norm = lambda x: x / (np.abs(x).max() + 1e-9)
GIANT = [norm(x) for x in perc(D_ + "other/ethnic/giant/mallet/*.wav")]
TEN = [norm(x) for x in perc(D_ + "tenor/tenor_lower/tenor_ff*.wav") + perc(D_ + "tenor/tenor_lower/tenor_fff*.wav")]
TENH = [norm(x) for x in perc(D_ + "tenor/tenor_higher/tenorH_ff*.wav")]
SN = [norm(x) for x in perc(D_ + "snare/drum3_marching/snare3_f*.wav")]
SNF = [norm(x) for x in (perc(D_ + "snare/drum3_marching/snare3_fff*.wav") or SN)]
CBD = [norm(x) for x in perc(D_ + "bass/bdrum*_fff*.wav") + perc(D_ + "bass/bdrum_ff*.wav")]
CR = perc("Percussion/cymbal-crash1_ff_rr*.wav")
SCR = perc("Percussion/susCymb1-cresc-Long_v1.wav")[0]
ac, imp, sfx = (S.Bus(N + SR * 4) for _ in range(3))
rr = {"n": 0}


def hit(kit, b, g, pan=0.0, bus=None):
    rr["n"] += 1
    (bus or ac).add(kit[rr["n"] % len(kit)], B(b), g, pan)


def tenors(b0, b1, g, step=0.25):
    for k, b in enumerate(grid(b0, b1, step)):
        acc = [1.0, 0.55, 0.75, 0.55][k % 4] if step == 0.25 else [1.0, 0.7][k % 2]
        hit(TEN if k % 2 == 0 else TENH, b, g * acc, pan=0.25 * (1 if k % 2 else -1))


def roll(b0, b1, g0, g1, step=0.125):
    ks = grid(b0, b1, step)
    for j, b in enumerate(ks):
        hit(SN, b, g0 + (g1 - g0) * j / max(1, len(ks) - 1), pan=0.1)


def boom(t, g=1.0):
    imp.add(S.taiko(1.2), t, 0.8 * g); imp.add(CBD[0], t, 0.8 * g); imp.add(CR[0], t, 0.55 * g); imp.add(S.subdrop(1.4, 110, 34), t, 0.8 * g)


tenors(0, 2, 0.35, step=0.5)
tenors(2, 4, 0.5)
for b in range(4):
    hit(GIANT, b, 0.5 + 0.1 * b)
roll(3, 4, 0.2, 0.6)
boom(B(4), 1.0)
for b in range(5, 8):
    hit(GIANT, b, 0.55)
roll(7, 8, 0.25, 0.9, step=0.0625)
for b0, b1 in ((8, 11), (12, 18)):
    tenors(b0, b1, 0.55)
    for b in range(b0, b1):
        hit(GIANT, b, 0.75)
        if b % 2:
            hit(SNF, b, 0.6)
roll(11, 12, 0.2, 0.7, step=0.0625)
tenors(18, 21, 0.6)
for b in range(18, 22):
    hit(GIANT, b, 0.85); hit(CBD, b, 0.45)
    hit(SNF, b + 0.5, 0.55)
roll(21, 22, 0.35, 1.0, step=0.0625)
for t, g in ((B(8), 1.0), (B(8.75), 0.6), (B(12), 0.9), (B(15), 0.7), (B(18), 0.9), (LAND, 1.15)):
    boom(t, g)

sx_ = lambda n: S.load(os.path.join(HERE, "sfx", n + ".wav"))
sfx.add(S.noise_riser(B(4) - 0.3, 0.6, 2.6), 0.3, 0.3)
sfx.add(sx_("shatter"), B(4) - 0.02, 0.6)
sfx.add(sx_("cut_dark"), B(2), 0.8)


def thunder():
    n = int(2.6 * SR)
    tt_ = (np.arange(n) / SR)[:, None]
    nz = lambda: rng.standard_normal((n, 2))
    crack = np.zeros((n, 2))
    for t0, g in ((-0.1, 0.12), (-0.06, 0.15), (-0.03, 0.2), (0.0, 1.0), (0.085, 0.75), (0.16, 0.5)):
        i = int((t0 + 0.12) * SR)
        m = int(0.18 * SR)
        crack[i:i + m] += rng.standard_normal((m, 2)) * np.exp(-np.arange(m) / (0.025 * SR))[:, None] * g
    crack = S.hp(crack, 1800, 2)
    after = np.maximum(0, tt_ - 0.12)
    rumble = S.lp(S.lp(nz() * np.minimum(1, after / 0.06) * np.exp(-after / 0.7), 160), 160) * 6
    mid = S.lp(S.hp(nz(), 300, 2), 2500) * np.exp(-after / 0.18) * (tt_ > 0.12) * 0.5
    x = crack + rumble + mid
    return x / (np.abs(x).max() + 1e-9)


sfx.add(fx(thunder(), pb.Reverb(room_size=0.95, wet_level=0.35, dry_level=0.85)), B(4) - 0.12, 0.95)
sfx.add(sx_("gather"), B(4.75) + 0.3 - 0.45, 0.7)
sfx.add(sx_("dive"), B(8) - 0.78, 0.8)
sfx.add(S.noise_riser(B(8) - B(6.5), 0.7, 2.8), B(6.5), 0.35)
for b in (7, 7.5):
    sfx.add(sx_("step"), B(b) - 0.25, 0.5)
for i in range(12):
    sfx.add(sx_("tick_a" if i % 2 else "tick_b"), B(9.2) + i * 0.034, 0.25, pan=-0.6 + 0.1 * i)
sfx.add(sx_("whip"), B(11.4) - 0.25, 0.85)
sfx.add(sx_("flip"), B(15) - 0.076, 0.8)
for k in range(8):
    sfx.add(S.blip(S.midi(88 + k), 0.015, 0.9, 140), B(12.5) + k * 0.025, 0.06)
    sfx.add(S.blip(S.midi(90 + k), 0.015, 0.9, 140), B(15) + k * 0.025, 0.06)
t = B(18)
k = 0
while t < B(22) - 1e-6:
    dur = 0.1 if t < B(21) else 0.05
    sfx.add(sx_("cut_a" if k % 2 else "cut_b"), t, 0.35 if dur > 0.06 else 0.25, pan=0.3 * (1 if k % 2 else -1))
    t += dur
    k += 1
sfx.add(SCR[-int(1.4 * SR):], B(22) - 1.4, 0.35)
sfx.add(S.subdrop(0.9, 70, 30), B(22), 0.6)
for a in (0, 0.16, 0.3, 0.5):
    sfx.add(S.blip(S.midi(100), 0.02, 0.9, 90), B(22) + 0.28 + a, 0.12)
for i in range(11):
    sfx.add(sx_("step"), B(23.5) + i * 0.025 - 0.25, 0.12)
sfx.add(S.noise_riser(LAND - B(24), 0.8, 3.0), B(24), 0.4)
sfx.add(SCR[-int(1.2 * SR):], LAND - 1.2, 0.4)
tag = "HTML to video, at the speed of light."
cmd = "$ strobe render ./site"
for i, c in enumerate(tag):
    if c != " ":
        sfx.add(S.blip(2600 + 400 * S.jhash(i, 1), 0.008, 0.9, 200), B(26.6) + i / 75, 0.05, pan=-0.3 + 0.6 * i / len(tag))
for i, c in enumerate(cmd):
    sfx.add(S.blip(1800 + 300 * S.jhash(i, 2), 0.012, 0.9, 160), B(27.4) + i / 45, 0.07, pan=0.1)

a = fx(ac.x[:N], pb.Compressor(threshold_db=-12, ratio=2.5, attack_ms=8, release_ms=120), pb.Reverb(room_size=0.8, damping=0.4, wet_level=0.25, dry_level=0.85))
i_ = fx(imp.x[:N], pb.Reverb(room_size=0.9, wet_level=0.25, dry_level=0.9))
sx = fx(sfx.x[:N], pb.Reverb(room_size=0.4, wet_level=0.1))
music = pulse * 1.7 + pad * 0.3 + sub * 0.9 + braam * 0.5 + line * 0.4 + pluck * 0.6 + a * 0.75 + i_ * 0.45
duck = np.ones(N)
j0, j1 = int(B(22) * SR), int(B(23.5) * SR)
duck[j0:j1] = 0.12
duck[j1:int(LAND * SR)] = np.linspace(0.3, 1, int(LAND * SR) - j1)
music *= duck[:, None]
mixd = S.hp(music + sx * 0.8, 25, 6)
pts = [(0, -8), (B(3.9), -3), (B(4), 0), (B(4.6), -4), (B(7.9), -3), (B(8), 1.5), (B(11), 0.5), (B(11.8), -1), (B(12), -3.5),
       (B(18), -3.5), (B(21.9), -2), (B(22), -8), (LAND - 0.02, -6), (LAND, 1), (LAND + 0.8, -0.5), (12, -3)]
tt = np.arange(N) / SR
xs, ys = zip(*pts)
mixd *= (10 ** (np.interp(tt, xs, ys) / 20))[:, None]
fade = np.ones(N); fade[-int(0.35 * SR):] = np.linspace(1, 0, int(0.35 * SR)) ** 2
mixd *= fade[:, None]
mixd = fx(mixd / (np.abs(mixd).max() + 1e-9) * 0.7, pb.Compressor(threshold_db=-12, ratio=2.2, attack_ms=12, release_ms=160), pb.Limiter(threshold_db=-2.0, release_ms=80))
tmp = os.path.join(HERE, "mix.f32")
mixd.astype(np.float32).tofile(tmp)
pre = ["-f", "f32le", "-ar", str(SR), "-ac", "2", "-i", tmp]
r = subprocess.run(["ffmpeg", "-hide_banner", "-nostats"] + pre + ["-af", "ebur128", "-f", "null", "-"], capture_output=True, text=True).stderr
I = float(re.findall(r"I:\s+(-?[\d.]+) LUFS", r)[-1])
subprocess.check_call(["ffmpeg", "-v", "error", "-y"] + pre + ["-af", f"volume={-9.5 - I:.2f}dB,alimiter=limit=0.84:attack=1:release=60:level=false",
                                                       "-ar", "48000", os.path.join(HERE, "score.wav")])
os.remove(tmp)
print("wrote score.wav")
