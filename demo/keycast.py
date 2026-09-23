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
from collections import Counter
from typing import NamedTuple

from PIL import Image, ImageDraw, ImageFont, ImageSequence

FONT = "/System/Library/Fonts/Menlo.ttc"

# The overlay's own colours: desaturated, so the bar sits next to the terminal
# colours of demo/demo.gif rather than on top of them.
DROPPED = (224, 108, 117)
DELIVERED = (152, 195, 121)
BOX_FILL = (30, 30, 38)
BOX_EDGE = (68, 71, 90)
KEY_TEXT = (220, 223, 228)
KEY_LABEL = (120, 124, 138)
KEY_PENDING = (78, 81, 96)

# Colours in the shared palette. Larger keeps the terminal's gradients, and
# costs file size: the GIF roughly doubles between 64 and 256.
PALETTE_SIZE = 128

PHRASE = "unlock"
PHRASE_START = 14.8
PHRASE_INTERVAL = 0.3  # `Set TypingSpeed` around the phrase in demo/lock.tape
PHRASE_NOTE = "the unlock phrase - taken, not passed on"


class Event(NamedTuple):
    """One label on the bar, shown from `start` until `end` seconds."""

    start: float
    end: float
    key: str
    note: str
    colour: tuple[int, int, int]
    typed: int | None = None  # letters of `key` entered so far, for the phrase


def phrase_events(until: float) -> list[Event]:
    """The phrase filling in, one letter per keystroke, then held.

    keylock unlocks on the last letters typed rather than on a submitted line,
    so the bar spells the phrase out as it fills instead of showing it whole:
    the letters already typed are lit, the rest stay dim.
    """
    steps = [
        Event(
            PHRASE_START + i * PHRASE_INTERVAL,
            PHRASE_START + (i + 1) * PHRASE_INTERVAL,
            PHRASE,
            PHRASE_NOTE,
            DELIVERED,
            typed=i + 1,
        )
        for i in range(len(PHRASE))
    ]
    return steps + [Event(steps[-1].end, until, PHRASE, PHRASE_NOTE, DELIVERED, len(PHRASE))]


# Anchored to the render, not to the tape's arithmetic: the lock leaves the
# sidebar row at 18.1s and the job prints its abort line at 20.7s.
EVENTS = [
    Event(7.8, 9.8, "a", "dropped - the session is locked", DROPPED),
    Event(9.8, 11.8, "space", "dropped - the session is locked", DROPPED),
    Event(11.8, 14.8, "ctrl+c", "dropped - the session is locked", DROPPED),
    *phrase_events(until=20.6),
    Event(20.6, 26.0, "x", "reaches the job, which aborts", DELIVERED),
]


def draw_key(
    canvas: ImageDraw.ImageDraw, at: tuple[int, int], font: ImageFont.FreeTypeFont, event: Event
) -> None:
    """The key itself: one label, or the phrase with the untyped tail dimmed."""
    if event.typed is None:
        canvas.text(at, event.key, font=font, fill=KEY_TEXT)
        return

    x, y = at
    for i, letter in enumerate(event.key):
        canvas.text((x, y), letter, font=font, fill=KEY_TEXT if i < event.typed else KEY_PENDING)
        x += font.getlength(letter) + 10


def draw(frame: Image.Image, second: float) -> Image.Image:
    event = next((e for e in EVENTS if e.start <= second < e.end), None)
    if event is None:
        return frame

    frame = frame.convert("RGB")
    canvas = ImageDraw.Draw(frame)
    key_font = ImageFont.truetype(FONT, 26)
    note_font = ImageFont.truetype(FONT, 18)

    width, height = frame.size
    box_w, box_h = 520, 84
    x = (width - box_w) // 2
    y = height - box_h - 40

    canvas.rounded_rectangle(
        [x, y, x + box_w, y + box_h], radius=10, fill=BOX_FILL, outline=BOX_EDGE
    )
    canvas.text((x + 24, y + 14), "key:", font=note_font, fill=KEY_LABEL)
    draw_key(canvas, (x + 24 + 52, y + 8), key_font, event)
    canvas.text((x + 24, y + 50), event.note, font=note_font, fill=event.colour)
    return frame


def build_palette(frames: list[Image.Image]) -> Image.Image:
    """One shared palette for the whole GIF.

    Per-frame palettes cost several hundred KB, and a palette taken from a
    single frame drops every colour that frame happens not to show — the first
    cut of this overlay quantized the green notes to grey. Median cut also
    moves the colours it keeps, and it shifted the terminal background off the
    one demo/demo.gif uses, which is exactly the thing a reader compares.

    So: median cut over a strip of frames spread across the recording for the
    bulk of the palette, with the overlay's own colours written in verbatim at
    the end. The background is pinned separately, after quantization.
    """
    picks = [frames[i * len(frames) // 8] for i in range(8)]
    width, height = picks[0].size
    strip = Image.new("RGB", (width, height * len(picks)))
    for i, frame in enumerate(picks):
        strip.paste(frame, (0, i * height))

    exact = [BOX_FILL, BOX_EDGE, KEY_TEXT, KEY_LABEL, KEY_PENDING, DROPPED, DELIVERED]
    entries = strip.quantize(
        colors=PALETTE_SIZE - len(exact), method=Image.MEDIANCUT
    ).getpalette()[: 3 * (PALETTE_SIZE - len(exact))]
    for colour in exact:
        entries.extend(colour)

    palette = Image.new("P", (1, 1))
    palette.putpalette(entries + [0] * (768 - len(entries)))
    return palette


def pin_background(frames: list[Image.Image], paletted: list[Image.Image]) -> None:
    """Give the terminal background back its exact colour.

    Quantizing with a ready-made palette is not a nearest-colour search: even
    with the background in the palette verbatim, PIL mapped it to a neighbour
    two units away. Two units are invisible on their own, but demo/lock.gif
    sits next to demo/demo.gif in the README, where the eye compares the two
    backgrounds directly. Rewriting the one palette entry the background landed
    on costs nothing and moves nothing else: every colour it covers is within
    those two units.
    """
    background = Counter(frames[len(frames) // 2].get_flattened_data()).most_common(1)[0][0]
    index = Counter(paletted[len(paletted) // 2].get_flattened_data()).most_common(1)[0][0]
    entries = list(paletted[0].getpalette())
    entries[3 * index : 3 * index + 3] = list(background)
    for frame in paletted:
        frame.putpalette(entries)


def main(gif: pathlib.Path) -> int:
    with Image.open(gif) as source:
        frames, durations, elapsed = [], [], 0.0
        for frame in ImageSequence.Iterator(source):
            delay = frame.info.get("duration", 40)
            frames.append(draw(frame.convert("RGB"), elapsed / 1000.0))
            durations.append(delay)
            elapsed += delay

    palette = build_palette(frames)
    paletted = [f.quantize(palette=palette, dither=Image.Dither.NONE) for f in frames]
    pin_background(frames, paletted)

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
