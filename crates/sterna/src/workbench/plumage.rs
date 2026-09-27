//! The parrots a person can choose, and the one sprite each is drawn from.
//!
//! **A bird theme is the bird.** Its plumage is the palette: the accent, the
//! second colour and the highlight are feathers, and the ground is a dark
//! tint of them. The sprite is 18 by 24 pixels, a perched parrot facing
//! right; each letter of [`ART`] is a region of plumage and a species is a
//! colour for every region, so eight birds are eight tables, not eight
//! drawings. It is drawn two pixels to a cell with the half blocks, which
//! needs a terminal that shows true colour -- elsewhere the outline bird
//! stands in, in the accent.

/// A species a theme can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bird {
    Amazon,
    SunConure,
    Hyacinth,
    Scarlet,
    BlueGold,
    GreenWing,
    Military,
    Cockatoo,
}

/// What a species is called and what it lends the screen.
pub struct Plumage {
    pub name: &'static str,
    pub title: &'static str,
    pub latin: &'static str,
    /// One line of character, for the card beside it.
    pub nest: &'static str,
    pub accent: u32,
    pub second: u32,
    pub highlight: u32,
    /// The dock's ground: a dark tint of the bird.
    pub ground: u32,
    /// A colour per region of [`ART`], `0` for none.
    regions: [u32; REGIONS.len()],
}

/// The regions of [`ART`], in the order [`Plumage::regions`] colours them:
/// crest, forehead, head, cheek, eye ring, eye, beak, body, shoulder, wing,
/// talons, perch, tail, tail tip.
const REGIONS: [char; 14] = [
    'X', 'F', 'H', 'C', 'R', 'E', 'K', 'B', 'S', 'W', 'G', 'P', 'T', 'U',
];

/// Sprite width in pixels (and so in cells).
pub const WIDTH: usize = 18;
/// Sprite height in cells: 24 pixels, two to a cell.
pub const ROWS: usize = 12;

const ART: [&str; 24] = [
    ".........XX.......",
    "........XXX.......",
    ".......XXX........",
    ".......FFFFF......",
    "......FFFFFFH.....",
    ".....HHHHRRHHH....",
    ".....HHHHREHHKK...",
    ".....HHHCCCHHKKK..",
    ".....HHCCCCCHK.K..",
    "....BBCCCCCH......",
    "...SBBBBBBB.......",
    "...SSBBBBBBB......",
    "..SWWBBBBBBB......",
    "..WWWWBBBBBB......",
    "..WWWWWBBBBB......",
    ".WWWWWWBBBBB......",
    ".WWWWWWWBBBG......",
    "PPPPPWWWBBGGPPPPPP",
    ".TTWWWW...........",
    "TTTTT.............",
    "UUUU..............",
    "UUU...............",
    "UU................",
    "U.................",
];

const TALONS: u32 = 0x5c5c60;
const PERCH: u32 = 0x7a5230;

/// A species' eleven colours -- crest, forehead, head, cheek, eye ring,
/// beak, body, shoulder, wing, tail, tail tip -- with the eye, talons and
/// perch every bird shares, in [`REGIONS`] order.
const fn regions(c: [u32; 11]) -> [u32; 14] {
    [
        c[0], c[1], c[2], c[3], c[4], 0x141414, c[5], c[6], c[7], c[8], TALONS, PERCH, c[9], c[10],
    ]
}

