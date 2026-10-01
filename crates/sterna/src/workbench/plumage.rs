//! The birds a person can choose, each drawn from its own sprite.
//!
//! **A bird theme is the bird.** Its plumage is the palette: the accent, the
//! second colour and the highlight are feathers, and the ground is a dark
//! tint of them. Every bird is its own drawing, traced from photographs in
//! `art/birds/<name>/`: `sprite.txt` is one letter a pixel, `palette.json`
//! colours each letter (and `_light` recolours the ones a light terminal
//! would lose), and `landmarks.json` says where the eye and the beak are.
//! A mood is an edit of the same drawing, found from those landmarks and the
//! sprite's own bounds, so no mood names a pixel of any one bird.
//!
//! It is drawn two pixels to a cell with the half blocks, which needs a
//! terminal that shows true colour.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A species a theme can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Bird {
    Amazon,
    SunConure,
    Hyacinth,
    Scarlet,
    BlueGold,
    GreenWing,
    Military,
    Cockatoo,
    ArcticTern,
}

/// What a species is called and what it lends the screen.
pub struct Plumage {
    pub name: &'static str,
    pub title: &'static str,
    pub latin: &'static str,
    /// One line of fact, for the card beside it.
    pub nest: &'static str,
    pub accent: u32,
    pub second: u32,
    pub highlight: u32,
    /// The dock's ground: a dark tint of the bird.
    pub ground: u32,
}

/// The families a bird belongs to, as the theme picker groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kin {
    Parrot,
    Seabird,
}

impl Bird {
    pub const ALL: [Self; 9] = [
        Self::Amazon,
        Self::SunConure,
        Self::Hyacinth,
        Self::Scarlet,
        Self::BlueGold,
        Self::GreenWing,
        Self::Military,
        Self::Cockatoo,
        Self::ArcticTern,
    ];

    pub fn plumage(self) -> &'static Plumage {
        match self {
            Self::Amazon => &AMAZON,
            Self::SunConure => &SUN_CONURE,
            Self::Hyacinth => &HYACINTH,
            Self::Scarlet => &SCARLET,
            Self::BlueGold => &BLUE_GOLD,
            Self::GreenWing => &GREEN_WING,
            Self::Military => &MILITARY,
            Self::Cockatoo => &COCKATOO,
            Self::ArcticTern => &ARCTIC_TERN,
        }
    }

    #[must_use]
    pub fn kin(self) -> Kin {
        match self {
            Self::ArcticTern => Kin::Seabird,
            _ => Kin::Parrot,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|bird| bird.plumage().name == name)
    }

    fn art(self) -> &'static Art {
        &arts()[self as usize]
    }
}

