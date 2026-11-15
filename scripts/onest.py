"""Static Onest weights for Mini (owner, 2026-11-15: a font close to Spotify's; Manrope read wide and loose).

Slint's software renderer takes one face per file, so the variable Onest[wght].ttf (Google Fonts, OFL) is cut into
the four weights Mini uses: 400, 500 (the default text), 600 (titles), 700 (bold).

    python -I scripts/onest.py path/to/Onest[wght].ttf   # writes assets/fonts/Onest-*.ttf
"""
import pathlib
import sys

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
WEIGHTS = {400: "Regular", 500: "Medium", 600: "SemiBold", 700: "Bold"}
# The same characters as the other text fonts (scripts/fonts.py): Latin, Cyrillic, punctuation and a few signs.
TEXT = set(range(0x20, 0x7F)) | set(range(0xA0, 0x180)) | set(range(0x400, 0x530)) | set(range(0x2000, 0x2070)) \
    | {0x20BD, 0x2116, 0x2190, 0x2191, 0x2192, 0x2193, 0x2026, 0x2122, 0x00B7, 0x2212, 0x2713, 0x2715, 0x25CF, 0x2605, 0x2606}


def main() -> None:
    src = pathlib.Path(sys.argv[1])
    for w, style in WEIGHTS.items():
        f = instantiateVariableFont(TTFont(src), {"wght": w})
        name = f["name"]
        # One family «Onest»; the weight class tells the faces apart (as Manrope's files do).
        for rec in list(name.names):
            if rec.nameID in (1, 2, 3, 4, 6, 16, 17, 25):
                name.removeNames(nameID=rec.nameID)
        name.setName("Onest", 1, 3, 1, 0x409)
        name.setName("Regular" if style == "Regular" else style, 2, 3, 1, 0x409)
        name.setName(f"Onest {style}", 4, 3, 1, 0x409)
        name.setName(f"Onest-{style}", 6, 3, 1, 0x409)
        name.setName(f"Onest-{style};{w}", 3, 3, 1, 0x409)
        f["OS/2"].usWeightClass = w
        f["OS/2"].fsSelection = (f["OS/2"].fsSelection & ~0b1100001) | (0b100000 if w == 700 else 0b1000000 if w == 400 else 0)
        f["head"].macStyle = 1 if w == 700 else 0
        opts = subset.Options()
        opts.layout_features = ["*"]
        opts.name_IDs = ["*"]
        opts.notdef_outline = True
        opts.glyph_names = False
        sub = subset.Subsetter(opts)
        sub.populate(unicodes=sorted(TEXT))
        sub.subset(f)
        out = ROOT / "assets" / "fonts" / f"Onest-{style}.ttf"
        f.save(out)
        print(out.name, out.stat().st_size)


if __name__ == "__main__":
    main()
