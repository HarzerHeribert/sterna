# Sulphur-crested cockatoo (Cacatua galerita): 18 x 24 sprite

Perched, facing right, with the crest partly raised. `.` is transparent, every row is 18 wide, and there are 24 rows.
The perch is row 18. The whole bird fits in columns 2-15.

## Photos used (Wikimedia Commons)

1. **Primary trace:** *Cacatua galerita -Hayman Island -perching on balcony-8.jpg*,
   Sarah Ackerman, CC BY 2.0 (first posted to Flickr as "Australian Bird on our Balcony").
   https://commons.wikimedia.org/wiki/File:Cacatua_galerita_-Hayman_Island_-perching_on_balcony-8.jpg
   The bird is side-on, faces right and has its crest partly raised. The outline, crest, head, beak, eye, breast, back, wing edge and feet all come from this photo.
2. **Head detail:** *Cacatua galerita - Vogelburg Weilrod 02.jpg*, H. Zell, CC BY-SA 3.0.
   https://commons.wikimedia.org/wiki/File:Cacatua_galerita_-_Vogelburg_Weilrod_02.jpg
   I took the eye ring, eye and beak colours from this close-up. It also shows the cheek feathers covering the lower mandible.
3. **Tail length check:** *Cacatua galerita - Vogelburg Weilrod 03.jpg*, H. Zell, CC BY-SA 3.0.
   https://commons.wikimedia.org/wiki/File:Cacatua_galerita_-_Vogelburg_Weilrod_03.jpg
   This is the whole bird side-on on a branch. Its tail reaches about 0.46 × the feet-to-crown height below the feet.
4. **Undertail yellow:** *Sulphur Crested Cockatoo.jpg*, Ffyfejam, CC BY-SA 4.0.
   https://commons.wikimedia.org/wiki/File:Sulphur_Crested_Cockatoo.jpg
   It shows the yellow wash under the tail of an upright perched bird.

## Method

