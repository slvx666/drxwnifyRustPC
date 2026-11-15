"""Writes assets/brand/wordmark.png: «drxwnify» in Rubik Wet Paint (OFL) as a white mask at 3x, cropped to the ink.

Mini shows it tinted with the theme's text colour (Image.colorize), so the font file itself stays out of the binary.
Usage: python -I scripts/wordmark.py <RubikWetPaint-Regular.ttf>
"""
import pathlib
import sys
from PIL import Image, ImageDraw, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
font = ImageFont.truetype(sys.argv[1], 72)
im = Image.new('L', (700, 160), 0)
ImageDraw.Draw(im).text((20, 20), 'drxwnify', font=font, fill=255)
box = im.getbbox()
mask = im.crop(box)
out = Image.new('RGBA', mask.size, (255, 255, 255, 0))
out.putalpha(mask)
dst = ROOT / 'assets' / 'brand' / 'wordmark.png'
dst.parent.mkdir(parents=True, exist_ok=True)
out.save(dst, optimize=True)
print(dst, out.size, dst.stat().st_size, 'bytes')
