import sys
import numpy as np
from scipy.ndimage import gaussian_filter

N = 64
SIGMA = 1.5
rng = np.random.default_rng(7)


def energy(m):
    return gaussian_filter(m.astype(np.float64), SIGMA, mode="wrap")


def tightest(m):
    e = energy(m)
    e[~m] = -np.inf
    return np.unravel_index(np.argmax(e), m.shape)


def largest_void(m):
    e = energy(m)
    e[m] = np.inf
    return np.unravel_index(np.argmin(e), m.shape)


m = np.zeros((N, N), bool)
m.flat[rng.choice(N * N, N * N // 10, replace=False)] = True
while True:
    a = tightest(m)
    m[a] = False
    b = largest_void(m)
    if a == b:
        m[a] = True
        break
    m[b] = True

rank = np.zeros((N, N), np.int32)
ones = int(m.sum())
p = m.copy()
for r in range(ones - 1, -1, -1):
    a = tightest(p)
    p[a] = False
    rank[a] = r
p = m.copy()
for r in range(ones, N * N):
    b = largest_void(p)
    p[b] = True
    rank[b] = r

np.savetxt(sys.argv[1], rank, fmt="%d", delimiter=",")