- **Crop:** photo 1 at 440x610+210+28, scaled 1.5x onto a 660x1100 canvas. The canvas is extended downward because the tail leaves the frame.
- **Checking the outline:** I sampled pixel colours along each row (`magick ... -scale 40x1 txt:-`). That separates the white bird in shade (#8A775D) from the beige wall behind it. My first back line was 20-40 px too far out at the neck.
- **Tail tip:** the tail runs out of the frame. I extended its two edges, (78,760)->(42,915) and (254,792)->(145,900), until they met below the frame. I set the tip at (5,1075), which gives the same tail/body ratio as photo 3.
- **Trace and raster:** the parts are Catmull-Rom outlines in `work/trace.py`. Overlays are in `work/fit.png` and `work/fitflat.png`. `work/raster.py` scales the head 1.2x about the neck (pivot 440,320). It then rasterises at 32x supersampling, and each cell takes its majority part, with extra weight for the eye, beak, crest and feet.
- **Pose:** I tried rotating 6° to fill the width better (`work/sprite_diagonal_v1.txt`), but it only gained 3% in size and made the bird lean, unlike the photo. The final sprite is unrotated (`work/up1.txt`). It was checked cell by cell against the trace in `work/check_up1.png`.
- **Scale:** 46.3 photo px per sprite px. The crest tip is at row 0 and the tail tip is at row 23.

## Landmarks (sprite coordinates, x right, y down)

- **Eye:** (13,5), with the pale blue ring on both sides at (12,5) and (14,5).
- **Beak:** (14,6) (15,6) (14,7) (15,7), with the hook tip at (14,8). (14,6) is the culmen highlight `k`.
- **Crown:** (14,3), the white dome in front of the crest base.
- **Crest:** from the tip at (7,0) through (7,1) (8,1) (8,2), then along row 3, x 8-13. (10,4) is the shaded base.
- **Cheek:** the faint ear-covert wash at (11,6).
- **Feet:** (9-11,17), with claws over the perch at (9,18) and (11,18).
- **Tail tip:** (2,23).

## Choices

- **White on light terminals:** the white body has a grey rim `G` (#B0B7BD) on the shaded back, nape and tail edge, and a paler rim `O` (#D3D8DC) on the lit front: forehead, throat and breast. On #F4F6F8 the whole outline still reads; on #0A0E12 the rims look like soft rounding, not a drawn outline.
- **Wing:** `S` (#E4E4DF) is only a little darker than `W` (#F6F4EE), so the bird stays white. The wing edge follows the light/shade boundary in the photo, a near-vertical line at x≈9.7.
- **Nothing black on black:**
  - The beak is dark slate (#434B55) with a #646D77 culmen highlight. It touches the background, so it has to stay clearly lighter than #0A0E12.
  - The eye is #1B2127. It sits between the ring pixels and never touches the background.
- **Colours from the photos:**
  - Crest #EDD84A, from the sunlit crest in photo 1 (#EBDC62, highlight #F0E38A); crest base in shade #C7AC28 (photo shade #A68A06-#C8B230).
  - Eye ring #B4D0E6 (photo 2: #A4C4DF, #95B1C2).
  - Beak: photo 2 has #40494E-#565E62, lifted to slate.
  - Feet: grey #6E757D.
- **Head size:** the head is 1.2x its photo size. At true size it would be under 4 px wide, and there would be no room for a ring pixel on each side of the eye with a 2x3 hooked beak below it. The body, tail and crest angle are unchanged.
- **Perch:** a 1-row branch across the full width (`P` #7A5230, the same as the existing mockup). The tail passes in front of it. Map `P` to transparent to drop it.
- **Left out:** no eye highlight, because a 1-px eye has no room for one. The undertail yellow is two connected pixels, (6,19) and (5,20), along the tail's underside.

## Revision 2 (critic's fixes)

Rev 1 is saved in `work/rev1_backup/`. This section replaces the Landmarks section above where they differ. `landmarks.json` is up to date.

- **Tail: broad and blunt, no longer a spike.** Photos 3 and 4 both show a broad tail with a squared end. Rows 19-23 are now `...GSSUO` / `..GSSUO` / `..GSWO` / `.GSWO` / `.GGO`. That makes the tail 5, 5, 4, 4 px wide, ending on a flat 3-px edge at row 23, x1-3. Its length and angle are unchanged: the tip is still on row 23 and the centreline runs at about 55-60°. `tail_tip` is now the middle of that end, (2,23), and `tail_end` lists all three pixels.
- **The tail's underside has a rim.** The lower-right edge is now `O`: (7,19) (6,20) (5,21) (4,22) (3,23). The yellow undertail `U` at (6,19) and (5,20) now sits one pixel inside the edge. A script checks that every `W`/`S`/`U` pixel has a non-background neighbour. The only exceptions are the top of the crown dome, (12,3) and (13,3), which the brief asked to be white.
- **Crest: a curved plume, not an L.** New rows: row 0 x6; row 1 x6-7; row 2 x7-9; row 3 x9-11; row 4 x10 (the base, tucked into the crown). The right edge steps 2, 2, 1 px per row, and the left edge steps 2, 1, 0. So the crest leaves the crown at about 30° and turns vertical at the tip, with no right angle. It is 2 px thick at the base and 1 px at the tip.
  - I compared three versions: the critic's example with the tip at (7,0), a thicker middle, and this one (`rev2/heads.png`). This one reads most like a swept plume.
  - It also matches photo 1 better. There the crest runs about 4.5 photo-head px (5.4 px after the 1.2x head scale) from its front at x≈12 back to its tip. That puts the tip near x≈6.6, behind the nape line.
- **White crown dome.** (12,3) and (13,3) are now `W` and (14,3) is the `O` rim. The crest meets the head behind the dome, at x≤11, and does not cover the eye. The dome is now the highest point in front of the crest.
- **Nape closed; no stray olive pixel.** (9,4) is `G`, which closes the notch between the crest and the nape rim. (10,4) is `Y`, the crest base. The shaded crest colour `y` #C7AC28 is gone from the palette.
- **Front rim darker.** `O` went from #D3D8DC to #BCC3C9. Against #F4F6F8 it is now 1.64:1 (was 1.33:1), and against #0A0E12 it is 10.9:1. On dark it still reads as soft rounding. `G` is unchanged: 1.87:1 on light.
- **Cheek wash dropped.** I tried two 2-px versions of the ear-covert wash `C` (#EFE2A6): the brief's vertical (11,6)+(11,7), and a horizontal (10,6)+(11,6).
  - The vertical one reads as a tear streak. The horizontal one reads as a painted cheek bar, which is the cockatiel's field mark and is too cartoonish for this species.
  - In photo 2 the real wash is very faint: #D6D3BF against white #D8D2C9. Photo 1 does not show it at all.
  - So the head is plain white, and `C` has been removed from the palette.
- **Unchanged:** body lean and width, head scale, tail length, the eye with a ring pixel on each side, the slate beak with its hook at (14,8) (2.19:1 on dark), the feet and the perch.
- **Preview:** built by `work/make_preview2.py` into `preview.html`, then screenshotted with headless Chrome. It shows:
  - the sprite x16 on dark and on light
  - half-block text at 26 px and at 13 px
  - rev 1 before the fixes
  - photo 1 (pose, crest) and photo 3 (tail) as references