impl Bird {
    pub const ALL: [Self; 8] = [
        Self::Amazon,
        Self::SunConure,
        Self::Hyacinth,
        Self::Scarlet,
        Self::BlueGold,
        Self::GreenWing,
        Self::Military,
        Self::Cockatoo,
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
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|bird| bird.plumage().name == name)
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
    regions: regions([
        0, 0x3d8fe0, 0x2e9e3e, 0xf2d024, 0xf3ead2, 0x3b3b3d, 0x37a64a, 0xd9302c, 0x23803a,
        0x3fae4f, 0,
    ]),
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
    regions: regions([
        0, 0xff7a1a, 0xffae1f, 0xff6a1a, 0xf7f1e6, 0x2a2a2c, 0xffc426, 0xffc426, 0x2f9e4f,
        0x3a8f5a, 0x2f6fb3,
    ]),
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
    regions: regions([
        0, 0x3553c4, 0x3050bc, 0xf5c518, 0xf5c518, 0x2b2b30, 0x2f4fb8, 0x2f4fb8, 0x27409a,
        0x2a46a8, 0x21388c,
    ]),
};
static SCARLET: Plumage = Plumage {
    name: "scarlet",
    title: "Scarlet Macaw",
    latin: "Ara macao",
    nest: "seen from a long way off",
    accent: 0xff4a3d,
    second: 0xf5c21b,
    highlight: 0x3d86e8,
    ground: 0x2c1515,
    regions: regions([
        0, 0xe0282a, 0xe0282a, 0xf4efe6, 0xf4efe6, 0xe8dcc0, 0xd9232a, 0xf5c21b, 0x2d6fd0,
        0xd9232a, 0x2d6fd0,
    ]),
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
    regions: regions([
        0, 0x3fae4f, 0x1f8fd6, 0xf4efe6, 0xf4efe6, 0x1c1c1e, 0xf5b914, 0x1f8fd6, 0x1f8fd6,
        0x1f8fd6, 0x1a6aa8,
    ]),
};
static GREEN_WING: Plumage = Plumage {
    name: "green-wing",
    title: "Green-winged Macaw",
    latin: "Ara chloropterus",
    nest: "gentle, with a strong beak",
    accent: 0xe0404f,
    second: 0x43a867,
    highlight: 0x3d7fd6,
    ground: 0x2a1519,
    regions: regions([
        0, 0xb3202a, 0xb3202a, 0xf4ece4, 0xf4ece4, 0xefe4cf, 0xb3202a, 0x3f9b5a, 0x2d6cb8,
        0xb3202a, 0x2d6cb8,
    ]),
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
    regions: regions([
        0, 0xe0302e, 0x6f9a3a, 0xf0e8e0, 0xf0e8e0, 0x2a2a2c, 0x6f9a3a, 0x6f9a3a, 0x4e7ab8,
        0x9a4a2a, 0x4e7ab8,
    ]),
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
    regions: regions([
        0xf7d23a, 0xf7f5ef, 0xf7f5ef, 0xf5eec4, 0xbcd8ff, 0x26262a, 0xf4f2ec, 0xf4f2ec, 0xe6e2d6,
        0xece8dc, 0,
    ]),
};

/// What the bird is doing: read off the session, like the outline bird's face.
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

/// The sprite for `bird` in `mood`, as [`ROWS`] rows of [`WIDTH`] cells.
pub fn sprite(bird: Bird, mood: Mood) -> Vec<Vec<Cell>> {
    cells(&pixels(bird, mood))
}

/// The pixel rows the head is cropped from: crest, eye and hooked beak.
const HEAD_ROWS: std::ops::RangeInclusive<usize> = 1..=8;
/// The pixel columns the head is cropped from, fixed for every mood so the
/// text beside it never shifts: the sweat drop at 4 to the tick at 17.
const HEAD_COLUMNS: std::ops::RangeInclusive<usize> = 4..=17;
/// How many cells wide [`head`] is.
pub const HEAD_WIDTH: usize = 14;
/// How many rows tall [`head`] is.
pub const HEAD_HEIGHT: usize = 4;

/// The bird's head alone, at the sprite's own resolution, in `mood`: what
/// stands beside the conversation's card once the bird has left its perch.
pub fn head(bird: Bird, mood: Mood) -> Vec<Vec<Cell>> {
    let grid = pixels(bird, mood);
    let crop: Vec<Vec<Option<u32>>> = grid[HEAD_ROWS]
        .iter()
        .map(|row| row[HEAD_COLUMNS].to_vec())
        .collect();
    cells(&crop)
}

