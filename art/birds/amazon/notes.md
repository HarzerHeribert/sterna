# Blue-fronted Amazon (Amazona aestiva): 18 x 24 sprite

Perched side-on, facing right. Traced from a real photo and then finished by hand.

## Photos (Wikimedia Commons)

| use | file | author | licence |
|---|---|---|---|
| **pose, proportions and all landmarks; most colours** (`photos/p02.jpg`) | https://commons.wikimedia.org/wiki/File:Turquoise-fronted_amazon_(Amazona_aestiva)_older_adult.JPG | Charles J. Sharp | CC BY-SA 4.0 |
| cross-check of the same pose (same bird, 13 s earlier) (`photos/p04.jpg`) | https://commons.wikimedia.org/wiki/File:Turquoise-fronted_amazon_(Amazona_aestiva)_Rio_Negro.jpg | Charles J. Sharp | CC BY-SA 4.0 |
| blue forehead colour (p02's blue is washed out by sunlight) (`photos/p11.jpg`) | https://commons.wikimedia.org/wiki/File:Amazona_aestiva_-upper_body-8a.jpg | Gilberto Santa Rosa | CC BY 2.0 |

The contact sheets `sheet1-3.jpg` list the other 24 candidates. `p29` was rejected because its pale beak looks like an Orange-winged Amazon.

## Method

1. Crop: `p02.jpg -crop 620x820+116+252 +repage -resize 775x1025!` (1.25x) → `crop.png`, with the grid in `grid_mine.png`. The zoomed crops are `head_zoom2.png`, `belly_zoom.png` and `tail_zoom.png`.
2. The traced landmark lists are in `work/trace.py` (Catmull-Rom). The fit on the photo is in `work/fit.png`.
3. Rasterised in Chrome with crispEdges to 18 x 24. The viewBox is `12 24 734.4 979.2`, so one sprite px is 40.8 crop px. The raw letters are in `work/raw_raster.txt`.
4. Finished by hand as row spans in `work/build.py`. Then the lower wing was split into coverts and flight feathers, and a blue pixel was added at (17,1). `work/ov2.png` overlays the sprite grid on the photo, and the eye, beak, red band, wing point and tail tip all land on their photo features.

The bird's bounding box in the photo is 715 x 970 px, which is 0.74. That matches 18:24 almost exactly, so the bird fills the grid.

## Landmarks (crop px → sprite px)

- eye centre (621,82) → **(14,1)**, with yellow on all four sides
- crown top (600,30) → (14,0). The front of the crown is yellow in this species, and the green crown is at (13,0).
- blue forehead (655–718, 36–96) → (16,0), (16,1) and (17,1)
- beak: the culmen is at (712,112), the front at (740,152), the hook tip at (705,224) and the gape at (645,180). These give the upper mandible K/k at (16,2), (17,2), (17,3) and the hook (17,4), and the lower mandible J at (15,3) and (16,3).
- red band (442,522)→(310,648) → a 1:1 diagonal at (10,12), (9,13), (8,14) and (7,15)
- wing point (dark primaries) (100–130, 720–835) → column 2, rows 17–19
- tail tip (22,1012) → **(0,23)**. The tail is 3 px wide and runs at about 55°, the photo's angle.
- lower foot (420–575, 768–840) → legs at (10,18) and (11,18), toes wrapping the perch at (9,19) and (12,19)

## Choices

- **Red patch.** In this photo the red sits on the secondaries (the speculum) along the wing's front and lower edge, next to the yellow-green flank. That is where it is drawn, and it is the red the brief calls the shoulder patch. The carpal edge of this bird shows only a few yellow flecks, which would be noise at this size.
- **Perch.** The photo's perch is a vertical stump. The sprite uses a 1 px horizontal branch across the full width, with the tail hanging in front of it, which is the mockup's convention. `P` is its own letter, so the app can map it to transparent.
- **Omitted:** a single displaced tail feather that hangs loose in the photo (215–260, 725–970). It is this moment of this bird, not the species.
- **Nothing black on black.** The photo's near-black primary tips (#191517) are drawn as slate `D` #3E4A57. The beak (sampled #383A47) is `K` #4B5663, with the lower mandible `J` #3A4450 and the pale base `k` #7A8591.
- **Colours** are medians of small patches of the photo. The greens are nudged slightly toward green: the photo's means are olive (coverts #616F3F), and against blue sky they read green. Without the sky they read brown on #0A0E12. Sampled → used:
  - neck #8AAE5D → G #80AA52
  - belly #A1B255 → L #A4BA52
  - coverts #616F3F → W #5F813B
  - flight feathers #4C6239 → V #4C6C31
  - tail #547A35 → T #568034, and its tip #749443 → t #86A644
  - red #C4452E → R #C8452E
  - yellow #EBB709 → Y #F0C21A
  - blue (p11) #7EC3C5 → F #6DB9C9
  - perch stump #B09983 → P #A89A8A
- **Eye:** one pixel, #5A2C1A. That is the iris and pupil read together as dark orange-brown. It is fully surrounded by yellow, so it needs no highlight.
- **Checks:** no isolated pixels (4-neighbour check), all rows are 18 wide and there are 24 rows. Every letter in the sprite has a palette entry and a region.

## Files

- `sprite.txt`, `palette.json` and `landmarks.json` are the deliverables.
- `preview.png` shows, left to right: 14x on #0A0E12, 14x on #F4F6F8, half-block text (24 px and 12 px, JetBrains Mono, line-height 1) on dark and on light, and the reference crop.
- `work/` holds the tracer, the rasteriser, the variants (`sprite_v1..vC`) and the renders.

## Revision 2 (after the critic's review)

Where this section disagrees with the sections above, this section is correct. The previous deliverables are kept as `work/sprite_r1_before.txt`, `work/pal_r1_before.json`, `work/landmarks_r1_before.json` and `work/preview_r1_before.png`.

1. **Wing point.** (2,17) and (2,18) were a 1-px grey bar that ran through the perch and read as a post. They are now `D` **#3B5530**, a dark green that continues the wing (2.33:1 on #0A0E12; 1.38:1 against `V`, so the primaries still separate from the flight feathers). (2,19) is now `T`, so the perch is no longer cut by grey and the tail hangs in front of it. `D` has no grey pixels left, and its region now names the folded primaries.
2. **Forehead step.** (17,1) `F` → `.`. The culmen at (17,2) now sticks out one pixel past the forehead, which gives the parrot step at the cere. The head reads as round with a hooked bill, not as a box with a visor. The blue stays at (16,0) and (16,1), and at 12 px half-block it is still a solid teal cell.
3. **Tail tip.** Row 22 is `ttt` in columns 0–2, not `.tt`. The tail is now 3 px wide on a clean 45° diagonal through rows 20–22 and ends in a 2-px square tip at row 23. Above that, the tail keeps the photo's angle of about 55°.
4. **Vent.** (7,17) `L` → `.`. The belly curves off at columns 8–10, and a clean 2-px gap sits between the belly and the tail at (6,17) and (7,17).
5. **Lower mandible.** `J` #3A4450 → **#444E5B** (2.29:1 on dark, 1.13:1 against `K`). (16,4) stays empty, so the hook at (17,4) still reads.
6. **Red shoulder (brief conformance).** I took the critic's second option and added `R` at the bend of the wing, **(13,6) and (13,7)**. There are three reasons:
   - The brief asks for a red shoulder.
   - In the nominate race *A. a. aestiva* the bend of the wing is red. The photo's bird shows yellow flecks there, which is the *xanthopteryx* pattern.
   - The photo overlay (`work/ov3.png`) puts those two cells exactly on the carpal edge where the photo's coloured flecks are.

   The speculum diagonal (10,12)→(7,15) is kept, because it is accurate to the photo and is the species' other red mark. At 12 px half-block, the shoulder is one solid red cell high on the wing, so it reads as a shoulder and not as a flank stripe.
   - **Alternative if the orchestrator prefers the photo as it is:** `work/sprite_r2A.txt` is this same revision without the two shoulder pixels (use it with the same palette), and it is rendered in the top row of `work/pAB.png`. The rest of the revision is identical in both.

Nothing else changed. Head size, eye, blue position, breast line, back line, feet and the perch row are as before.

**Checks.** The sprite has 24 rows of 18. There are no 4-neighbour-isolated pixels, and every letter has a palette entry and a region.

**Landmarks changed:**
- `forehead` is now (16,0) and (16,1).
- `red_patch` is now the shoulder, (13,6) and (13,7).
- The new `speculum` key holds the diagonal.
- `wing_tip` is now (2,18), because (2,19) is tail now.

`eye` (14,1), `beak_tip` (17,4), `crown` (14,0) and `tail_tip` (0,23) are unchanged.

**Renders:**
- `preview.png` has the same layout as before.
- `work/pAB.png` compares variant A (the photo only) with variant B (shipped).
- `work/z28_ov3.png` shows a 28x zoom beside the overlay on the photo.
