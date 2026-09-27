# Arctic tern (Sterna paradisaea), perched on a post, facing right. 24 x 24 px

## Reference photos (Wikimedia Commons)
- **Pose and proportions**, the traced one: "Arctic Tern (Sterna paradisaea).jpg" by Billy Lindblom, CC BY 2.0 (originally on Flickr), 2009-09-07.
  https://commons.wikimedia.org/wiki/File:Arctic_Tern_(Sterna_paradisaea).jpg
  The bird stands side-on on a rock facing left; the crop was mirrored (`ref_c05_flop.jpg`, `crop.png`).
- **Head, cap edge, eye position and neutral colours**: "Arctic tern close-up (51101372759).jpg" by USFWS Alaska, public domain, 2018-06-16.
  https://commons.wikimedia.org/wiki/File:Arctic_tern_close-up_(51101372759).jpg
- **Overcast colours and a post perch**: "Arctic Tern (Sterna paradisaea), Norwick - geograph.org.uk - 5862264.jpg" by Mike Pennington, CC BY-SA 2.0 (Geograph), 2018-08-02.
  https://commons.wikimedia.org/wiki/File:Arctic_Tern_(Sterna_paradisaea),_Norwick_-_geograph.org.uk_-_5862264.jpg
- **Bill and leg red in sunlight**: "Arctic Tern on Beach.jpg" by Lucas Golden, CC BY 4.0, 2022-07-16.
  https://commons.wikimedia.org/wiki/File:Arctic_Tern_on_Beach.jpg

## Method
1. Picked the photo from a 23-photo contact sheet (`cand/sheetA.jpg`, `cand/sheetB.jpg`). It was the only side-on, closed-bill, standing bird that shows the whole tail.
2. Measured landmarks on `grid.png` and on the zoomed grids `zhead.png`, `zbody.png` and `ztail.png`. The units are crop.png pixels, with the bird facing right.
   Bill tip (930,168), gape (842,146), eye (822,124), crown (795,63), nape (722,150), breast front (893,250),
   feet (735-810, 432-469), wingtip (125,412), near streamer tip (90,483), far streamer tip (180,515).
3. Traced the parts in `trace.py` (Catmull-Rom). The overlay on the photo at 45% (`fit.png`) fits.
4. Rasterised with 16x supersampling and majority vote (`covmap.py`). At true proportions the bird is 24 x 12 px: the head is 4 px and the bill 2.5 px, too small to read.
   Hand-finished from the k=1.3 coverage map (`cmp.py` overlays the sprite on the photo):
   - The **body silhouette follows the photo**: upright breast, back falling steeply from the nape, long rear.
   - The **head is enlarged about 1.5x** so the cap, eye and dagger bill stay legible. The breast sits 1 px behind the photo's.
   - The **rear is nearly horizontal and thick**. The primaries are 2 rows, the tail 1 to 2 rows, and the whole stack is 3 to 4 rows (rows 13-18).
     It drops about 2 rows over 12 px, which avoids the steep, thin tail the user disliked in the flight tern.
   - The **tail streamers extend past the wingtip** (streamer x0, wingtip x2), as in the Arctic tern. The **fork** is two streamers:
     the near one runs straight to x0 on row 16, and the far one dips and ends shorter at x1-2 on row 18, with an empty gap between the tips.
5. Checked the sprite: every row is 24 wide, uses only palette letters, and the eye is the only isolated pixel.

## Palette (sampled, then normalised; the photos are warm-lit or under-exposed)
| letter | colour | part | source sample |
|---|---|---|---|
| C | #414B57 | black cap | photo #1C1A1D. Drawn as dark slate, never black on black |
| E | #12161B | eye | photo #19171A. A dark dot inside the cap; a white glint looked cartoonish |
| R | #C8363F | bill | close-up core #B73A44, sunlit #C12B46 |
| W | #EEF0F2 | cheek, throat, undertail, streamers | close-up cheek #DFDEDF |
| U | #C9CED4 | grey-washed breast and belly | overcast breast #69696E at exposure 0.62, which gives #AAAAB2; kept lighter than the wing |
| G | #A7AFB8 | mantle and folded wing | close-up wing #A4A1A4, mantle #94929C |
| D | #707A85 | primaries | close-up tertials #898689, darker in shade |
| L | #C94A45 | legs and feet | sunlit legs #C9585B |
| P / p | #6E5B4A / #54463A | wooden post and its shaded side | none; a plain weathered post |

`_light` in palette.json sets W to #DCE1E6 and U to #C2C9D0 for light terminals. Without it, the white cheek and tail vanish on #F4F6F8.