/// One colour or none per pixel, the mood's edits applied.
fn pixels(bird: Bird, mood: Mood) -> Vec<Vec<Option<u32>>> {
    let plumage = bird.plumage();
    let colour = |letter: char| {
        REGIONS
            .iter()
            .position(|region| *region == letter)
            .map(|index| plumage.regions[index])
            .filter(|rgb| *rgb != 0)
    };
    let mut grid: Vec<Vec<Option<u32>>> = ART
        .iter()
        .map(|line| line.chars().map(colour).collect())
        .collect();
    let mut put = |y: usize, x: usize, rgb: Option<u32>| grid[y][x] = rgb;
    let head = colour('H');
    let (seed, tick, sweat) = (Some(0xe8c35a), Some(0x5fd07a), Some(0x9cc9ff));
    match mood {
        Mood::Idle => {}
        Mood::Blink => {
            for (y, x) in [(5, 9), (5, 10), (6, 10)] {
                put(y, x, head);
            }
            put(6, 9, colour('R'));
        }
        Mood::Think => {
            for (y, x) in [(2, 14), (1, 15), (0, 16), (0, 17)] {
                put(y, x, Some(0xc9ced8));
            }
        }
        Mood::Work => {
            for (y, x) in [(10, 14), (11, 16), (12, 15)] {
                put(y, x, seed);
            }
            // The beak opens: the hook lifts, the lower mandible drops.
            put(8, 15, None);
            put(9, 13, colour('K'));
        }
        Mood::Done => {
            for (y, x) in [(3, 15), (4, 16), (3, 17), (2, 17)] {
                put(y, x, tick);
            }
            put(1, 17, None);
        }
        Mood::Oops => {
            put(6, 10, Some(0xff5a52));
            put(4, 4, sweat);
            put(5, 4, sweat);
        }
    }
    grid
}

/// Pixel rows two at a time as half-block cells.
fn cells(grid: &[Vec<Option<u32>>]) -> Vec<Vec<Cell>> {
    grid.chunks(2)
        .map(|pair| {
            (0..pair[0].len())
                .map(|x| match (pair[0][x], pair[1][x]) {
                    (None, None) => (' ', None, None),
                    (Some(top), None) => ('▀', Some(top), None),
                    (None, Some(bottom)) => ('▄', Some(bottom), None),
                    (Some(top), Some(bottom)) => ('▀', Some(top), Some(bottom)),
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

    #[test]
    fn every_bird_is_a_whole_sprite_in_its_own_colours() {
        for bird in Bird::ALL {
            let rows = sprite(bird, Mood::Idle);
            assert_eq!(rows.len(), ROWS);
            assert!(rows.iter().all(|row| row.len() == WIDTH));
            assert_eq!(Bird::parse(bird.plumage().name), Some(bird));
        }
        // The cockatoo alone wears a crest; a macaw's crest cells are empty.
        let crest = |bird| sprite(bird, Mood::Idle)[0][9];
        assert_eq!(crest(Bird::Cockatoo).1, Some(0xf7d23a));
        assert_eq!(crest(Bird::Hyacinth), (' ', None, None));
    }

    #[test]
    fn a_mood_is_an_edit_of_the_same_bird() {
        let idle = sprite(Bird::Scarlet, Mood::Idle);
        for mood in [Mood::Blink, Mood::Think, Mood::Work, Mood::Done, Mood::Oops] {
            let other = sprite(Bird::Scarlet, mood);
            let changed = idle
                .iter()
                .flatten()
                .zip(other.iter().flatten())
                .filter(|(a, b)| a != b)
                .count();
            assert!(
                (1..=6).contains(&changed),
                "{mood:?} changed {changed} cells"
            );
        }
    }
}
