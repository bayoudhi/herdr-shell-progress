#!/usr/bin/env python3
"""Draws a keycast bar onto demo/lock.gif.

A locked keylock session drops every key, so a recording of one shows nothing
happening — which is the point, and also impossible to read. This overlays the
keys the tape presses, and whether each one reached the command.

The timings below follow demo/lock.tape's key sequence; change one and change
the other. Times are seconds from the start of the rendered GIF.

    python3 demo/keycast.py demo/lock.gif

Needs Pillow (pip install pillow). Rewrites the GIF in place, keeping every
frame's own delay — re-encoding through ffmpeg dropped frames and shortened
the recording, which slid the labels out of sync with the keys.
"""

import pathlib
import sys

from PIL import Image, ImageDraw, ImageFont, ImageSequence

FONT = "/System/Library/Fonts/Menlo.ttc"

# (start, end, key label, note, note colour)
DROPPED = (214, 92, 92)
DELIVERED = (126, 199, 126)
EVENTS = [
    (8.1, 10.1, "a", "dropped - the session is locked", DROPPED),
    (10.1, 12.1, "space", "dropped - the session is locked", DROPPED),
    (12.1, 15.1, "ctrl+c", "dropped - the session is locked", DROPPED),
    (15.1, 19.4, "u n l o c k", "the unlock phrase - taken, not passed on", DELIVERED),
    (19.4, 24.0, "x", "reaches the job, which aborts", DELIVERED),
]


def draw(frame: Image.Image, second: float) -> Image.Image:
    event = next((e for e in EVENTS if e[0] <= second < e[1]), None)
    if event is None:
        return frame
    _, _, key, note, colour = event

    frame = frame.convert("RGB")
    canvas = ImageDraw.Draw(frame)
    key_font = ImageFont.truetype(FONT, 26)
    note_font = ImageFont.truetype(FONT, 18)

    width, height = frame.size
    box_w, box_h = 520, 84
    x = (width - box_w) // 2
    y = height - box_h - 40

    canvas.rounded_rectangle(
        [x, y, x + box_w, y + box_h], radius=10, fill=(32, 32, 38), outline=(70, 70, 80)
    )
    canvas.text((x + 24, y + 14), "key:", font=note_font, fill=(150, 150, 160))
    canvas.text((x + 24 + 52, y + 8), key, font=key_font, fill=(240, 240, 245))
    canvas.text((x + 24, y + 50), note, font=note_font, fill=colour)
    return frame


def main(gif: pathlib.Path) -> int:
    with Image.open(gif) as source:
        frames, durations, elapsed = [], [], 0.0
        for frame in ImageSequence.Iterator(source):
            delay = frame.info.get("duration", 40)
            frames.append(draw(frame.convert("RGB"), elapsed / 1000.0))
            durations.append(delay)
            elapsed += delay

    # One shared palette keeps the file small: per-frame palettes cost several
    # hundred KB. It is built from a mid frame with a wide band of every note
    # colour painted over it: a plain mid frame only ever carries one of them,
    # and the missing colours came out grey. The bands have to be large — a
    # few hundred pixels of a new colour do not survive median cut.
    sample = frames[len(frames) // 2].copy()
    swatch = ImageDraw.Draw(sample)
    colours = sorted({e[4] for e in EVENTS})
    band = sample.height // (2 * len(colours))
    for i, colour in enumerate(colours):
        swatch.rectangle([0, i * band, sample.width, (i + 1) * band], fill=colour)
    palette = sample.quantize(colors=64, method=Image.MEDIANCUT)
    paletted = [f.quantize(palette=palette, dither=Image.Dither.NONE) for f in frames]

    paletted[0].save(
        gif,
        save_all=True,
        append_images=paletted[1:],
        duration=durations,
        loop=0,
        optimize=True,
    )
    print(f"{gif}: {len(frames)} frames, {elapsed / 1000:.1f}s, keycast applied")
    return 0


if __name__ == "__main__":
    target = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "demo/lock.gif")
    raise SystemExit(main(target))