static AMAZON: Plumage = Plumage {
    name: "amazon",
    title: "Blue-fronted Amazon",
    latin: "Amazona aestiva",
    nest: "a talker, and it listens first",
    accent: 0x45b653,
    second: 0x4a97e8,
    highlight: 0xf2d024,
    ground: 0x16241a,
};
static SUN_CONURE: Plumage = Plumage {
    name: "sun-conure",
    title: "Sun Conure",
    latin: "Aratinga solstitialis",
    nest: "loud about good news",
    accent: 0xffb020,
    second: 0x36a35a,
    highlight: 0xff6a1a,
    ground: 0x2a1f10,
};
static HYACINTH: Plumage = Plumage {
    name: "hyacinth",
    title: "Hyacinth Macaw",
    latin: "Anodorhynchus hyacinthinus",
    nest: "cracks the hard ones",
    accent: 0x5b7cf0,
    second: 0xf5c518,
    highlight: 0x8fa6ff,
    ground: 0x161c34,
};
static SCARLET: Plumage = Plumage {
    name: "scarlet",
    title: "Scarlet Macaw",
    latin: "Ara macao",
    nest: "seen from a long way off",
    // Its yellow leads: its red is the colour of a failure on this screen.
    accent: 0xf5c21b,
    second: 0xff4a3d,
    highlight: 0x3d86e8,
    ground: 0x2c1515,
};
static BLUE_GOLD: Plumage = Plumage {
    name: "blue-gold",
    title: "Blue-and-gold Macaw",
    latin: "Ara ararauna",
    nest: "steady, and never quiet for long",
    accent: 0x2fa0ea,
    second: 0xf5b914,
    highlight: 0x52c46a,
    ground: 0x122230,
};
static GREEN_WING: Plumage = Plumage {
    name: "green-wing",
    title: "Green-winged Macaw",
    latin: "Ara chloropterus",
    nest: "gentle, with a strong beak",
    // Named for its green wing, which leads; its red is a failure's colour.
    accent: 0x43a867,
    second: 0xe0404f,
    highlight: 0x3d7fd6,
    ground: 0x2a1519,
};
static MILITARY: Plumage = Plumage {
    name: "military",
    title: "Military Macaw",
    latin: "Ara militaris",
    nest: "in formation",
    accent: 0x8fbd4f,
    second: 0xe0413e,
    highlight: 0x5a8fd6,
    ground: 0x1c2412,
};
static COCKATOO: Plumage = Plumage {
    name: "cockatoo",
    title: "Sulphur-crested Cockatoo",
    latin: "Cacatua galerita",
    nest: "crest up when it has an idea",
    accent: 0xf7d23a,
    second: 0xf4f2ec,
    highlight: 0x9cc3ff,
    ground: 0x24221a,
};
static ARCTIC_TERN: Plumage = Plumage {
    name: "arctic-tern",
    title: "Arctic Tern",
    latin: "Sterna paradisaea",
    nest: "the longest migration of any bird",
    accent: 0xdce1e6,
    second: 0xc8363f,
    highlight: 0x707a85,
    ground: 0x14181d,
};

/// What the bird is doing: read off the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Blink,
    Think,
    Work,
    Done,
    Oops,
}

/// One cell of the sprite: its glyph and its two colours, `None` for the
/// terminal's own.
pub type Cell = (char, Option<u32>, Option<u32>);

/// One drawing and what it is drawn with, parsed from `art/birds/<name>/`.
struct Art {
    /// One letter a pixel, `None` where the drawing is empty.
    letters: Vec<Vec<Option<char>>>,
    palette: HashMap<char, u32>,
    /// The letters a light terminal recolours.
    light: HashMap<char, u32>,
    /// `[x, y]` of the eye and of the beak's tip.
    eye: (usize, usize),
    beak_tip: (usize, usize),
    /// The drawing's own head crop, when it names one: its pixel rows and
    /// its first column.
    head_crop: Option<(usize, usize, usize)>,
}

/// The drawings, one per [`Bird`] in [`Bird::ALL`]'s order.
fn arts() -> &'static [Art] {
    static ARTS: OnceLock<Vec<Art>> = OnceLock::new();
    ARTS.get_or_init(|| {
        macro_rules! art {
            ($dir:literal) => {
                Art::parse(
                    include_str!(concat!("../../../../art/birds/", $dir, "/sprite.txt")),
                    include_str!(concat!("../../../../art/birds/", $dir, "/palette.json")),
                    include_str!(concat!("../../../../art/birds/", $dir, "/landmarks.json")),
                )
            };
        }
        vec![
            art!("amazon"),
            art!("sun-conure"),
            art!("hyacinth"),
            art!("scarlet"),
            art!("blue-gold"),
            art!("green-wing"),
            art!("military"),
            art!("cockatoo"),
            art!("tern-perched"),
        ]
    })
}

/// The flying tern `sterna --version` prints.
fn mark_art() -> &'static Art {
    static MARK: OnceLock<Art> = OnceLock::new();
    MARK.get_or_init(|| {
        Art::parse(
            include_str!("../../../../art/birds/tern-mark/sprite.txt"),
            include_str!("../../../../art/birds/tern-mark/palette.json"),
            include_str!("../../../../art/birds/tern-mark/landmarks.json"),
        )
    })
}

fn hex(value: &serde_json::Value) -> Option<u32> {
    u32::from_str_radix(value.as_str()?.strip_prefix('#')?, 16).ok()
}

