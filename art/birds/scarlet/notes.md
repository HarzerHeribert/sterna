# Scarlet macaw (Ara macao): 18 x 24 sprite, perched, facing right

## Photos (Wikimedia Commons)
| use | file | author | licence |
|---|---|---|---|
| **traced** (outline, head profile, proportions), mirrored | [Scarlet macaw.jpg](https://commons.wikimedia.org/wiki/File:Scarlet_macaw.jpg) | Rivadavia.vila | CC0 |
| true side view: wing layout, feet, sunlit colours; mirrored | [Ara macao -Vogelpark Walsrode -perch-8a.jpg](https://commons.wikimedia.org/wiki/File:Ara_macao_-Vogelpark_Walsrode_-perch-8a.jpg) | Tobias (Flickr, "Bird Park (Vogelpark Walsrode) Germany") | CC BY-SA 2.0 |
| evenly lit colours: red, yellow, green tips, blue, face, beak | [Scarlet Macaw Phoenix Zoo Mar23 A7R 04528.jpg](https://commons.wikimedia.org/wiki/File:Scarlet_Macaw_Phoenix_Zoo_Mar23_A7R_04528.jpg) | Timothy A. Gonsalves | CC BY-SA 4.0 |

I also looked at five other candidates (`p2`–`p4`, `p7`, `p8`, all shown in `sheet.jpg`) and did not use them.

## Method
1. `trace.py` holds the landmark lists measured on `p5.jpg`. The crop is 360x1240+330+40, flopped so the bird faces right. The lists cover body, wing, tail, tail edge, head, face patch, both mandibles and the eye. The trace is Catmull-Rom, and `fit.png` shows it over the photo at 50%.
2. The photo is an upright bird with its tail hanging straight down, about 4.5:1. That does not fit 18x24 without the body shrinking to 5 px. To fit, I leaned the body 10° forward about the feet and kept the head upright. The head is scaled ×1.45 so the face, eye and two-tone beak survive. The tail is compressed to ~0.5 of its length, swung back 25° and widened ×1.35, so it is broad, not a thin streamer (the problem you flagged on the tern). `g1_grid.png` is that pose over the 18x24 grid.
3. `gridtrace.py` redraws the parts in sprite space and rasterises them with `crispEdges` on #FF00FF, giving `work/gt.txt`. I hand-finished that through `v2`–`v4b`, and `sprite.txt` is `v4b`.

## Landmarks (sprite coordinates, x = column, y = row)
- eye `[13,2]`, fully ringed by the white face patch (rows 1–4, x 11–14)
- crown `[13,0]`, nape `[10,2]`
- upper mandible: 13 px, x 15–17, rows 1–6. The front bulges to x17 on rows 2–4, and the hook tip is at `[15,6]`, curling back under the dark lower mandible.
- lower mandible: 6 px, x 13–15, rows 3–5
- wing: red coverts on rows 6–9 with a dark-red leading-edge line from (9,8) to (12,12). The yellow band is rows 9–12, the green-tip scallop rows 12–13 and the blue flight feathers rows 13–17.
- feet: `[12,15] [13,15]` with claws `[11,16] [13,16]` over the perch. The perch is row 16 across the full width, as in the mockup. The tail is drawn in front of it.
- tail: from under the wing tip to the tip at `[1,23]`. It is red, with a blue outer feather along its upper edge, a blue edge underneath near the base, and one pale-blue rump pixel at `[4,17]` (the macaw's light-blue upper-tail coverts).

`landmarks.json` has every pixel list: beak, both mandibles, face, yellow band and feet.

## Colours
I took these from the evenly lit Phoenix Zoo photo using per-hue medians and highlights. The traced photo is in shade, and the Walsrode one is over-saturated by sun.
- red `#D8241C` / shadow `#9C1410` / tail red `#C81E18`
- yellow `#F6BA1C`, green tips `#5E8A3C` (olive, as sampled), blue `#3558A8`, pale-blue rump `#8FB0DE`
- face `#EEE7E2`, upper mandible `#E2D5BF`
- Black parts follow your dark-slate rule. The lower mandible is `#3A4450` and the feet are `#4B5663`. The eye `#2A313B` never touches the background, because the face patch encloses it.
- Only the feet (`#4B5663`) sit against the terminal black, and they stay distinguishable. The white face never touches either background: red and beak surround it on every side.

## Choices and limits
- The pose is stylised to fit the grid: it leans, the head is enlarged and the tail is compressed. The photo shows the true proportions; the sprite keeps the tail as its longest feature.
- The eye has no highlight. One pixel inside the white patch reads as an eye, and a highlight needs a second pixel.
- Two lines run diagonally on purpose: the green tips (a checker that reads as a scalloped edge) and the dark-red wing edge. They pass the lint for continuity, and no pixel is isolated.
- In `preview.png`, the half-block uses the locally installed `JetBrainsMono NF` at line-height 1. Plain "JetBrains Mono" is not installed here, and the browser fallback font leaves seams between rows.

## Revision 2 (2026-09-26): critic's fixes
The previous version is kept as `v5_before.txt`, `palette_v4b.json`, `landmarks_v4b.json` and `preview_v4b.png`. The drafts were `v6a`–`v6e` and `v7a`–`v7b`, compared in `cmp7c.png` and `cmp8c.png`. `sprite.txt` is `v7a`. `preview.py` builds `preview.png`.

- **Tail length and proportion.** I dropped one pure-red mantle row (old row 7) and one blue flight-feather row (old row 15), which moves the perch and feet from row 16 to row 14. The tail now runs from under the wing tip at row 15 to x0–1 on row 23. Per-row widths are 4, 4, 4 (rows 15–17), then 3, 3, 3 (18–20), then 2, 2, 2 (21–23), and never thinner than 2. The upper edge continues the back line at x5 (5, 4, 3, 3, 2, 1, 1, 0, 0). The axis runs from the centre (7.0, row 15) to (1.0, row 23), which is about 53°, and it is about 11.2 px long from the top of row 15 to the bottom of row 23. It was about 8.5 px at about 1.4:1 before. The tail beyond the wing tip is now about 0.75x the crown-to-wing-tip height (14 rows); it was about 0.5x. The photo's 1.4x does not fit 24 rows without shrinking the head.
- **Tail colours.** The blue lower-edge fragment (old (9,18), (8,19), (7,20)) is gone, and the lower edge is red all the way down. The blue outer feather `t` runs along the upper edge from (5,15) to (0,22). Row 23 is red (`TT`), so the tip is red, as in the traced photo.
- **Rump.** I removed the pale-blue `b` pixel and dropped `b` from the palette. The rump is hidden by the wing in this side view.
- **Eye.** I set old (14,3) from L to W. The eye (13,2) now has W on all 8 sides. The lower mandible is the 2x2 block (13,4), (14,4), (13,5), (14,5), so the eye no longer runs into the dark jaw at any scale.
- **Hook.** I set (15,5) from L to U. The hook runs (16,5)–(15,5)–(15,6) with edge contact and curls around the front of the lower mandible.
- **Beak on light.** A new `u` = `#C9B48E` (mid-horn) sits on the front-profile edge pixels (16,1), (17,2), (17,3), (17,4), (16,5) and on the hook tip (15,6). Against `#F4F6F8` that is about 1.9:1; the old U was 1.2:1. I also darkened U from `#E2D5BF` to `#DDCCB0`, which separates the beak from the white face a little better. Both stay clearly pale on `#0A0E12` (see `head8.png`).
- **Feet.** There is now a solid 2 px foot `F` at (12,13)–(13,13), directly under the red belly at (12,12)–(13,12), and one claw pixel at (13,14) on the front of the perch. It no longer alternates with the perch. In row 12 the last scallop pixel (11,12) became R, so the belly wraps down to the foot and (11,13) R is edge-connected to it (in `v6a` it was a lone red corner pixel). The perch colour is unchanged: with a solid foot, the hue difference reads (`cmp7c.png`).
- **Wing edge.** The dark-red `r` line now starts at the back edge at the shoulder, (7,6), and runs at 45° through (8,7), (9,8), (10,9), (11,10) to (12,11) at the band, so the wing's upper outline is closed. The lone yellow pixel above the band (old (5,9)) is gone: the band's top row (row 9) is full from x5 to x9, and the row above is red.
- **Lint.** All rows are 18 wide and no pixel is isolated. The only single-colour pixels are the deliberate diagonals (`r`, `t`, `u`), the green scallop checker and the eye.
- `landmarks.json` is regenerated from the new sprite. It adds `wing_leading_edge` and `tail`, `perch_row` is 14, and `tail_tip` is `[0,23]`.
