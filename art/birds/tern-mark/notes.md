# Arctic tern (Sterna paradisaea) in flight: logo mark

**Sprite:** `sprite.txt`, 38 x 31. `.` is transparent. As half-block text it is 38 columns by 16 lines, with one blank row added at the bottom.
**Pose:** the bird is side-on in level flight, facing right. The near wing is raised and swept back. The far wing sits forward and down, under the head. The tail forks at a shallow angle.

## Photos (Wikimedia Commons)
- **Main trace:** "Arctic tern (Sterna paradisaea) in flight Myrar.jpg" by **Charles J. Sharp**, licence **CC BY-SA 4.0**.
  https://commons.wikimedia.org/wiki/File:Arctic_tern_(Sterna_paradisaea)_in_flight_Myrar.jpg
  I used the 1920 px thumbnail, mirrored with `-flop`, cropped to `1200x960+250+160` (`crop.png`), and rotated 5° clockwise to level the body line.
- **Fork shape:** "Eidersperrw sterna paradisaea with fish oben.jpg" by **Dirk Ingo Franke**, licence **CC BY-SA 2.0 de**.
  https://commons.wikimedia.org/wiki/File:Eidersperrw_sterna_paradisaea_with_fish_oben.jpg
  This photo is taken from above, so the whole fork shows. The streamers split at about 40–45% of the tail length and open about 17°.
- **Considered but not used:** "…in flight Myrar 2" (the previous banking pose), "…Myrar 3", "Eidersperr sterna paradisaea v hinte" (D. I. Franke), "Sterna paradisaea (14188418130)" (E. Chernetsova, CC BY 2.0). Contact sheets are `sheet1.jpg`, `sheet2.jpg`, `sheet3.jpg` and `sheetA.jpg`.

## Why the pose changed from the banking one (m2)
In the banking pose, the far wing hangs down behind the body at about 65° and tapers to single pixels. That limb is the steep, thin "tail" the user saw.

In the Myrar pose, the far wing points forward under the head. It is 8 px wide at its root and grey, so it cannot be read as a tail. The tail is the only thing trailing behind the bird. The flight idea is kept: a raised pointed wing, streamers, a black cap and a red bill.

## Tail
In the Myrar photo, the two streamers overlap edge-on, so only one line is visible. After levelling it runs about 15° below the body line.
- The **lower streamer** follows that photo line. It drops one row every 3 px, about 18°.
- The **upper streamer** is opened upward by the fork angle measured in the Franke photo, which puts it at about 0°. The middle of the V is about 9° off the body line.
- **Thickness:** the tail is 3 px at the rump (x12–13), then a 2 px wedge (rows 24–25, x8–13). It then splits into two 1 px streamers.
- **The gap:** it opens steadily from 1 row at x5–7 to 3 rows at x0–1. The two streamers never touch at a corner.

## Landmarks (sprite coordinates, x right / y down; also in `landmarks.json`)
| part | where |
|---|---|
| eye | (30,22) |
| beak | (33,24) (34,24) (35,25) (36,25); base at (33,24), tip at (36,25) |
| crown | (30,21) |
| nape | (28,21) |
| tail tips | (0,24) upper, (0,28) lower |
| fork split | (8,25) |
| tail base | (13,24) |
| wing tip | (15,0) |
| far-wing tip | (37,30) |

Photo landmarks (in the `crop.png` frame) are in `tmtrace.py`. `ov.png` shows the traced parts over the photo. `spov.png` shows the final pixels over the levelled photo.

## Palette (`palette.json`; `_light` holds the variant for #F4F6F8)
The photo is backlit, so the bird's underside is underexposed.

| key | colour | from the photo |
|---|---|---|
| W body | #E9EEF2 | brightest white on the neck, #ECE6E9 |
| g near wing | #C4CBD2 | backlit underwing, #87756C..#968E8B |
| G far wing | #9AA3AD | #9C8E8A..#A49184 |
| C cap | #424B5C | #2C2C3A..#333749 |
| R bill | #CF2B2B | #C6252A |
| E eye | #DCE3E9 | none (added highlight) |

- **Whites:** lifted to white, keeping a cool cast.
- **Wing greys:** cooled into the app's slate range. The far wing is kept darker for depth.
- **Cap:** lifted into the dark slate band the user's rule requires, so it is never black on the #0A0E12 background.
- **Bill:** lifted a little so it holds up on the dark background.
- **Eye:** a 1 px highlight surrounded by the cap on all four sides, so it reads as an eye rather than a notch.

## Choices
- **Features kept:** a pointed wing tip, the white nape separating the cap from the wing, and the bill drooping about 22° as in the photo.
- **Feet dropped:** the photo shows them tucked at the vent. At this size they read as a red spot on the belly.
- **Shading:** flat tones only, no outlines.
- **Size:** at a 32 px icon the mark renders at 0.84 px per sprite pixel. The streamers stay visible, which is checked in the `preview.png` sizes row.
- **Scripts:** `tmtrace.py` holds the landmarks and the designed fork, `raster.py` rasterises with 6x6 supersampling, `final.py` does the hand finish, `check.py` finds stray pixels, and `preview.py` renders `preview.png`.

