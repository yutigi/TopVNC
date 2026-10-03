"""Per-zone quality of decoded frames: PSNR and SSIM on luma (BT.601).

Scores the output of the R&D codec study, examples/fovea_study.rs in
prototype.patch (applies to c46bb72); see spec.md, Background.
"""
import json, sys, os
import numpy as np

S = sys.argv[1]           # study dir
FR = sys.argv[2]          # frames dir
W, H = 1920, 1080
zinfo = json.load(open(f"{S}/zones.json"))
tile, cols, rows = zinfo["tile"], zinfo["columns"], zinfo["rows"]
tz = np.array(zinfo["zones"], dtype=np.uint8).reshape(rows, cols)
zmap = np.kron(tz, np.ones((tile, tile), dtype=np.uint8))[:H, :W]

def luma(rgb):
    rgb = rgb.astype(np.float64)
    return 0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2]

def chroma(rgb):
    rgb = rgb.astype(np.float64)
    r, g, b = rgb[..., 0], rgb[..., 1], rgb[..., 2]
    cb = -0.168736 * r - 0.331264 * g + 0.5 * b
    cr = 0.5 * r - 0.418688 * g - 0.081312 * b
    return cb, cr

def load_rgb(path):
    return np.frombuffer(open(path, "rb").read(), dtype=np.uint8).reshape(H, W, 3)

def box(img, r):
    """Mean over a (2r+1)^2 window, edges replicated, via integral image."""
    k = 2 * r + 1
    p = np.pad(img, r + 1, mode="edge")
    c = p.cumsum(0).cumsum(1)
    s = c[k:, k:] - c[:-k, k:] - c[k:, :-k] + c[:-k, :-k]
    return s[: img.shape[0], : img.shape[1]] / (k * k)

def ssim_map(a, b, r=3):
    C1, C2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    ma, mb = box(a, r), box(b, r)
    va = box(a * a, r) - ma * ma
    vb = box(b * b, r) - mb * mb
    cov = box(a * b, r) - ma * mb
    return ((2 * ma * mb + C1) * (2 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2))

def psnr(mse):
    return 99.0 if mse <= 1e-12 else 10 * np.log10(255 ** 2 / mse)

results = [json.loads(l) for l in open(f"{S}/results.jsonl")]
refs = {}
out = []
for r in results:
    name = r["frame"]
    if name not in refs:
        if name == "synthetic":
            # Reference is the U6 decoded? No: regenerate is costly; use strategy-independent source dumped by study if present.
            path = f"{S}/synthetic_source.rgb"
            refs[name] = load_rgb(path) if os.path.exists(path) else None
        else:
            refs[name] = load_rgb(f"{FR}/{name}.rgb")
    ref_rgb = refs[name]
    if ref_rgb is None:
        continue
    dec_rgb = load_rgb(f"{S}/decoded/{name}__{r['strategy']:02d}.rgb")
    ref, dec = luma(ref_rgb), luma(dec_rgb)
    (rcb, rcr), (dcb, dcr) = chroma(ref_rgb), chroma(dec_rgb)
    cerr = ((dcb - rcb) ** 2 + (dcr - rcr) ** 2) / 2
    err = (dec - ref) ** 2
    sm = ssim_map(ref, dec)
    q = {}
    for z, label in enumerate(["fovea", "mid", "periph"]):
        m = zmap == z
        q[f"psnr_{label}"] = round(psnr(err[m].mean()), 2)
        q[f"ssim_{label}"] = round(float(sm[m].mean()), 4)
        q[f"cpsnr_{label}"] = round(psnr(cerr[m].mean()), 2)
    q["psnr_all"] = round(psnr(err.mean()), 2)
    q["ssim_all"] = round(float(sm.mean()), 4)
    r.update(q)
    out.append(r)
with open(f"{S}/scored.jsonl", "w") as f:
    for r in out:
        f.write(json.dumps(r) + "\n")
# Pixel share per zone
print("zone pixel share:", [round(float((zmap == z).mean()), 3) for z in range(3)])