fn point(value: &serde_json::Value) -> Option<(usize, usize)> {
    Some((value[0].as_u64()? as usize, value[1].as_u64()? as usize))
}

fn colours(map: &serde_json::Value) -> HashMap<char, u32> {
    map.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let mut letters = key.chars();
            let letter = letters.next().filter(|_| letters.next().is_none())?;
            Some((letter, hex(value)?))
        })
        .collect()
}

impl Art {
    /// Parses one drawing. The files are this repository's own and a test
    /// reads every one, so a malformed file is a build that fails its tests,
    /// never a person's broken screen.
    fn parse(sprite: &str, palette: &str, landmarks: &str) -> Self {
        let palette: serde_json::Value = serde_json::from_str(palette).expect("palette.json");
        let landmarks: serde_json::Value = serde_json::from_str(landmarks).expect("landmarks.json");
        let crop = &landmarks["head_crop"];
        Self {
            letters: sprite
                .lines()
                .map(|line| line.chars().map(|c| (c != '.').then_some(c)).collect())
                .collect(),
            light: colours(&palette["_light"]),
            palette: colours(&palette),
            eye: point(&landmarks["eye"]).expect("an eye"),
            beak_tip: point(&landmarks["beak_tip"]).expect("a beak tip"),
            head_crop: point(&crop["rows"])
                .zip(crop["cols"][0].as_u64())
                .map(|((top, bottom), left)| (top, bottom, left as usize)),
        }
    }

