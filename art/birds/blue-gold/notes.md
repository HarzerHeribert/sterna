# Blue-and-yellow macaw (Ara ararauna), 18 x 24, perched, facing right

## Photos (Wikimedia Commons)
- **Silhouette and landmarks, c06.jpg**: "Ara ararauna -parrot perching on table -Fort Myers Beach-8a.jpg".
  Amber Rae Lambke (originally posted to Flickr as "ft. myers beach parrot"), CC BY 2.0.
  https://commons.wikimedia.org/wiki/File:Ara_ararauna_-parrot_perching_on_table_-Fort_Myers_Beach-8a.jpg
  This was the only candidate showing the whole bird side-on with the full tail. It faces left in the photo, so the sprite mirrors it.
- **Face detail and colours, c21.jpg**: "Ara ararauna - Vogelburg Weilrod 02.jpg". H. Zell, own work, CC BY-SA 3.0.
  https://commons.wikimedia.org/wiki/File:Ara_ararauna_-_Vogelburg_Weilrod_02.jpg
- **Colours in sunlight, c01.jpg**: "Ara ararauna -Brazil -perching on branch-6a.jpg". Tiago Zaniratti (Flickr "DSC_0312"), CC BY 2.0.
  https://commons.wikimedia.org/wiki/File:Ara_ararauna_-Brazil_-perching_on_branch-6a.jpg
- **Colour cross-checks**: "Ara ararauna (Linnaeus 1758).jpg" by Michael Gäbler, CC BY 3.0, and "Ara ararauna 3.jpg" by Eliaxt, CC BY-SA 4.0.

## Method
- `bird.py` holds the landmark lists measured on the c06 crop (800x1520+120+190, scaled 0.6) and on a 2x head crop.
- `fit.png` is the traced overlay at 45% on the photo, next to the flat trace.
- `pose.py` and `rpose.py` apply the pose changes below and rasterise with crispEdges on #FF00FF. The raw rasters are `x1.txt` and `x2.txt`.
- The final sprite was finished by hand from `x1`; the intermediate steps are `h1.txt` and `h5.txt`.

## Pose changes needed to fit 18 x 24
A faithful trace at this size (`r0.txt`) gives a 2x3-pixel head and a 1-pixel tail, which no longer reads as a macaw. The real macaw's tail is about 1.3 times its crown-to-vent length.
- **Lean**: the photo's forward lean is kept, with at most 4° of straightening.
- **Head**: enlarged 1.4x about the neck and kept level. The face ends up 3x3 with the eye, and the beak 3x5.
- **Tail**: shortened to about 0.6 of its real length. It hangs at about 40° from vertical instead of the photo's 23°, which is less steep and uses the width.
  - It stays 3 px wide for rows 15-18, 2 px for rows 19-22, and 1 px only at the tip. This follows the user's note on the tern, whose tail was too steep and too thin.
  - It is still the longest feature: 9 rows and about 11 cells along its length.
- **Wing tip**: the deep-blue wing tip (D) lies along the top of the tail base, as it does in c06.
- **Perch**: a short branch (P) runs from the belly to the right edge. The feet (F) sit on it. Map P to '' to drop it.

## Colours
Sampled with `magick -crop ... -resize 1x1 txt:-`. The c06 colours are dull (overcast), so the hues come from c06, c01 and c21, with values lifted a little for a dark terminal.
- **Blue, back and wing (B `#2A93C4`)**: hue about 200. Samples were c06 `#2E6572`/`#31707F`, c01 `#2492F2`/`#2F7EC7` and c21 crown `#398C94`.
- **Deep blue, flight feathers (D `#265AA0`)**: samples c06 `#284D8C` and `#275D8B`.
- **Tail (T `#2F74BE`)**: samples c06 `#20648E`/`#30628B` and c23 `#234578`. It is slightly brighter than D so the wing tip stays visible over the tail.
- **Yellow (Y `#E8A416`)**: samples c01 `#E6B304`, c21 `#EA9C07`, c16 `#E79E0C` and c06 neck `#DD8303`.
- **Green forehead (G `#5C9632`)**: samples c06 `#476C23` and c21 `#4A5A2E`. It is lifted so it separates from both the blue and the yellow on #0A0E12.
- **Face (W `#E4E8EA`)**: from c21 `#D8DEE3`.
- **Black parts**: following the rule, nothing is black on black.
  - The beak (N) is `#4B5663`; c21 shows it as grey-black `#454F5F`.
  - The lower mandible and throat band (K) are `#3A4450`.
  - The two slates keep the hooked upper mandible separate from the jaw and throat.
  - The eye (E `#1B2229`) is surrounded by white face on every side, so it never touches the background.