---

## v4 revision (2026-09-26): what changed after the critique

The sprite is still **38 x 31** in the same Myrar flight pose. It is built by `v4/build4.py` (variant `F`), written by `v4/final4.py`, checked by `v4/check4.py` and rendered by `v4/preview4.py`. The previous files are kept as `v4/prev_*`.

**New reference photo:** "Arctic tern (Sterna paradisaea) with eel Blonduos.jpg" by **Charles J. Sharp**, licence **CC BY-SA 4.0**.
https://commons.wikimedia.org/wiki/File:Arctic_tern_(Sterna_paradisaea)_with_eel_Blonduos.jpg
It is a true side view in level flight. Three things in it drove this revision:
- The tail is a grey wedge from the rump that splits into two streamers. The fork is narrow in side view.
- The tail is clearly greyer than the body. Sampled: tail #7B8493, lit body #D6D2D3.
- The hand is slim and pointed. The arm is the broad part, with a concave trailing edge where the two meet.

### Tail
- **Upper streamer:** it leaves row 24. The run is (3–7,24), then the tip at (0–2,25). That is about 6–8°, and (4,25) stays empty.
- **Lower streamer:** (5–8,26), then (3–7,27), then (1–3,28), then the tip at (0–1,29). That is about 20°. It is 2 px thick from the root to x5, and every step overlaps the one before, so it is a solid line rather than a corner-joined staircase.
- **Root:** a 2 px wedge at rows 24–25, x8–12.
- **The fork gap:** it opens steadily, 1 row at x5–7, 2 rows at x1–4 and 3 rows at x0. `check4.py` confirms the two streamers never touch, even at a corner.
- **Colour:** the tail has its own key, `T`. On dark it is #CFD6DC, one step greyer than the body as in the photos. The tone change marks where the rump ends, so row 24 no longer reads as one white spear from tail tip to bill. On light it is #7A8692, which is 3.43:1 against the background.

### Wing (the "sailboat" read)
- **Dark primary line `P`:** 1 px along the hand's leading edge, from the tip (15,0) to the wrist (25,12). It is #8A94A0 on dark and #5F6B78 on light, and stands for the dark outer web in the photo.
- **Leading edge:** it now moves exactly one pixel per row from (17,4) to (25,12). The only 2-row steps are at the tip (rows 0–3).
- **Trailing edge:** it is bowed, sitting at x15 for rows 0–3, x16 for rows 4–5, x17 for rows 6–7 and x18 for rows 8–13. At row 14 it steps back out to x17 for the arm. That gives a slim hand and a broader arm, as in the Blonduos photo.
- **Width:** the trailing edge only steps where the leading edge also steps, so the width never shrinks going down: 1,1,2,2,2,3,3,4,4,5,6,7,8,8,9…
- **Stray stub:** (17,21) is now `g`, so the trailing edge runs straight down into the back.
- **Belly:** a gentle bulge at (17–24,26) breaks the flat hull.

### Head
- **Far wing:** its forward edge moved back one pixel, and the tip is now (35–36,30). Every bill pixel has at least 2 px clear below it, and nothing sits in front of the bill tip.
- **Eye:** `E` is now a muted glint, #7E8998 on dark and #5E6A76 on light. It no longer reads as a white stare on dark or as a hole in the cap on light.
- **Cap on dark:** `C` is lifted to #566175. That is 3.10:1 against #0A0E12 and 5.35:1 against the white body. The light-mode cap is unchanged.

### Light variant
The body is lifted to #AEB9C3 (1.84:1, previously 1.65:1). The wing is #8F9BA6, which keeps it 1.42:1 apart from the body. The tail is #7A8692 (3.43:1).

The critique's suggested tail tone, #8E9AA6, measures only 2.65:1, so I used a darker one.

### Terminal check
The half-block panel is now drawn pixel-exact. Each cell is an 11 x 18 box, with the upper half set to the fg colour (top pixel) and the lower half set to the bg colour (bottom pixel), and no font glyph is involved. This is how terminals draw U+2580 themselves, so the panel has no hairline seams. The `sizes` row adds a 32 px box-filter downsample at 4x, and small terminal renders on dark and light.

### Landmarks (v4, also in `landmarks.json`)
| part | where |
|---|---|
| eye | (30,22) |
| crown | (30,21) |
| nape | (28,21) |
| beak | base (33,24), tip (36,25) |
| tail tips | (0,25) upper, (0,29) lower |
| fork crotch | (7,25) |
| tail base | (12,24) |
| wing tip | (15,0) |
| wrist | (25,12) |
| far-wing tip | (36,30) |


## Orchestrator edits (2026-09-27)
- The raised wing's arm (rows 13-21) narrows toward the body (x0 = 18 + (row-12)//2), so it reads as a tern's blade, not a sail.
- The bill is one wedge: row 24 x33-36, row 25 x33-34 (landmarks updated).
