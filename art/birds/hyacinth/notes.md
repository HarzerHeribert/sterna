# Hyacinth macaw (*Anodorhynchus hyacinthinus*): 18 x 24 sprite

Perched, facing right. The head is at the top right, the body leans forward, the tail runs down-left
to the bottom-left corner (0,23), and the bird grips a broken branch that runs off to the lower right.

## Reference photos (Wikimedia Commons)

| use | file | author | licence |
|---|---|---|---|
| **pose and trace** (mirrored so the bird faces right) | [Anodorhynchus hyacinthinus -Mato Grosso -Brazil-8b.jpg](https://commons.wikimedia.org/wiki/File:Anodorhynchus_hyacinthinus_-Mato_Grosso_-Brazil-8b.jpg) | Nori Almeida (Flickr "Pantanal 2009") | CC BY 2.0 |
| colour of sunlit plumage, beak and lappet | [Hyacinth Macaw (Anodorhynchus hyacinthinus) (31676594802).jpg](https://commons.wikimedia.org/wiki/File:Hyacinth_Macaw_(Anodorhynchus_hyacinthinus)_(31676594802).jpg) | Bernard DUPONT from FRANCE | CC BY-SA 2.0 |
| face close-up (eye ring, lappet colour) | [Arara Azul no Pantanal.jpg](https://commons.wikimedia.org/wiki/File:Arara_Azul_no_Pantanal.jpg) | Leonardo Ramos | CC BY-SA 4.0 |
| second opinion on the diagonal pose (not traced) | [Anodorhynchus hyacinthinus -Brazilian Pantanal-8.jpg](https://commons.wikimedia.org/wiki/File:Anodorhynchus_hyacinthinus_-Brazilian_Pantanal-8.jpg) | Alexander Yates | CC BY 2.0 |

I chose the traced photo because it is a clean side-on profile: eye ring, lappet and hooked beak are
all visible, it shows a real perch, and its diagonal body and tail fill an 18 x 24 box better than an
upright bird. An upright pose would give a smaller head at this grid size.

## Method
1. `crop.png` = `ref_pose.jpg` cropped 460x510+90+30, flopped, scaled x2. `grid.png` is that crop with a 40 px grid.
2. `trace.py` holds landmark lists for each part (branch, tail, under, body, head, wing, feet, beak,
   lappet, ring, eye), drawn through Catmull-Rom curves. `fit.png` shows the overlay on the photo at 45% opacity, with the flat render beside it.
3. `raster.py` rasterises at 16 px per cell and takes each cell by majority vote, with priority for small
   features. The body fits best at 38 crop px per cell (x0=151, y0=-37). The tail's thin split tip
   (about 4 cells) falls off the left edge, so the tail runs into the bottom-left corner.
4. Hand-finish:
   - The head is redrawn at about 30 px per cell, 1.27x the body. At 38 px the head is 5 px wide, too small to keep the eye ring and the lappet apart.
   - The crown is one row above the eye, so the eye sits inside the head. In the photo the ring nearly touches the crown outline.
   - The upper mandible is a 1-px arc of light slate (K) that ends in a hook tip at (16,6). The darker lower mandible (k) fills the inside, so the hook reads.
   - The lappet is a 3-px diagonal crescent from the gape down and back: (15,3), (14,4), (13,5). It is separated from the eye by blue.
   - The eye ring is the yellow pixel behind the eye (R at (13,2), E at (14,2)). I tried three other layouts at shipping size: an L around the eye looked like a yellow brow, and R-E-R merged with the lappet into one blob.
   - The branch is thinned to a 2-px diagonal with the broken stub in front of the wing, as in the photo. At full thickness it filled the lower right.
   - The breast pulls in at (16,7), so the hook tip does not merge into the breast.
   - The tail is 3 px wide at the base and tapers to 1 px. It is two-toned: bright upper surface (T) and dark underside (U). Its slope is about 0.6 columns per row, close to the photo's 0.7, so it does not look steep or thin.
5. Checks: every row is 18 wide and there are 24 rows. No pixel is isolated. Every region is one 8-connected piece. The only single-pixel regions are the eye and the ring.

## Palette (sampled, then adjusted for a terminal)
Colours were sampled with `magick ... -resize 1x1` patches and with per-part mask medians. The traced
photo is in shade, so the blues come from blue-filtered percentiles across the sunlit shots. Plumage
median is about #28457F, lit about #4F63C6, head about #446191/#4A6495. I kept the hue at about 222 deg,
cobalt rather than periwinkle, and raised the values so the plumage reads on #0A0E12. The head is kept
only slightly paler than the body, because a strongly paler head read as a helmet.
- Lappet #F5C400 (photo #F6C601, #FAC401). Eye ring #F7CB1C (photo #F9CD13, #F8E43A).
- Beak: slate #5A606B for the upper mandible and #3C424B for the lower (photo #4B4343 to #626260).
- Feet: slate #4B5058, following the no-black-on-black rule; the photo feet are about #212022.
- Branch #8B6236 (photo #8E612B).
- The pupil #1C2026 is the only near-black. It never touches the background: yellow is on one side and blue on the other three.
- The tail underside #27367F is the darkest blue that stays visible on #0A0E12. It keeps the tail two pixels wide on a dark terminal.

## Landmarks (sprite coordinates, x = column, y = row)
- eye (14,2)
- eye ring (13,2)
- crown (14,1)
- beak: upper mandible (16,2), (16,3), (17,3), (17,4), (17,5) and hook tip (16,6); lower mandible (15,4), (16,4), (14,5), (15,5), (16,5)
- lappet (15,3), (14,4), (13,5)
- feet (13,11), (14,11)
- tail tip (0,23)

`landmarks.json` holds the same points, split into `beak_upper` and `beak_lower` so a mood can open the beak.
Row 0 is left empty as headroom for mood effects.

## Files
- `sprite.txt`, `palette.json` (with `_regions`), `landmarks.json`, `preview.png`: the deliverables.
- `trace.py`, `raster.py`, `spr.py` (renderer), `fit.png`, `grid.png`, `crop.png`, `headref.png`, `ref_*.jpg`, `cand/`: the working material.
- `work/` holds the intermediate drafts (v1 to v8, the automatic rasters g36/g38/g40 and the head rasters h26 to h32).

## Revision 2 (2026-09-26): the critic's fixes

The previous deliverables are kept in `work/v8_before_revision/`. The variants and checks are in `rev/`:
`A-E.txt`, `check.py` (isolation and connectivity), `cmp.py` (side-by-side render), `topam.py` and
`ovl.png` (the old and new sprite at 55% over the mirrored photo). `preview.py` rebuilds `preview.png`.

- **Tail angle.** The old left edge in rows 15-23 ran 4,4,3,3,2,2,1,1,0: 0.5 columns per row, about 63
  degrees, and steeper toward the tip. The earlier note's "0.6 cols/row" was an average over rows 12-15,
  not the tail you see. The tail top now starts one column further right, and the edge steps two columns
  every three rows: 6,5,5,4,3,3,2,2,1,0 over rows 14-23. That is about 0.67 columns per row, about 56
  degrees, against the photo's straight 0.69 from (265,255) to the tip (72,535). In `rev/ovl.png` the new
  band lies on the photo's tail. The old one bent to the left of it.
- **Tail thickness on dark.** U (#27367F) has only 1.77:1 contrast on #0A0E12, so on a dark terminal the
  old T,U pairs showed as a 1-px staircase. Rows 18-23 are now solid T, 2 px wide (`TTT`, `TT`, `TT`, `TT`,
  `TT`, then a 1-px tip at (0,23)). U remains only as the undertail in rows 12-17. I did not brighten U,
  because at 2.11:1 it would still be dim and it would merge with T.
- **Head size and nape.** One column is off the back of the head: rows 2-3 start at col 11 and rows 4-5
  at col 10. Row 6 is `........WWHHHHHBK.`, with (9,6) as the W shoulder. The back of the head now curves
  (12,11,11,10,10), and then the shoulder steps out two columns at row 6. The old sprite had one straight
  wedge from the crown to the tail tip. The head is still larger than in the photo (6 rows × 8 columns).
  It has to be, to keep the eye, ring and crescent apart.
- **Eye and lappet.** The eye moved back a column: R (12,2), E (13,2), H (14,2). The pupil no longer
  touches the lappet at a corner, and a full blue row (row 3, cols 13-14) separates them. The lappet is now
  a 4-px crescent: (15,3), (14,4), (13,4), (13,5). It is 2 px thick in the middle, from the gape down and
  back, like the photo's band. The pupil and every Y pixel are enclosed by head, beak or lappet on all
  eight sides.
  - Deviation: row 1 is `............HHHH..` (cols 12-15), not the suggested cols 11-15. With (11,1)
    filled, the crown became a flat-topped square block. With it empty, the crown reads round, as in the
    photo. The ring R at (12,2) still has blue on all four sides and meets the transparent area only at
    one corner.
- **Branch.** It is 2 px thick and runs down from under the feet at about 0.8 columns per row (about 51
  degrees; the photo's branch measures 53-59): rows 12-17 at cols 12-14, 14-15, 15-16, 15-16, 16-17, 17.
  It leaves the right edge at row 17 as a diagonal, so the old L-shaped hook at (17,15) is gone. The stub in
  front of the wing, (10,10) and (10-12,11), is kept. A steeper variant (rows 13-18, `rev/D.txt`) ended in a
  2-px vertical run down the frame edge, which read as a hook again.
- **Beak luminance.** K is now #6A707B, up from #5A606B. Its contrast is 1.31:1 against the head blue
  (was 1.03), 2.03:1 against the lower mandible k (was 1.60), 3.89:1 on #0A0E12 and 4.60:1 on #F4F6F8.
  I did not darken (16,2) to k, because k shows only 1.9:1 on the dark background, and (16,2) is on the
  beak's outline.

Checks: 24 rows × 18 columns. There is one opaque 8-connected piece and every letter is one 8-connected
region. The only single-pixel regions are the pupil E and the ring R. Pixels with no same-letter
4-neighbour are all deliberate diagonal features:
- the crescent tip (15,3)
- the hook tip (16,6)
- the undertail edge (7,16) and (6,17)
- the wing-tip overlap (5,15)
- the tail tip (0,23)

Nothing black touches the background. The pupil #1C2026 and the lower mandible k are fully enclosed.

Updated landmarks: eye (13,2), eye ring (12,2), crown (13,1), nape (10,4), lappet (15,3) (14,4) (13,4)
(13,5), beak unchanged (upper (16,2) (16,3) (17,3) (17,4) (17,5) and hook tip (16,6); lower (15,4) (16,4)
(14,5) (15,5) (16,5)), feet (13,11) (14,11), tail tip (0,23).
