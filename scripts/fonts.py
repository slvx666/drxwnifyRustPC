"""Subsets the UI fonts to the characters the app shows and writes assets/fonts + assets/icons.json.

Text fonts keep Latin, Latin-1, Latin Extended-A, Cyrillic and punctuation. Japanese decor fonts keep only the
kanji/kana that the UI itself draws (brand glyph, section words). The icon font keeps only the icons in use.
Track titles in Japanese/Chinese/Korean are drawn with the system fonts, not these.

Usage: python -I scripts/fonts.py <pc fonts dir> <pc ui source dir>
"""
import json, re, sys, pathlib
from fontTools import subset
from fontTools.ttLib import TTFont

src, ui = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
out = pathlib.Path(__file__).resolve().parent.parent / "assets" / "fonts"
out.mkdir(parents=True, exist_ok=True)

TEXT = set(range(0x20, 0x7F)) | set(range(0xA0, 0x180)) | set(range(0x400, 0x530)) | set(range(0x2000, 0x2070)) \
    | {0x20BD, 0x2116, 0x2190, 0x2191, 0x2192, 0x2193, 0x2026, 0x2122, 0x00B7, 0x2212, 0x2713, 0x2715, 0x25CF, 0x2605, 0x2606}

# CJK the UI draws itself (decor words, brand glyph): collected from the pc UI sources.
cjk = set()
for f in ui.rglob("*.kt"):
    for ch in f.read_text(encoding="utf-8"):
        o = ord(ch)
        if 0x3000 <= o <= 0x30FF or 0x4E00 <= o <= 0x9FFF or 0xFF00 <= o <= 0xFFEF:
            cjk.add(o)

ti = (ui / "Ti.kt").read_text(encoding="utf-8")
icons = {m.group(1): int(m.group(2), 16) for m in re.finditer(r'const val (\w+) = "\\u([0-9A-Fa-f]{4})"', ti)}

def run(name, unicodes, layout=True):
    path = src / name
    if not path.exists():
        print("missing", name); return
    opts = subset.Options()
    opts.layout_features = ["*"] if layout else []
    if not layout:
        # The icon font's ligature table is malformed and icons need no shaping.
        opts.drop_tables += ["GSUB", "GPOS", "GDEF"]
    opts.name_IDs = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    font = TTFont(path, lazy=False) if layout else TTFont(path)
    if not layout:
        for t in ("GSUB", "GPOS", "GDEF"):
            if t in font:
                del font[t]
    s = subset.Subsetter(opts)
    s.populate(unicodes=sorted(unicodes))
    s.subset(font)
    font.save(out / name)
    print(f"{name}: {path.stat().st_size // 1024} KB -> {(out / name).stat().st_size // 1024} KB")

for f in sorted(src.glob("*.ttf")):
    if f.name == "tabler-icons.ttf":
        run(f.name, set(icons.values()), layout=False)
    elif "JP" in f.name:
        run(f.name, TEXT | cjk)
    else:
        run(f.name, TEXT)

(out.parent / "icons.json").write_text(json.dumps({k: f"{v:04X}" for k, v in sorted(icons.items())}, indent=1), encoding="utf-8")
total = sum(p.stat().st_size for p in out.glob("*.ttf"))
print(f"total {total // 1024} KB, {len(icons)} icons, {len(cjk)} CJK chars")