    fn width(&self) -> usize {
        self.letters.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// The first pixel row with anything drawn on it.
    fn top(&self) -> usize {
        self.letters
            .iter()
            .position(|row| row.iter().any(Option::is_some))
            .unwrap_or(0)
    }

    /// The rightmost column the head reaches: the drawn pixels from the top
    /// down to the beak's tip, so a perch that runs past the bird does not
    /// push the marks away from its face.
    fn head_right(&self) -> usize {
        self.letters[self.top()..=self.beak_tip.1]
            .iter()
            .filter_map(|row| row.iter().rposition(Option::is_some))
            .max()
            .unwrap_or(0)
    }

    /// The pixels in colour, on a canvas wide enough for the marks beside
    /// the head, from the first pixel row pair with anything drawn on it.
    fn pixels(&self, mood: Mood, light: bool) -> Vec<Vec<Option<u32>>> {
        // On a light ground a letter takes its `_light` colour, or its own
        // when the drawing gives none; either one that would vanish into the
        // ground (the cockatoo's white, the tern's) is shaded just enough to
        // hold its outline.
        let colour = |letter: char| {
            let dark = self.palette.get(&letter).copied()?;
            Some(match (light, self.light.get(&letter)) {
                (false, _) => dark,
                (true, light) => {
                    super::look::readable(light.copied().unwrap_or(dark), true, OUTLINE)
                }
            })
        };
        let (top, right) = (self.top(), self.head_right());
        let width = self.width().max(right + MARK_ROOM + 1);
        let mut grid: Vec<Vec<Option<u32>>> = self
            .letters
            .iter()
            .map(|row| {
                let mut row: Vec<Option<u32>> = row.iter().map(|c| c.and_then(colour)).collect();
                row.resize(width, None);
                row
            })
            .collect();
        let (ex, ey) = self.eye;
        let (_, beak_y) = self.beak_tip;
        // A mark only ever lands on empty pixels: it is beside the bird,
        // never painted over it.
        let mark = |grid: &mut Vec<Vec<Option<u32>>>, points: &[(usize, usize)], rgb: u32| {
            for &(x, y) in points {
                if let Some(pixel) = grid.get_mut(y).and_then(|row| row.get_mut(x))
                    && pixel.is_none()
                {
                    *pixel = Some(rgb);
                }
            }
        };
        match mood {
            Mood::Idle => {}
            // The eye closes: it takes the colour most of its neighbours
            // have, which is the head around it.
            Mood::Blink => {
                let mut seen: HashMap<u32, usize> = HashMap::new();
                for (x, y) in [
                    (ex.wrapping_sub(1), ey),
                    (ex + 1, ey),
                    (ex, ey.wrapping_sub(1)),
                    (ex, ey + 1),
                ] {
                    if let Some(Some(rgb)) = grid.get(y).and_then(|row| row.get(x)) {
                        *seen.entry(*rgb).or_default() += 1;
                    }
                }
                let head = seen
                    .into_iter()
                    .max_by_key(|(rgb, n)| (*n, *rgb))
                    .map(|(rgb, _)| rgb);
                grid[ey][ex] = head;
            }
            Mood::Oops => grid[ey][ex] = Some(OOPS),
            // A thought rises from beside the head.
            Mood::Think => mark(
                &mut grid,
                &[(right + 2, top + 2), (right + 3, top + 1), (right + 4, top)],
                THOUGHT,
            ),
            // Seeds under the beak: it is busy.
            Mood::Work => mark(
                &mut grid,
                &[
                    (right + 1, beak_y + 2),
                    (right + 3, beak_y + 3),
                    (right + 2, beak_y + 4),
                ],
                SEED,
            ),
            // A check, three pixels tall, beside the head.
            Mood::Done => mark(
                &mut grid,
                &[
                    (right + 1, top + 2),
                    (right + 2, top + 3),
                    (right + 3, top + 2),
                    (right + 4, top + 1),
                ],
                TICK,
            ),
        }
        // Whole empty cell rows above the drawing are not drawn: the card is
        // as tall as the bird.
        grid.drain(..top - top % 2);
        grid
    }

    /// The pixel rows and the first column of the head alone.
    fn head_window(&self) -> (std::ops::Range<usize>, usize) {
        match self.head_crop {
            Some((top, bottom, left)) => (top..bottom + 1, left),
            None => {
                let (bx, by) = self.beak_tip;
                // Eight pixel rows that end just below the beak, starting on
                // an even row so the half blocks pair as the full sprite's.
                let start = (by + 2).saturating_sub(HEAD_PIXELS).max(self.top());
                let start = start - start % 2;
                (start..start + HEAD_PIXELS, bx.saturating_sub(12))
            }
        }
    }
}

/// The least contrast a plumage colour keeps against a light ground.
const OUTLINE: f64 = 1.4;
/// Pixels to the right of the head kept for the think, work and done marks.
const MARK_ROOM: usize = 4;
/// How many pixel rows the header's head is: four cells.
const HEAD_PIXELS: usize = 8;
const THOUGHT: u32 = 0xc9ced8;
const SEED: u32 = 0xe8c35a;
const TICK: u32 = 0x5fd07a;
const OOPS: u32 = 0xff5a52;

/// The sprite for `bird` in `mood`, as rows of half-block cells, in its
/// light-ground colours when `light`.
pub fn sprite(bird: Bird, mood: Mood, light: bool) -> Vec<Vec<Cell>> {
    cells(&bird.art().pixels(mood, light))
}

/// The bird's head alone, at the sprite's own resolution, in `mood`: what
/// stands beside the conversation's card once the bird has left its perch.
/// Four rows tall; as wide as the bird's head and its marks.
pub fn head(bird: Bird, mood: Mood, light: bool) -> Vec<Vec<Cell>> {
    let art = bird.art();
    let (rows, left) = art.head_window();
    // `pixels` has dropped the empty rows above the drawing.
    let dropped = art.top() - art.top() % 2;
    let grid = art.pixels(mood, light);
    let crop: Vec<Vec<Option<u32>>> = (rows.start - dropped..rows.end - dropped)
        .map(|y| {
            grid.get(y)
                .map_or_else(Vec::new, |row| row[left.min(row.len())..].to_vec())
        })
        .collect();
    let width = crop.iter().map(Vec::len).max().unwrap_or(0);
    let crop: Vec<Vec<Option<u32>>> = crop
        .into_iter()
        .map(|mut row| {
            row.resize(width, None);
            row
        })
        .collect();
    cells(&crop)
}

/// The flying tern as lines of text in true colour, for `sterna --version`
/// and the start's splash: `light` draws it with its light-terminal colours,
/// and `lowered` sets it that many pixels lower -- a glide's rise and fall
/// moves the traced drawing, it never redraws it.
pub fn mark(light: bool, lowered: usize) -> Vec<String> {
    let mut grid = mark_art().pixels(Mood::Idle, light);
    let width = grid.first().map_or(0, Vec::len);
    grid.splice(0..0, std::iter::repeat_n(vec![None; width], lowered));
    if grid.len() % 2 == 1 {
        grid.push(vec![None; grid[0].len()]);
    }
    let rgb =
        |layer: u8, c: u32| format!("{layer}8;2;{};{};{}", c >> 16, (c >> 8) & 0xff, c & 0xff);
    cells(&grid)
        .into_iter()
        .map(|row| {
            let end = row
                .iter()
                .rposition(|cell| cell.1.is_some())
                .map_or(0, |i| i + 1);
            let mut line = String::new();
            for (glyph, fg, bg) in &row[..end] {
                match (fg, bg) {
                    (Some(fg), Some(bg)) => {
                        line.push_str(&format!("\x1b[{};{}m{glyph}", rgb(3, *fg), rgb(4, *bg)));
                    }
                    (Some(fg), None) => line.push_str(&format!("\x1b[0;{}m{glyph}", rgb(3, *fg))),
                    _ => line.push_str("\x1b[0m "),
                }
            }
            line + "\x1b[0m"
        })
        .collect()
}

/// Pixel rows two at a time as half-block cells.
fn cells(grid: &[Vec<Option<u32>>]) -> Vec<Vec<Cell>> {
    grid.chunks(2)
        .map(|pair| {
            (0..pair[0].len())
                .map(|x| {
                    let bottom = pair.get(1).and_then(|row| row.get(x)).copied().flatten();
                    match (pair[0][x], bottom) {
                        (None, None) => (' ', None, None),
                        (Some(top), None) => ('▀', Some(top), None),
                        (None, Some(bottom)) => ('▄', Some(bottom), None),
                        (Some(top), Some(bottom)) => ('▀', Some(top), Some(bottom)),
                    }
                })
                .collect()
        })
        .collect()
}

/// Whether this terminal shows 24-bit colour, which the sprite needs.
pub fn truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("truecolor") || value.contains("24bit")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every drawing parses whole: rows of one width, every letter coloured,
    /// the eye and the beak's tip on drawn pixels.
    #[test]
    fn every_drawing_is_whole_and_every_letter_is_coloured() {
        for (art, (w, h)) in arts()
            .iter()
            .zip([(18, 24); 8].into_iter().chain([(24, 24)]))
            .chain([(mark_art(), (38, 31))])
        {
            assert_eq!(art.letters.len(), h);
            assert!(art.letters.iter().all(|row| row.len() == w));
            for letter in art.letters.iter().flatten().flatten() {
                assert!(art.palette.contains_key(letter), "{letter} has no colour");
            }
        }
        // The moods are found from these two, so on every bird they must
        // be pixels of the bird. (The mark takes no mood.)
        for bird in Bird::ALL {
            let art = bird.art();
            let at = |(x, y): (usize, usize)| art.letters[y][x];
            assert!(at(art.eye).is_some(), "{bird:?}: the eye is not drawn");
            assert!(
                at(art.beak_tip).is_some(),
                "{bird:?}: the beak's tip is not drawn"
            );
            assert_eq!(Bird::parse(bird.plumage().name), Some(bird));
        }
    }

