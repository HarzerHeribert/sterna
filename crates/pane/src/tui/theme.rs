//! The palettes a person chooses between.
use crate::workbench::plumage::Bird;
use ratatui::style::Color;

/// A `0xrrggbb` as a terminal colour.
pub(crate) fn rgb(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Neon,
    Amber,
    Ice,
    Mono,
    Violet,
    Cobalt,
    Mint,
    Rose,
    /// A parrot: its plumage is the palette, and it perches in the card.
    Bird(crate::workbench::plumage::Bird),
}
impl Theme {
    /// The theme nobody chose: a parrot wherever the terminal can draw its
    /// plumage in colour, neon where it cannot.
    #[must_use]
    pub fn natural() -> Self {
        if crate::workbench::plumage::truecolor() {
            Self::Bird(Bird::Amazon)
        } else {
            Self::default()
        }
    }
    pub const ALL: [Self; 16] = [
        Self::Neon,
        Self::Amber,
        Self::Ice,
        Self::Mono,
        Self::Violet,
        Self::Cobalt,
        Self::Mint,
        Self::Rose,
        Self::Bird(Bird::Amazon),
        Self::Bird(Bird::SunConure),
        Self::Bird(Bird::Hyacinth),
        Self::Bird(Bird::Scarlet),
        Self::Bird(Bird::BlueGold),
        Self::Bird(Bird::GreenWing),
        Self::Bird(Bird::Military),
        Self::Bird(Bird::Cockatoo),
    ];
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "neon" => Some(Self::Neon),
            "amber" => Some(Self::Amber),
            "ice" => Some(Self::Ice),
            "mono" => Some(Self::Mono),
            "violet" => Some(Self::Violet),
            "cobalt" => Some(Self::Cobalt),
            "mint" => Some(Self::Mint),
            "rose" => Some(Self::Rose),
            other => Bird::parse(other).map(Self::Bird),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Neon => "neon",
            Self::Amber => "amber",
            Self::Ice => "ice",
            Self::Mono => "mono",
            Self::Violet => "violet",
            Self::Cobalt => "cobalt",
            Self::Mint => "mint",
            Self::Rose => "rose",
            Self::Bird(bird) => bird.plumage().name,
        }
    }
    pub(crate) fn backlight(self) -> Color {
        Color::Reset
    }
    pub(crate) fn dock(self) -> Color {
        match self {
            Self::Neon => Color::Rgb(20, 32, 26),
            Self::Amber => Color::Rgb(38, 29, 19),
            Self::Ice => Color::Rgb(17, 30, 39),
            Self::Mono => Color::Rgb(27, 29, 30),
            Self::Violet => Color::Rgb(30, 23, 43),
            Self::Cobalt => Color::Rgb(18, 26, 44),
            Self::Mint => Color::Rgb(16, 34, 30),
            Self::Rose => Color::Rgb(38, 22, 34),
            Self::Bird(bird) => rgb(bird.plumage().ground),
        }
    }
    /// The answer's own ground: the theme's dock, darkened, so the model's
    /// reply reads as one block without competing with a cell header for
    /// attention. Deliberately near the terminal's own background — a
    /// highlight the eye finds and does not have to look past.
    pub(crate) fn hush(self) -> Color {
        match self.dock() {
            Color::Rgb(r, g, b) => Color::Rgb(r / 2, g / 2, b / 2),
            other => other,
        }
    }

    pub fn accent(self) -> Color {
        match self {
            Self::Neon => Color::Rgb(223, 255, 0),
            Self::Amber => Color::LightYellow,
            Self::Ice => Color::LightCyan,
            Self::Mono => Color::White,
            Self::Violet => Color::Rgb(191, 154, 255),
            Self::Cobalt => Color::Rgb(114, 155, 255),
            Self::Mint => Color::Rgb(100, 231, 187),
            Self::Rose => Color::Rgb(242, 156, 218),
            Self::Bird(bird) => rgb(bird.plumage().accent),
        }
    }
}

/// `/theme`'s sheet: every palette, the one in force selected. The workbench
/// draws it as a list and a preview.
impl Theme {
    /// The family this theme belongs to.
    #[must_use]
    pub fn family(self) -> Family {
        match self {
            Self::Bird(_) => Family::Parrots,
            _ => Family::Classic,
        }
    }
    /// What the picker calls this theme.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Bird(bird) => bird.plumage().title,
            other => other.name(),
        }
    }
    /// Every theme, family by family in [`Family::ALL`]'s order, so the
    /// picker's rows and its headings agree.
    pub fn by_family() -> impl Iterator<Item = Theme> {
        Family::ALL
            .into_iter()
            .flat_map(|family| Theme::ALL.into_iter().filter(move |t| t.family() == family))
    }
    pub fn picker(current: Theme) -> super::Panel {
        let themes: Vec<Theme> = Theme::by_family().collect();
        super::Panel {
            title: "Themes".into(),
            selected: themes.iter().position(|t| *t == current).unwrap_or(0),
            rows: themes
                .iter()
                .map(|t| super::PanelRow {
                    text: format!("{} · {}", t.family().label(), t.title()),
                    command: Some(format!("/theme {}", t.name())),
                })
                .collect(),
            ..super::Panel::default()
        }
    }
}

/// The families themes come in. The picker shows one heading per family,
/// in this order; a new family is one more entry here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// A palette alone.
    Classic,
    /// A parrot: its plumage is the palette, and it perches in the card.
    Parrots,
}
impl Family {
    pub const ALL: [Self; 2] = [Self::Classic, Self::Parrots];
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Parrots => "Parrots",
        }
    }
    /// One line on what the family is, under its heading.
    #[must_use]
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Classic => "a palette alone",
            Self::Parrots => "the bird's plumage, and the bird",
        }
    }
}
