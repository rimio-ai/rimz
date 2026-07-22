#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["fonttools==4.61.1"]
# ///
"""Generate the blank font used by the ttyd pixel compatibility layer."""

from pathlib import Path
import re
import sys

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen


ROOT = Path(__file__).resolve().parents[1]
PIXEL_SOURCE = ROOT / "crates/rimz/src/sidebar_pane/pixel/mod.rs"
DEFAULT_OUTPUT = ROOT / "crates/rimz/src/web/ttyd/pixel_blank.ttf"
PLACEHOLDER = 0x10EEEE


def codepoints() -> list[int]:
    source = PIXEL_SOURCE.read_text()
    match = re.search(
        r"ROW_COLUMN_DIACRITICS: \[char; \d+\] = \[(.*?)\n\];",
        source,
        re.DOTALL,
    )
    if match is None:
        raise SystemExit(f"could not find ROW_COLUMN_DIACRITICS in {PIXEL_SOURCE}")
    values = [int(value, 16) for value in re.findall(r"'\\u\{([0-9a-fA-F]+)\}'", match.group(1))]
    if len(values) != 297:
        raise SystemExit(f"expected 297 row/column diacritics, found {len(values)}")
    return sorted([PLACEHOLDER, *values])


def build(output: Path) -> None:
    builder = FontBuilder(1_000, isTTF=True)
    glyph_order = [".notdef", "blank"]
    builder.setupGlyphOrder(glyph_order)
    builder.setupCharacterMap(dict.fromkeys(codepoints(), "blank"))

    glyphs = {}
    for name in glyph_order:
        pen = TTGlyphPen(None)
        glyphs[name] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({".notdef": (500, 0), "blank": (0, 0)})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupOS2(
        sTypoAscender=800,
        sTypoDescender=-200,
        usWinAscent=800,
        usWinDescent=200,
    )
    builder.setupNameTable(
        {
            "familyName": "RimZ Pixel Blank",
            "styleName": "Regular",
            "uniqueFontIdentifier": "RimZ Pixel Blank Regular",
            "fullName": "RimZ Pixel Blank Regular",
            "psName": "RimZ-Pixel-Blank",
            "version": "Version 1.000",
        }
    )
    builder.setupPost()
    builder.setupMaxp()
    builder.setupHead(created=0, modified=0)
    builder.font.recalcTimestamp = False
    output.parent.mkdir(parents=True, exist_ok=True)
    builder.save(output)


if __name__ == "__main__":
    destination = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_OUTPUT
    build(destination)