    /// Each bird is its own drawing, not one outline in eight palettes.
    #[test]
    fn every_bird_is_its_own_drawing() {
        let shapes: Vec<Vec<Vec<bool>>> = Bird::ALL
            .iter()
            .map(|bird| {
                bird.art()
                    .letters
                    .iter()
                    .map(|row| row.iter().map(Option::is_some).collect())
                    .collect()
            })
            .collect();
        for (i, a) in shapes.iter().enumerate() {
            for b in &shapes[i + 1..] {
                assert_ne!(a, b, "two birds share an outline");
            }
        }
    }

    /// A blink closes the eye in the head's colour and changes nothing
    /// else; oops reddens the eye; the marks land beside the head, on empty
    /// pixels, for every bird.
    #[test]
    fn moods_are_found_from_each_birds_landmarks() {
        for bird in Bird::ALL {
            let art = bird.art();
            let lift = art.top() - art.top() % 2;
            let (ex, ey) = (art.eye.0, art.eye.1 - lift);
            let idle = art.pixels(Mood::Idle, false);
            let changed = |mood| -> Vec<(usize, usize)> {
                let other = art.pixels(mood, false);
                (0..idle.len())
                    .flat_map(|y| (0..idle[y].len()).map(move |x| (x, y)))
                    .filter(|&(x, y)| idle[y][x] != other[y][x])
                    .collect()
            };
            assert_eq!(changed(Mood::Blink), vec![(ex, ey)], "{bird:?} blink");
            let blink = art.pixels(Mood::Blink, false)[ey][ex];
            assert_ne!(blink, idle[ey][ex]);
            assert!(
                [
                    (ex - 1, ey),
                    (ex + 1, ey),
                    (ex, ey.wrapping_sub(1)),
                    (ex, ey + 1)
                ]
                .iter()
                .any(|&(x, y)| idle.get(y).and_then(|row| row.get(x)) == Some(&blink)),
                "{bird:?}: a blink is the colour around the eye"
            );
            assert_eq!(art.pixels(Mood::Oops, false)[ey][ex], Some(OOPS));
            for mood in [Mood::Think, Mood::Work, Mood::Done] {
                let marks = changed(mood);
                assert!(!marks.is_empty(), "{bird:?} {mood:?} drew nothing");
                assert!(
                    marks.iter().all(|&(x, y)| idle[y][x].is_none()
                        && (art.head_right() + 1..=art.head_right() + MARK_ROOM).contains(&x)),
                    "{bird:?} {mood:?} is not beside the head, or painted over the bird"
                );
            }
        }
    }