## Layout and landmarks (sprite coordinates, x right, y down)
- The bird occupies rows 5-18. The feet are on row 16. **The perch top is row 17, the same row as the parrots' branch** (P, cols 16-19, rows 17-23).
  Rows 0-4 are empty and free for mood marks (think dots and so on) above the head.
- Eye (18,7). Bill pixels (20,7) (20,8) (21,8) (22,8) (23,8), gape (20,8). Crown (17,5). Nape (13,8).
  Wingtip (2,15). Tail tip (0,16), far streamer tip (1,18). Feet (17-19,16).
- **Head crop:** rows 4-11 and cols 10-23. That is 8 px, or 4 terminal rows, 14 wide, and it starts on an even row so the half-block pairs match the full sprite.
- Mood edits I checked (`vm.png`):
  - Blink: set (18,7) to C.
  - Open bill (calling): set (21,7) and (22,7) to R, clear (21,8) (22,8) (23,8), and set (21,9) and (22,9) to R.

## Files
sprite.txt, palette.json, landmarks.json, preview.png (dark, light, half-block dark and light, the reference, actual 16 px size, head crop).
Working files: trace.py, rast.py, covmap.py, cmp.py, view.py, preview.py, crop.png, grid.png, fit.png, and d_cmp.png / e_cmp.png (the sprite overlaid on the photo).

## Revision 2 (2026-09-26): the critic's fixes. These coordinates supersede every coordinate above.
The previous version is kept in `v1/`. The before and after comparison, with the mood head crops, is in `moods.png`, made by `moods.py`. The overlay on the photo is `new_cmp.png`.
- **The whole bird and the post moved 1 col left.** The near streamer was trimmed by 1 px, so it still ends at col 0. The wingtip is now (1,15), so the tail passes it by 1 px.
- **Bill, 5 px:** (19,7) plus (19..23,8). It has a 1-px base on the cap row and stays horizontal, with no droop. The head runs from nape col 12 to forehead col 18, 7 px, so the bill-to-head ratio is 0.71 (the photo is about 0.73).
- **Tail fork:** the near streamer is row 16, cols 0-11. The far streamer is row 17 cols 5-9 plus row 18 cols 1-5, joined edge to edge at col 5. Row 17 cols 0-4 stay empty as the fork gap. In half-blocks that gap is the empty lower half of cells 0-4, so the fork survives at 16 px.
- **White vent:** row 15 is D 1-10, then W 11-13, then U 14-18. The pale lower outline now runs from the belly through the vent into the streamers (joined at col 11).
- **Folded wing:** the G/U boundary slants from the shoulder to the vent. U starts at col 18 on row 10, 19 on rows 11-12, 17 on row 13, 15 on row 14 and 14 on row 15. The wing is one diagonal shape that leads into the D primaries, with a pale flank below it.
- **Chest:** the front edges run 19, 20, 21, 21, 20, 19, 18 down rows 9-15, a smooth curve from the throat with no square shoulder.
- **Feet:** (17,16), (18,16), (19,16). The post is now cols 15-18, rows 17-23, with p at col 18 from row 18 down. The front toe hangs 1 px past the post's front edge, and (15-16,16) are empty.
- **Streamers have their own letter, T.** On dark it is the same #EEF0F2 as W. `_light` sets T to #B0B9C2, which is 1.83:1 on #F4F6F8; the old W was 1.21:1. The streamers now show on light terminals. W (cheek, vent) keeps #DCE1E6 on light, because C, G and U surround it.
- **Eye:** unchanged, E #12161B on C, 2.05:1. The critic accepted it, since a lighter eye would lose the realism.
- **Head crop:** rows 4-11, cols 11-23. That is 13 wide and 8 px tall (4 terminal rows), starting on an even row. Col 11 is 1 px of air behind the nape, and it keeps the crown, the eye, the full 5-px bill, the cheek and 2 rows of breast. The "11-22" option in the critique would cut the bill tip at col 23, so it was not used.
- **Landmarks (landmarks.json):** eye (17,7); bill (19,7), (19..23,8); gape (19,8); tip (23,8); crown (16,5); nape (12,8); breast (21,11); vent (11-13,15); wingtip (1,15); tail tip (0,16); far tip (1,18); feet (17-19,16); perch cols 15-18, top row 17.
- **Mood edits, re-checked (`moods.png`):**
  - Blink: set (17,7) to C.
  - Calling: set (19..23,7) to R, clear (20..23,8), and set (19..22,9) to R. The lower mandible is 1 px shorter than the upper.
- Checks run: every row is 24 wide, only palette letters are used, and every landmark sits on the expected letter. The eye is the only isolated pixel.
