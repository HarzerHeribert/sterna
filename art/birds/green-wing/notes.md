# Red-and-green macaw (Ara chloropterus), 18 x 24, perched, facing right

## Photos (Wikimedia Commons)
- **Shape reference (traced):** "Green-winged Macaw (Ara chloroptera) -Maine -zoo.jpg", Peter Dutton, CC BY 2.0.
  https://commons.wikimedia.org/wiki/File:Green-winged_Macaw_(Ara_chloroptera)_-Maine_-zoo.jpg
  Side-on on a pipe perch, wing folded, the whole tail visible. Mirrored so the bird faces right.
- **Colour reference (well lit):** "Green-winged macaw at Cougar Mountain Zoological Park.jpg", Dcoetzee, CC0.
  https://commons.wikimedia.org/wiki/File:Green-winged_macaw_at_Cougar_Mountain_Zoological_Park.jpg
- On the contact sheet but not used: "Red-and-Green Macaw ... York's Wild Kingdom York Maine.jpg" (EgorovaSvetlana,
  CC BY-SA 4.0), which faces right but its tail hangs straight down behind the perch, and
  "Ara chloropterus -perching on top of cage-8.jpg" (Lee, CC BY-SA 2.0), which is head only.

## Method
960 px thumbnail, crop 600x880+300+420, flopped (`base.png`). Traced 13 parts as Catmull-Rom landmark lists in
`parts.py`: tail, body, far wing, wing, shoulder, green coverts, face, lower mandible, upper mandible, eye, perch,
two feet. `fit.png` shows the overlay at 45%. `parts2.py` makes two pose changes, then `raster.py` renders at
10x in crispEdges and takes the majority colour in each cell (`r2.txt`). The sprite is hand-finished from that
(`h1`..`h7`; `sprite.txt` = `h7`).

Mapping: sprite = (base - (34, 32)) * 0.0311. Crown top is row 0 and tail tip is row 23.

## Deliberate departures from the photo
1. **Head drawn 1.3x larger** (scaled about the crown). At true scale the head is about 4 px, so the white face
   is 2 px and the eye does not survive. At 1.3x the face is a 3x3 patch with the eye inside it, and the beak is
   2x4 with a hook.
2. **Tail rotated 8 deg toward horizontal** (about 54 deg becomes about 46-50 deg). This avoids the steep, thin
   look the user disliked on the tern. The tail is 4 px wide for most of its length (lit `T` above, shaded `t`
   below), tapering to 1 px. Its length is unchanged.
3. **Perch** is a neutral wood colour, not the photo's white PVC pipe. It is its own letter `P`, so it can be
   dropped.
4. The far wing's blue edge shows behind the breast in the photo. It was dropped because at 1 px it read as a
   blue stripe on the chest.

## Palette (sampled, then adjusted for #0A0E12)
- R `#B8292A`: breast, p75-p90 of the red cluster in the lit photo (#A42A26..#BA3A30).
- r `#7E1F20`: red p25 (#7C1F1F), the dark shoulder edge.
- T `#9A2524` / t `#6E1C1E`: the tail is darker than the body in both photos (#8C2322 lit, #702F21 shade).
- G `#6E9431`: coverts measured #668A1C..#70812F, lifted slightly.
- B `#2A8FA8`: flight feathers measured #1D93A0.
- W `#E8E3DE`: face, a warm white (p25 of the white cluster, #DBDDDE).
- L `#DDA29C`: one pixel of the red feather lines under the eye. A white/red blend, so it reads as lines and not
  as a spot.
- U `#D6CCB8`: upper mandible (#CAC4BF).
- K `#46505C`: the lower mandible is black in life (#30342E). It is slate here per the no-black-on-black rule.
- E `#343D48`: eye. The darkest pixel, but not black, and it always sits inside the white face.
- F `#6E757D`: feet (#555C63, lifted).
- P `#7A5B3E`: perch.

## Landmarks (sprite coords [x, y]; also in landmarks.json)
- eye [13,2]
- crown [13,0]
- upper mandible [15..16,2], [15..16,3], [15,4], [15,5]
- beak_tip [15,5]
- lower mandible [14,3], [14,4]
- face [13,1] [14,1] [12,2] [14,2] [12,3] [13,3] [13,4]
- shoulder [10,3]
- wing_tip [8,14]
- feet [12,10] [14,10] [12,11] [14,11]
- perch row 11, x 8..16
- tail_tip [1,23]

## Checks
- All rows are 18 wide; there are 24 rows.
- No floating pixels. Four things are deliberately single pixels:
  - the eye `E`
  - the face-line pixel `L`
  - the perch pixel `P` between the feet
  - the shaded tail edge `t`, a continuous 45 deg diagonal
- Checked at x14 on #0A0E12 and #F4F6F8, and as `▀` half-blocks in JetBrains Mono at line-height 1 (26 px and
  14 px). See preview.png.

## Revision 2 (critic pass), 2026-09-26
The previous files are kept in `rev/` (`sprite_v1.txt`, `palette_v1.json`, `landmarks_v1.json`, `preview_v1.png`,
`notes_v1.md`).

1. **Tail tip.** The tip no longer steepens. Rows 20-22 are now `.TTTt`, `TTt`, `Tt` and row 23 is empty. The
   left edge moves one column per row from row 14 (x7) to row 21 (x0), so the 45 deg line runs to the point.
   The old 3-px vertical stub at x1 is gone. I tried a blunter tip, `TTtt` on row 21, and rejected it (`rev/cmpT.png`)
   because it ends in a flat dark bar. The shaded edge steps 2 px between rows 20 and 21, which is where the
   tip narrows.
   tail_tip is now [0,22].
2. **Eye and lower mandible separated.** [14,3] changed from K to W, and [14,5] changed from R to K. The eye
   [13,2] now has white on every side: W above, below and to both sides. No dark pixel touches it, even at a
   corner. The lower mandible is K at [14,4] and [14,5], under the white face and behind the pale hook [15,4..5].
   lower_mandible and face in landmarks.json are updated to match.
3. **Tail contrast.** T changed from `#9A2524` to `#A62726`, which is 2.71:1 on #0A0E12 (was 2.46). t changed from
   `#6E1C1E` to `#862222`, which is 2.09:1 (was 1.70). The shaded lower edge now shows on dark, so the tail reads
   at its drawn 4 px width. The tail is still darker than body R `#B8292A` (3.12:1), as it is in the photo.
4. **Beak outline on light.** U changed from `#D6CCB8` to `#C9B99C`. That is 1.78:1 on #F4F6F8 (was 1.47) and
   10.05:1 on #0A0E12. U is 4.26:1 against K.
5. **Face-line pixel (optional fix, taken).** The pink L pixel at [12,3] read as a cockatiel-style cheek blush,
   so it is now plain R. The red feather lines are densest there in the photo (`rev/headref.png`), just behind
   and below the eye. At this size, showing them as red biting into the lower rear of the white face is plainer
   than using a pink. L is dropped from the palette. `rev/cmpL.png` compares the three options: keep L, make it W,
   make it R. W gave a clean bare face, which reads as a scarlet macaw.

Checks: 18x24, all rows equal. Only two single-letter pixels remain: the eye E and the perch P between the feet.
No pixel floats. The preview is at x14 on dark and light, as ▀ half-blocks at 26 px and at 14 px. Every letter
in the sprite is in the palette, and the palette has no unused letters.