    /// The header's head is four rows of the bird's own face: its eye and
    /// the tip of its beak are in the crop.
    #[test]
    fn the_head_is_four_rows_with_the_eye_and_the_beak() {
        for bird in Bird::ALL {
            let head = head(bird, Mood::Idle, false);
            assert_eq!(head.len(), 4, "{bird:?}");
            let art = bird.art();
            let (rows, left) = art.head_window();
            for (x, y) in [art.eye, art.beak_tip] {
                assert!(
                    rows.contains(&y) && x >= left,
                    "{bird:?} crop misses ({x}, {y})"
                );
            }
        }
    }

    /// On a light ground no plumage vanishes: every colour -- the drawing's
    /// own light variant, or one shaded for it -- keeps an outline's worth
    /// of contrast.
    #[test]
    fn a_light_ground_loses_no_bird() {
        use super::super::look::{LIGHT_GROUND, contrast};
        for bird in Bird::ALL {
            let art = bird.art();
            let light = art.pixels(Mood::Idle, true);
            for (y, row) in art
                .letters
                .iter()
                .enumerate()
                .skip(art.top() - art.top() % 2)
            {
                for (x, letter) in row.iter().enumerate() {
                    let Some(letter) = letter else {
                        continue;
                    };
                    let rgb = light[y - (art.top() - art.top() % 2)][x].unwrap();
                    assert!(
                        contrast(rgb, LIGHT_GROUND) >= OUTLINE - 0.05,
                        "{bird:?} {letter} {rgb:06x} vanishes on a light ground"
                    );
                }
            }
        }
    }

    #[test]
    fn the_mark_is_the_flying_tern_in_its_colours() {
        let dark = mark(false, 0);
        assert_eq!(dark.len(), 16);
        // The bill's red.
        assert!(dark.concat().contains("38;2;207;43;43"));
        assert!(mark(true, 0).concat().contains("38;2;196;38;42"));
    }
}
