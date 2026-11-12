//! The showcase's design tokens: colors by semantic role, for light and dark
//! windows, the corner radii and the numerals. Everything else reads them
//! from here; these definitions are the only place a color value is written
//! down.
//!
//! Text meant to be read keeps 4.5:1 against the surface it sits on in either
//! theme, and text on a colored fill keeps it against the fill.

use std::sync::Arc;

use gpui::{App, FontFeatures, Global, Hsla, Pixels, Window, WindowAppearance, hsla, px, rgb};

#[derive(Clone)]
pub struct Theme {
    /// The window's main surface and the text on it.
    pub background: Hsla,
    pub foreground: Hsla,
    /// Quiet fills: table headers, segmented control tracks, pressed states.
    pub muted: Hsla,
    /// Secondary text: descriptions, labels, metadata.
    pub muted_foreground: Hsla,
    /// Hairlines between regions and around cards.
    pub border: Hsla,
    /// The frame of a control that takes input, and the track of a switch
    /// that is off: a step stronger than `border`, so that it reads as a
    /// control rather than a region.
    pub input: Hsla,
    /// The navigation sidebar's surface, and its hovered and selected rows.
    pub sidebar: Hsla,
    pub sidebar_accent: Hsla,
    /// Hovered rows and quiet buttons.
    pub accent: Hsla,
    /// A filled button, and the selected one of a row of choices; then
    /// either hovered.
    pub secondary: Hsla,
    pub secondary_hover: Hsla,
    /// Selection emphasis: switches that are on.
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    /// Every other row of a table.
    pub stripe: Hsla,
    /// Gains and losses, always shown with a sign as well.
    pub success: Hsla,
    pub danger: Hsla,
    /// Text on a `success` or `danger` fill: a count badge, the chart's last
    /// price.
    pub success_foreground: Hsla,
    pub danger_foreground: Hsla,
    /// Initials on an avatar, whose fill stands for the person.
    pub avatar_foreground: Hsla,
    /// A chart's series, such as its moving averages, in order.
    pub series: [Hsla; 3],
    /// Which GPUI the showcase runs on, marked in the toolbar so that two
    /// windows side by side cannot be mistaken: upstream in red, gpui-fast
    /// in green, with `build_foreground` on either.
    pub build_upstream: Hsla,
    pub build_fast: Hsla,
    pub build_foreground: Hsla,
    /// Tooltips and other surfaces above the window.
    pub popover: Hsla,
    /// The corner radii: small marks and controls inset in a track
    /// (`radius_sm`), controls (`radius`), and the cards and tables around
    /// them (`radius_lg`). A control inset in a track by `p_0p5` takes
    /// `radius_sm`, so that the two corners stay concentric.
    pub radius_sm: Pixels,
    pub radius: Pixels,
    pub radius_lg: Pixels,
    /// Tabular numerals, set on the window, so that the numbers in a column
    /// line up and a number that changes does not shift its neighbours.
    pub numbers: FontFeatures,
}

impl Global for Theme {}

impl Theme {
    fn light() -> Self {
        Self {
            background: rgb(0xffffff).into(),
            foreground: rgb(0x0a0a0a).into(),
            muted: rgb(0xf4f4f5).into(),
            muted_foreground: rgb(0x71717a).into(),
            border: rgb(0xe4e4e7).into(),
            input: rgb(0xd4d4d8).into(),
            sidebar: rgb(0xfafafa).into(),
            sidebar_accent: rgb(0xececee).into(),
            accent: rgb(0xf4f4f5).into(),
            secondary: rgb(0xf4f4f5).into(),
            secondary_hover: rgb(0xe4e4e7).into(),
            primary: rgb(0x18181b).into(),
            primary_foreground: rgb(0xfafafa).into(),
            stripe: hsla(0., 0., 0.5, 0.03),
            success: rgb(0x15803d).into(),
            danger: rgb(0xdc2626).into(),
            success_foreground: rgb(0xffffff).into(),
            danger_foreground: rgb(0xffffff).into(),
            avatar_foreground: rgb(0xffffff).into(),
            series: [
                rgb(0xca8a04).into(),
                rgb(0x9333ea).into(),
                rgb(0x0284c7).into(),
            ],
            build_upstream: rgb(0xdc2626).into(),
            build_fast: rgb(0x15803d).into(),
            build_foreground: rgb(0xffffff).into(),
            popover: rgb(0xffffff).into(),
            radius_sm: px(4.),
            radius: px(6.),
            radius_lg: px(8.),
            numbers: tabular_numbers(),
        }
    }

    fn dark() -> Self {
        Self {
            background: rgb(0x0a0a0a).into(),
            foreground: rgb(0xfafafa).into(),
            muted: rgb(0x1c1c1f).into(),
            muted_foreground: rgb(0xa1a1aa).into(),
            border: rgb(0x27272a).into(),
            input: rgb(0x3f3f46).into(),
            sidebar: rgb(0x111113).into(),
            sidebar_accent: rgb(0x232326).into(),
            accent: rgb(0x1c1c1f).into(),
            secondary: rgb(0x27272a).into(),
            secondary_hover: rgb(0x3f3f46).into(),
            primary: rgb(0xfafafa).into(),
            primary_foreground: rgb(0x18181b).into(),
            stripe: hsla(0., 0., 0.5, 0.05),
            success: rgb(0x4ade80).into(),
            // The light theme's red is under 4.5:1 on this background; this
            // one matches the green's weight.
            danger: rgb(0xf87171).into(),
            success_foreground: rgb(0x0a0a0a).into(),
            danger_foreground: rgb(0x0a0a0a).into(),
            avatar_foreground: rgb(0xffffff).into(),
            series: [
                rgb(0xfacc15).into(),
                rgb(0xc084fc).into(),
                rgb(0x38bdf8).into(),
            ],
            build_upstream: rgb(0xdc2626).into(),
            build_fast: rgb(0x15803d).into(),
            build_foreground: rgb(0xffffff).into(),
            popover: rgb(0x18181b).into(),
            radius_sm: px(4.),
            radius: px(6.),
            radius_lg: px(8.),
            numbers: tabular_numbers(),
        }
    }

    /// Sets the theme for the window's appearance, and keeps it following it.
    pub fn follow(window: &mut Window, cx: &mut App) {
        cx.set_global(Self::for_appearance(window.appearance()));
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }
}

fn tabular_numbers() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// The theme, for elements to read their colors from.
pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}
