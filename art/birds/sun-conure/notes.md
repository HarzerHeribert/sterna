# Sun conure (Aratinga solstitialis): 18 x 24 sprite, perched, facing right

## Photos (Wikimedia Commons)
- **Traced**: File:Aratinga-solstitialis.jpg, by Penkinvaltaaja, CC BY-SA 4.0.
  https://commons.wikimedia.org/wiki/File:Aratinga-solstitialis.jpg (960 px thumb saved as `ref.jpg`).
  Side-on, perched, already facing right, so no mirroring was needed.
- **Colour and feature checks**: File:Aratinga solstitialis - Loro Parque 01.jpg, by H. Zell, CC BY-SA 3.0.
  https://commons.wikimedia.org/wiki/File:Aratinga_solstitialis_-_Loro_Parque_01.jpg
  (whole tail with its dark blue tip, red-orange face).
- **Colour and feature checks**: File:Aratinga solstitialis on perch.jpg, by Sarah G from Tulsa, USA, CC BY-SA 2.0.
  https://commons.wikimedia.org/wiki/File:Aratinga_solstitialis_on_perch.jpg
  (clean white eye ring, green wing with blue edges).

Contact sheets: `sheet1.jpg`, `sheet2.jpg`. Measuring grid: `grid.png`, `grid_head.png`, `grid_mid.png`, `grid_low.png`.
Trace overlay: `fit.png`. Sprite over the photo: `sfit.png`.

## Mapping
The sprite pixel (c, r) covers photo x = 20.5 + 51c and y = 40.5 + 51r on the 960 x 1217 thumb, at 51 photo px per sprite px.
The offsets put the eye at the centre of pixel (14, 2). Landmark lists and the Catmull-Rom tracer are in `trace.py`, the
crispEdges rasteriser is in `raster.py` (its raw output is `raw.txt`), and the renderers are `preview.py` and `compare.py`.

## Landmarks: photo px → sprite px
| part | photo | sprite |
|---|---|---|
| crown top | (650, 88) | (12, 1) |
| eye | (760, 168) | (14, 2) |
| forehead front | (860, 215) | (16, 3) |
| beak: culmen, front, hook tip | (838, 229), (856, 270), (778, 350) | (14–16, 4), hook (16, 5) |
| throat | (705, 358) | (13, 6) |
| breast front | (650, 450) → (572, 632) | col 12 on rows 6–10 → col 10 on row 12 |
| wing shoulder / tip | (560, 140) / (62, 992) | (10, 2–4) / (1, 18) |
| yellow → green → blue on the wing | y ≈ 455 / y ≈ 590–735 | rows 8–9 / rows 11–13 |
| feet | (420–648, 690–745) | (8–9, 13), (11–12, 13), talons (9, 14), (12, 14) |
| tail | (300, 812) → (130, 1095) | (5, 15) → (2, 20), 2 px wide, one column left every two rows |
| tail tip | extrapolated to (122, 1172) | (2, 22) |

## Choices
- **Perch.** The branch in the photo is a steep diagonal. The sprite uses a horizontal two-row perch on rows 14–15 (P, then the
  shade p), which is one text line in half-block, across the full width as in the mockup's `Bird.dc.html`. Both feet were moved
  onto it; the far foot is at the photo's height, the near foot moved up about 50 px.
- **Tail.** In the traced photo the tail is cut off by a log at y ≈ 1100. It was extended about 1.5 sprite px along the same line,
  with a two-pixel blue tip taken from the Loro Parque photo. It stays two pixels wide down to the one-pixel tip, at the photo's
  angle (about 30° from vertical, close to the body axis), so it is not thin.
- **Eye.** A 1 px eye at (14, 2) with a two-pixel off-white ring behind and below it. The ring is grey in the traced photo and
  clean white in the perch photo, and the ring is what identifies the species. A full ring (R E R) was tried and read as cartoonish.
- **Beak.** Three pixels across row 4 plus the hook hanging at the front, (16, 5). Variants tried: `KKK/KK.` looks like a block,
  `KKK/.K.` looks like a T, and `KKK/K.K` looks like an open mouth.