- **Feet and perch**: feet F `#5C6672` (dark grey toes); perch P `#7A5A3C` (plain wood).

## Landmarks (sprite coordinates [x, y], see landmarks.json)
- **Eye**: (13,2), inside the white face with green above.
- **Crown**: (14,0), green.
- **Beak (upper mandible)**: rows 1-5, cols 15-17, with the hook tip at (17,5) and an open notch at (16,5).
- **Lower mandible**: (14,4), (14,5), (15,5).
- **Throat band**: (12,5), (13,5), (13,6), (14,6).
- **Wing tip**: (3,16).
- **Tail tip**: (0,23).
- **Feet**: (12..14,14).
- **Perch**: row 15, cols 10-17.
- **Blink**: set (13,2) to W.

## Dropped
- The black feather lines on the white face. On a 3x3 face with an eye, any line pixel reads as noise.
- The yellow shoulder spot on the wing.
- The yellow underside of the tail. I tried it as 2 px along the tail base (`h2.txt`), and it read as stray gold pixels.

## Revision 2 (after the critic's review)
The previous files are kept in `rev/` as `*_v1.*`. The variant renders are `rev/cmp2.png` (wing tip and D colour) and `rev/cmp3.png` (beak). `preview.png` now shows the previous version at actual size next to the new one.

**Geometry.** This starts from the critic's `critic/varC.txt`, with two more wing-tip pixels.
- **Neck and back**: B is set at (8,6), (8,7), (7,8), (7,9), (6,10) and (5,12).
  - The back edge is now one regular 2:1 diagonal from the nape (8,6) to the tail tip (0,23). The head sits on 7-px shoulders and not on a stalk, and the body is 8 px wide.
- **Chin**: (14,6) goes from K to Y, and (14,7) goes from '.' to Y.
  - The yellow breast wraps under the black throat band, and the chest bulges forward as in c06.
  - Only the lower-mandible pixel (15,5) still touches the outline.
- **Tail**: y=19 now covers cols 2-4 and y=20 covers cols 1-3. The widths run 3, 3, 3, 3, 2, 2, 1 from y=17 to y=23, and the 2x2 kink at y=20/21 is gone.
- **Wing tip**: (4,16) and (3,17) are now D.
  - The folded primaries form a 2-px band along the top of the tail root, (4,15), (5,15), (3,16) and (4,16), tapering to a 1-px point at (3,17).
  - This reads as a wing tip lying over the tail, as in c06, where the tips reach about a third of the way down the tail. It no longer reads as a bite out of the tail.
  - (4,14) stays B, because making it D would put a step in the wing's upper boundary. With (5,12) and (5,13) now a 2-px run, it no longer reads as a speck.

**Palette.**
- **Y `#E8A416` to `#D4900C`**: 2.49:1 on #F4F6F8 (was 1.99) and 7.19:1 on #0A0E12. This is closer to the c06 neck sample `#DD8303`, and the front outline now holds on a light terminal.
- **D `#265AA0` to `#3563B8`**: 3.35:1 on dark (was 2.81). The hue is about 219°, the same as the c06 primaries sample `#284D8C`, only lighter.
  - The more neutral lift, `#2E66B4`, merged into the tail T (`rev/cmp2.png`, B). This hue keeps the wing band a separate ultramarine against the azure tail.
- **K `#3A4450` to `#434D5A`**: 2.26:1 on dark (was 1.96).
  - The critic's `#4E5967` is lighter than the beak N and outside the #3A4450..#4B5663 slate range. At that value the jaw would stop being a separate shape.
  - With the chin fix, only one K pixel is left on the outline anyway.

**Beak left as is.** The critic's minor point is that the hook points straight down. Moving the tip to (16,5), so that it curls back (`rev/cmp3.png`, E/F), closes the notch at (16,5). At actual size the beak then reads as a lump and not a hooked parrot beak, so the notch stays. N stays `#4B5663`, the lightest allowed slate, so that it holds on dark. On light it reads dark grey, not black, which is the accepted cost of the no-black-on-black rule.

**Landmarks changed.**
- throat = (12,5), (13,5), (13,6). (14,6) is breast now.
- wing_tip = (3,17).
- Added: nape (9,5) and breast (14,7).
- Everything else (eye (13,2), beak, lower mandible, crown, tail tip, feet, perch row 15) is unchanged.
