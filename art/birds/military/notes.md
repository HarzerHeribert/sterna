# Military macaw (Ara militaris): 18 x 24 sprite

Perched and facing right, with the whole bird visible: green body, red forehead, pink face with an eye and a face line, a hooked slate beak, blue flight feathers, and a long red tail with a blue edge and blue tip. The perch is optional.

## Reference photos (Wikimedia Commons)

| use | file | author | licence |
|---|---|---|---|
| **primary: pose, silhouette, all landmarks, plumage colours** | [Macaw Mtn bird rehabilitation centre-Military Macaw (6849859148).jpg](https://commons.wikimedia.org/wiki/File:Macaw_Mtn_bird_rehabilitation_centre-Military_Macaw_(6849859148).jpg) (1280 px thumb) | Murray Foubister | CC BY-SA 2.0 |
| face, eye and beak colour in sunlight (photo is side-on, bird faces right) | [Military macaw at Cougar Mountain Zoological Park.jpg](https://commons.wikimedia.org/wiki/File:Military_macaw_at_Cougar_Mountain_Zoological_Park.jpg) | Dcoetzee | CC0 |
| check of the perched pose and tail angle | [Military Macaw (Ara militaris) RWD1.jpg](https://commons.wikimedia.org/wiki/File:Military_Macaw_(Ara_militaris)_RWD1.jpg) | Dick Daniels (carolinabirds.org) | CC BY-SA 3.0 |
| check of the full tail, from a bird facing left | [Ara militaris -London Zoo-8a.jpg](https://commons.wikimedia.org/wiki/File:Ara_militaris_-London_Zoo-8a.jpg) | neiljs (Flickr; uploaded by Snowmanradio) | CC BY 2.0 |

The primary photo was the only candidate out of about 60 that shows the bird side-on, perched, facing right, with the whole tail visible. The bird already faces right, so it was not mirrored. The other candidates are on contact sheets (`sheetA-D.jpg`, `capA/B.jpg`).

## Method

1. Cropped `ref.jpg` at 440x833+395+20 and scaled it to 150% (`crop.png`). Measured it on magenta 40 px grids (`grid_head.png`, `grid_body.png`, `grid_low.png`, `grid_tail.png`).
2. Traced 12 parts as Catmull-Rom landmark lists in `work/mac.py`: perch, tailblue, tailred, blue, body, wing, head, face, red, beak, mand, foot, plus the eye point. Overlaid the trace on the photo at 45% opacity (`work/overlay_side.png`, `work/overlay_head.png`) and adjusted it until it fit.
3. Rasterised the trace into 18x24 cells by 16x16 supersampled majority, with extra weight for small features (`work/raster.py`). Tried head scales of 1.0, 1.4, 1.5 and 1.7.
4. Finished the sprite by hand, through versions `work/v1..v7.txt`. The final version is v7 with a larger beak (`sprite.txt`).

**Measured landmarks in crop.png pixels:**
- Crown (530,15). Red forehead (559-620, 37-99). Eye (528,88). Face patch (487-567, 75-169).
- Upper beak (561-623, 76-195), hook tip (575,192). Lower mandible (521-567, 134-189).
- Nape (360,100). Breast front at x approx. 490 from y 250 to 420. Foot (405-466, 535-595).
- Green wing tip (85,585). Primary tips (38,862). Tail top (150-245, 760-790), red down to y 1110, blue tip (150,1250).
- Perch upper edge from (430,588) at a slope of 0.62.

## Choices

- **The head is drawn at about 1.5x its natural size.** It is 6 rows by 8 columns including the beak. At the natural scale, which fits the long tail into 24 rows, the head would be 3x5 px. The beak would then be 1x2 px, and the face and red forehead would be one pixel each. The body, wing, blue primaries and tail keep the photo's proportions. The tail (rows 16-23) is about as long as chin to vent (rows 5-15), as in the photo, and hangs with the same slight lean to the left.
- **Beak:** 8 px of upper beak (`K`) curving from x=14 at the top to the hook tip at (12,5), with a 2 px lower mandible (`k`) behind it. Both are slate, not black. `K` is #4F6275, bluish like the beak in sunlight. `k` is #3A4655, inside the #3A4450..#4B5663 range.
- **Eye:** 1 px, #1B1E24. It has pink face on its left, right and below, and green crown above, so it needs no highlight and never touches the background. The yellow iris cannot be shown at 1 px.
- **Face:** 4 pink-lilac px (#D8A8C4). One darker line pixel (`l`) sits at the lower front, where the photo's dark feather lines are densest.
- **Red forehead:** 3 px at the front top of the head, above the beak base. Its top is on the head outline, as in the photo.
- **Wing:** yellow-olive coverts (`W`) with one diagonal row of shading (`w`) that runs parallel to the blue band. The blue (`B`) runs as a diagonal band from the lower breast, past the foot, to a primary tip at (2,17). A darker edge (`u`) separates it from the tail.
- **Tail:** brick red (`T`), 2 px wide at the top. A lighter blue edge (`b`) runs on the left, and the last 2 rows are blue.
- **Perch:** a 2 px diagonal at a 2:1 slope (the photo's is 0.62), in `P` (top) and `p` (underside). If both letters are dropped, the bird stays one connected shape and stands on its foot `f` at (9,11).
- **Colours:** the photo was taken in shade behind wire mesh, so the raw samples are dull. For example, the red samples at #8C1829 (maximum 169,37,58), the face at #8A6BA0, the blue primaries at #063A6E..#044C8B, the tail at #7D242B, the wing at #8BA12A..#C5D314 and the head at #70B414. The palette keeps each hue and raises its lightness so it reads on #0A0E12, and it stays plain on #F4F6F8.
- **Checks:** no isolated pixels, one 4-connected silhouette, and all rows 18 wide. Pixels that are the only one of their colour are deliberate: the eye, the face line, the foot, and the diagonal shading lines `w`/`u`, which touch diagonally.

## Landmarks (sprite cells, [x, y], origin top-left)

- eye [11,2]
- upper beak: 8 cells from (13,2) to the tip (12,5), front at (14,2)
- lower mandible (11,4), (11,5)
- forehead (12,0), (12,1), (13,1)
- crown [10,0]; nape [6,2]
- foot [9,11]; primaries tip [2,17]
- tail base [4,16]; tail tip [4,23]

In half-block text, cell row y is on text line y//2: the top half when y is even, the bottom half when y is odd. The eye is on line 1, top half.

## Files

- `sprite.txt`, `palette.json`, `landmarks.json`, `preview.png`: the deliverables.
- `preview.png` shows, left to right: the sprite on #0A0E12, on #F4F6F8, as half-block text at JetBrains Mono 32 px (18 columns x 12 lines), and the reference photo.
- `work/`: trace, raster, palette, renderers and all intermediate versions.

## Revision 2 (after the critic's review)

This section supersedes the head size, landmark and palette details above. The previous sprite is kept as `work/v7_final_before_revise.txt` and its palette as `work/pal_v7.py`. Variants are in `work/v8A/B/C.txt` (heads) and `work/v8T1/T2/T3.txt` (tails). The chosen one is `work/v8.txt` = `sprite.txt`.

**What changed:**

- **Head is 5 rows instead of 6.** I merged the old crown rows 0-1 into one row (`........HHHHRR....`). The eye moves to (11,1), and everything below the head moves up one row. The head is now about 1.3x natural size instead of 1.5x. The red forehead is 2 px, at (12,0) and (13,0), on the front of the crown right above the beak. I rejected a variant with the red running down between the eye and the beak (`v8B`), because it read as a red stripe.
- **Beak hook, from the critic's fix:** the front is furthest forward at x=14 on rows 2-3 (mid-height), and the hook tip hangs back at (13,4) in front of the lower mandible. The top of the upper beak (13,1) rises from under the red forehead. The upper beak has 8 `K` cells and the lower mandible has 3 `k` cells at (11,3), (11,4) and (12,4).
- **Tail is longer, and 2+ cells wide to the end.** The tail runs from row 15 to row 23. It is `bTT` (3 wide) on rows 15-20, `bT` on row 21, and has a 2-cell blue tip `bb` on rows 22-23. That puts 7 rows past the primary tip (16), up from 6. Head, body and tail are now 21%, 50% and 29% of the height. I tried a one-column lean to the left in the lower tail (`v8T2`, `v8T3`), as in the photo, but it made a kink with a red block at the bottom, so the tail stays straight and tapers instead.
- **Hole closed:** (3,16), which was (3,17) before the shift, is now `b`. The outer tail stripe joins the primaries, and row 16 is `..ubTT...........p`.
- **Back outline dent filled:** (3,13), which was (3,14), is now `B`. The left edge runs x=3 on rows 10-14 and x=2 on row 15.
- **Edge contrast on #0A0E12:** `u` goes from #1D4F99 (2.43:1) to #2660B2 (3.13:1). `B` goes from #2A6CC4 to #2D70CA (3.95:1), which keeps the shading visible at B/u 1.26. `k` goes from #3A4655 (2.02:1) to #6E7686 (4.24:1). It is now a lighter neutral grey, sampled from the sunlit Cougar Mountain photo, where the lower mandible measures #5E687A..#808391 against the upper beak's #356287..#3E6696. `K` goes from #4F6275 to the bluer #4F6A88 (3.46:1). The two mandibles now differ in both value and hue (blue slate over neutral grey).
- **Back outline on #F4F6F8:** `W` goes from #8FB23A to #84A435 (2.25 to 2.64:1 on light). `w` goes from #6C9230 to #668A2C, so the covert shading stays visible (W/w 1.40).
- **Check:** no holes, no enclosed background, no isolated pixels, and one 4-connected bird without the perch. The only edge colour under 3:1 on dark is the optional perch shadow `p` (2.23:1). It always sits under `P` (3.60:1).

**Landmarks now (sprite cells, [x, y]):** eye [11,1]; upper beak (13,1), (12..14,2), (12..14,3), (13,4); beak front [14,2] (also 14,3); hook tip [13,4]; lower mandible (11,3), (11,4), (12,4); forehead (12,0), (13,0); face (10,1), (12,1), (10,2), (11,2); face line (10,3); crown [10,0]; nape [6,2]; breast [10,6]; foot [9,10]; primaries tip [2,16]; tail base [4,15]; tail tip [3,23] and [4,23]. The perch is the same diagonal, one row higher, from (9,11) to (17,16). In half-block text the eye is on line 0, bottom half.