- **Colours.** Region medians and spot samples came from the photos (see `palette.json`). Two were lifted for the #0A0E12
  ground. The primaries measure navy #14162F, which would be black on black, so they became #34529A. The beak measures
  #3F394C, so it became slate #474C5A. The lit green comes from the perch photo (#5EA310), toned down to #4A8F24. The perch
  is weathered pale grey in the photo (#A9B0B5) and would vanish on #F4F6F8, so it is a mid warm grey, #9C9186 over #6E655C.
- **Stray pixels.** A single orange vent pixel at (7, 13) became wing blue, and a lone red-orange crown pixel at (14, 1) became
  crown orange. The only remaining one-pixel regions are the eye and its ring, which is on purpose.

## Files
`sprite.txt` (24 rows of 18), `palette.json`, `landmarks.json` (sprite coordinates, x then y, origin at the top left),
`preview.png` (dark, light, half-block on dark and on light at 22 and 12 px, and the reference photo).

## Revision 2 (critic pass, 2026-09-26)
Coordinates are (col, row). The previous files are kept in `rev2/` (`sprite_v1.txt`, `palette_v1.json`, `landmarks_v1.json`,
`preview_v1.png`). Items 1-4 were applied exactly as in `critic/proposal.txt` (`rev2/base.txt` diffs clean against it). The
variants tried are in `rev2/v1.png` and `rev2/v2.png`, and the new sprite over the photo is in `rev2/sfit.png` (crops in `rev2/sfit_crops.png`).
- **Beak** is now `KLL / kK. / .K.` on rows 4-6: (14,5) is the lower mandible `k`, (15,5) is `K`, (16,5) is cleared, and (15,6) is
  `K`, the hook tip below and behind the bill front, where the photo has it (778, 350). The overlay puts every beak cell on the bill.
- **Beak tones.** The critic suggested #33343C for `k`. It measures 1.57:1 on #0A0E12, which breaks the no-black-on-black rule,
  so the beak family moved up together. The tones are `L` #6E7486 culmen sheen on (15,4) and (16,4) (4.15:1 on dark), `K` #4E5464
  (2.56:1, was #474C5A at 2.26:1) and `k` #3A4150 (1.89:1, in the dark-slate range). `k` stays visibly darker than `K`, which
  is what separates the two mandibles.
- **Head.** (12,2) and (12,3) are now `C`. (11,3), (11,4), (12,4), (11,5), (12,5) and (13,5) are now `F`. `H` is only (11,1),
  (10,2) and (11,2). The red-orange ear patch now sits behind and above the eye and joins the forehead mask in front of it.
  Making (12,1) and (13,1) `C` as well was tried (variants E and F), but the traced photo samples orange there (#FDAA46,
  #FC933E), and the species has a golden-orange crown over an orange-red face, so the crown stays `F`.
- **Belly / wing edge.** (10,10), (9,11) and (8,12) are now `B`, so the belly is 3 px wide on rows 10-12. That change left
  (8,11) `U` as a lone pixel, so (7,11) became `U` as well (the photo shows navy primaries there). The primaries now run
  (7-8,11) → (6-7,12) → (3-7,13) without a gap.
- **Far foot.** (8,12) is now orange above the foot at (8,13), so the half-block cell is orange over grey and the foot reads.
  `G` is unchanged.
- **Tail.** `T` moved from #8A8433 (khaki) to #859A30, the yellow-olive green of the Loro Parque photo. Its shape is unchanged.
- **Mantle rim.** A new `Y` #F0C21E marks the back silhouette: (8,2), (9,2), (6,3), (7,3), (5,4), (4,5), (4,6), (3,7), (3,8) and (3,9).
  On #F4F6F8 that is 1.56:1 (`S` alone was 1.32:1). Deepening all of `S` (variant Y2) merged the mantle into the nape, so only the rim changed.
- **Single-pixel check.** The only 4-isolated pixels are the eye, its two ring pixels, (5,4) in the diagonal rim line and the
  beak's own tones (K at (14,4), k at (14,5)). The beak is one connected shape.
