# Preview

`/Apps/preview/preview` (dock tile from `/Apps/preview/manifest`). The window is `user/preview`;
the decoders are the `user/imgdec` crate, which has host tests (`scripts/imgdec-test.sh`).
It follows docs/ui-concepts/SPEC-APPS.md, section 5.

Start it with a path to a picture (`/Apps/preview/preview FILE`): the pictures in the same folder
(`.png .ppm .pgm .pbm .pnm .bmp .pdf`) fill a thumbnail strip and Left / Right / Space step through
them. Or open one with Cmd-O (the Powerbox panel) or drop it on the window.

## Formats

| | |
| --- | --- |
| PNG | every colour type and bit depth (1 to 16), palettes, tRNS transparency, Adam7 interlace; chunk CRCs and the zlib checksum are checked. Gamma and colour profiles are ignored |
| PPM, PGM, PBM | P1 to P6, text and binary, maxval to 65535 |
| BMP | 1/4/8-bit palettes, 16/24/32 bit, bottom-up or top-down; RLE is reported as unsupported |
| PDF | no rendering: a card with the page count and the readable text, **Show text** (a read-only view) and **Save text as...**. Text drawn with embedded glyph-id fonts, unusual compression or encryption can't be read and the card says so |
| JPEG, GIF, others | a card saying they can't be opened yet |

Images over 8 megapixels are refused before anything is allocated; a damaged or hostile file gives
an error card (`Can not open this image` and why), never a crash (the decoders are fuzzed with
truncated and mutated files in the host tests).

## Controls

The zoom control has Fit, 25%, 50%, 100% and 200% (Cmd-0 is Fit, Cmd-+ and Cmd-- step through them):
**Fit** never enlarges, 25 and 50 use a box average (alpha weighted), 100 is exact, 200 repeats
pixels. Drag to pan a picture bigger than the window (a mini map shows where you are); the wheel pans
too. **Rotate** (Cmd-R, Shift for the other way) turns it a quarter. **Info** (Cmd-I) shows size, file
size, format and when it was modified. **Save as...** (Cmd-S) writes a PNG through the Save panel
(transparent areas are flattened onto white); for a PDF it saves the text.

## Sandbox

Preview reads untrusted files, so its first act is `sandbox("fs=<folder of the picture>:r
fs=/dev/wm:r net=none spawn=0 clipboard=0")` (with no path, only `/dev/wm` for the theme): no
network, no spawning, no clipboard, and files only from that folder, read-only, plus whatever the
user hands over through the Open, Save panels or a drop. The thumbnails and the strip need that
folder grant; there is no "open with" from Files yet.

Test: `scripts/preview-test.sh` boots a desktop, opens a folder of a PNG, PPM, BMP, a damaged PNG and
a PDF, steps through them, opens the info panel, zooms, rotates, pans and opens the PDF's text view.
