//! Theming: semantic tokens, not hardcoded colours.
//!
//! Rendering code asks for `t.accent` or `t.diff_add`, never `Color::Green`.
//! That indirection is what makes palettes, light/dark, `NO_COLOR` and a
//! monochrome fallback one lookup table instead of edits all over the UI.
//!
//! The default theme deliberately uses the 16 ANSI colours, which are *theme
//! variables* owned by the user's terminal — their "red" already matches the
//! rest of their setup. Named palettes opt into fixed 24-bit colours.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,

    // Text tiers
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_alt: Color,

    // Status
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,

    // Chrome
    pub border: Color,
    pub border_focus: Color,
    pub surface: Color,

    /// Per-kind block fills. A block of content on a slightly tinted background
    /// reads as a unit without spending two rows on a border, which is what
    /// makes a transcript feel composed rather than printed.
    /// `None` means "no fill" — used by the ANSI and mono themes, which cannot
    /// know whether the terminal is light or dark.
    pub bg_user: Option<Color>,
    pub bg_tool: Option<Color>,
    pub bg_tool_err: Option<Color>,
    pub bg_panel: Option<Color>,
    pub bg_selected: Option<Color>,
    /// Headings and list bullets. Warm by convention.
    pub heading: Color,
    /// Tool names in result headers.
    pub tool_title: Color,

    // Diff
    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_hunk: Color,

    // Syntax
    pub syn_keyword: Color,
    pub syn_string: Color,
    pub syn_number: Color,
    pub syn_comment: Color,
    pub syn_func: Color,
    pub syn_type: Color,

    /// False for the monochrome theme: rendering then leans on bold/dim only.
    pub colored: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Uses the terminal's own 16 colours, so it matches whatever the user runs.
pub const ANSI: Theme = Theme {
    name: "ansi",
    text: Color::Reset,
    muted: Color::DarkGray,
    accent: Color::Cyan,
    accent_alt: Color::Magenta,
    success: Color::Green,
    warning: Color::Yellow,
    error: Color::Red,
    info: Color::Blue,
    border: Color::DarkGray,
    border_focus: Color::Cyan,
    surface: Color::Reset,
    bg_user: None,
    bg_tool: None,
    bg_tool_err: None,
    bg_panel: None,
    bg_selected: Some(Color::DarkGray),
    heading: Color::Yellow,
    tool_title: Color::Yellow,
    diff_add: Color::Green,
    diff_del: Color::Red,
    diff_hunk: Color::Cyan,
    syn_keyword: Color::Magenta,
    syn_string: Color::Green,
    syn_number: Color::LightYellow,
    syn_comment: Color::DarkGray,
    syn_func: Color::Blue,
    syn_type: Color::LightCyan,
    colored: true,
};

/// No colour at all: hierarchy comes from bold and dim only. Used for
/// `NO_COLOR`, dumb terminals, and users who want it.
pub const MONO: Theme = Theme {
    name: "mono",
    text: Color::Reset,
    muted: Color::DarkGray,
    accent: Color::Reset,
    accent_alt: Color::Reset,
    success: Color::Reset,
    warning: Color::Reset,
    error: Color::Reset,
    info: Color::Reset,
    border: Color::DarkGray,
    border_focus: Color::Reset,
    surface: Color::Reset,
    bg_user: None,
    bg_tool: None,
    bg_tool_err: None,
    bg_panel: None,
    bg_selected: None,
    heading: Color::Reset,
    tool_title: Color::Reset,
    diff_add: Color::Reset,
    diff_del: Color::Reset,
    diff_hunk: Color::DarkGray,
    syn_keyword: Color::Reset,
    syn_string: Color::Reset,
    syn_number: Color::Reset,
    syn_comment: Color::DarkGray,
    syn_func: Color::Reset,
    syn_type: Color::Reset,
    colored: false,
};

/// A dark theme in the style of oh-my-pi: amber accent, per-kind block tints,
/// and VS Code Dark+ syntax colours. This is the default because the block fills
/// are what give the transcript its composed look, and fills need known colours.
pub const DARK: Theme = Theme {
    name: "dark",
    text: rgb(220, 223, 228),
    muted: rgb(119, 125, 136),
    accent: rgb(254, 188, 56),
    accent_alt: rgb(178, 129, 214),
    success: rgb(137, 210, 129),
    warning: rgb(228, 192, 15),
    error: rgb(252, 58, 75),
    info: rgb(0, 136, 250),
    border: rgb(61, 66, 74),
    border_focus: rgb(23, 143, 185),
    surface: rgb(29, 33, 41),
    bg_user: Some(rgb(34, 29, 26)),
    bg_tool: Some(rgb(22, 26, 31)),
    bg_tool_err: Some(rgb(41, 29, 29)),
    bg_panel: Some(rgb(29, 33, 41)),
    bg_selected: Some(rgb(49, 54, 63)),
    heading: rgb(254, 188, 56),
    tool_title: rgb(254, 188, 56),
    diff_add: rgb(137, 210, 129),
    diff_del: rgb(252, 58, 75),
    diff_hunk: rgb(119, 125, 136),
    syn_keyword: rgb(86, 156, 214),
    syn_string: rgb(206, 145, 120),
    syn_number: rgb(181, 206, 168),
    syn_comment: rgb(106, 153, 85),
    syn_func: rgb(220, 220, 170),
    syn_type: rgb(78, 201, 176),
    colored: true,
};

pub const CATPPUCCIN_MOCHA: Theme = Theme {
    name: "catppuccin-mocha",
    text: rgb(205, 214, 244),
    muted: rgb(108, 112, 134),
    accent: rgb(137, 180, 250),
    accent_alt: rgb(203, 166, 247),
    success: rgb(166, 227, 161),
    warning: rgb(249, 226, 175),
    error: rgb(243, 139, 168),
    info: rgb(137, 220, 235),
    border: rgb(69, 71, 90),
    border_focus: rgb(137, 180, 250),
    surface: rgb(49, 50, 68),
    bg_user: Some(rgb(35, 28, 36)),
    bg_tool: Some(rgb(28, 30, 40)),
    bg_tool_err: Some(rgb(45, 26, 32)),
    bg_panel: Some(rgb(30, 30, 46)),
    bg_selected: Some(rgb(49, 50, 68)),
    heading: rgb(249, 226, 175),
    tool_title: rgb(249, 226, 175),
    diff_add: rgb(166, 227, 161),
    diff_del: rgb(243, 139, 168),
    diff_hunk: rgb(137, 220, 235),
    syn_keyword: rgb(203, 166, 247),
    syn_string: rgb(166, 227, 161),
    syn_number: rgb(250, 179, 135),
    syn_comment: rgb(108, 112, 134),
    syn_func: rgb(137, 180, 250),
    syn_type: rgb(249, 226, 175),
    colored: true,
};

pub const GRUVBOX_DARK: Theme = Theme {
    name: "gruvbox-dark",
    text: rgb(235, 219, 178),
    muted: rgb(146, 131, 116),
    accent: rgb(131, 165, 152),
    accent_alt: rgb(211, 134, 155),
    success: rgb(184, 187, 38),
    warning: rgb(250, 189, 47),
    error: rgb(251, 73, 52),
    info: rgb(131, 165, 152),
    border: rgb(80, 73, 69),
    border_focus: rgb(131, 165, 152),
    surface: rgb(60, 56, 54),
    bg_user: Some(rgb(44, 36, 28)),
    bg_tool: Some(rgb(36, 38, 34)),
    bg_tool_err: Some(rgb(50, 30, 26)),
    bg_panel: Some(rgb(40, 40, 40)),
    bg_selected: Some(rgb(80, 73, 69)),
    heading: rgb(250, 189, 47),
    tool_title: rgb(250, 189, 47),
    diff_add: rgb(184, 187, 38),
    diff_del: rgb(251, 73, 52),
    diff_hunk: rgb(131, 165, 152),
    syn_keyword: rgb(251, 73, 52),
    syn_string: rgb(184, 187, 38),
    syn_number: rgb(211, 134, 155),
    syn_comment: rgb(146, 131, 116),
    syn_func: rgb(142, 192, 124),
    syn_type: rgb(250, 189, 47),
    colored: true,
};

pub const NORD: Theme = Theme {
    name: "nord",
    text: rgb(216, 222, 233),
    muted: rgb(97, 110, 136),
    accent: rgb(136, 192, 208),
    accent_alt: rgb(180, 142, 173),
    success: rgb(163, 190, 140),
    warning: rgb(235, 203, 139),
    error: rgb(191, 97, 106),
    info: rgb(129, 161, 193),
    border: rgb(67, 76, 94),
    border_focus: rgb(136, 192, 208),
    surface: rgb(59, 66, 82),
    bg_user: Some(rgb(42, 44, 51)),
    bg_tool: Some(rgb(40, 46, 56)),
    bg_tool_err: Some(rgb(52, 38, 40)),
    bg_panel: Some(rgb(46, 52, 64)),
    bg_selected: Some(rgb(67, 76, 94)),
    heading: rgb(235, 203, 139),
    tool_title: rgb(235, 203, 139),
    diff_add: rgb(163, 190, 140),
    diff_del: rgb(191, 97, 106),
    diff_hunk: rgb(136, 192, 208),
    syn_keyword: rgb(129, 161, 193),
    syn_string: rgb(163, 190, 140),
    syn_number: rgb(180, 142, 173),
    syn_comment: rgb(97, 110, 136),
    syn_func: rgb(136, 192, 208),
    syn_type: rgb(143, 188, 187),
    colored: true,
};

pub const TOKYO_NIGHT: Theme = Theme {
    name: "tokyo-night",
    text: rgb(192, 202, 245),
    muted: rgb(86, 95, 137),
    accent: rgb(122, 162, 247),
    accent_alt: rgb(187, 154, 247),
    success: rgb(158, 206, 106),
    warning: rgb(224, 175, 104),
    error: rgb(247, 118, 142),
    info: rgb(125, 207, 255),
    border: rgb(59, 66, 97),
    border_focus: rgb(122, 162, 247),
    surface: rgb(41, 46, 66),
    bg_user: Some(rgb(35, 32, 44)),
    bg_tool: Some(rgb(28, 32, 48)),
    bg_tool_err: Some(rgb(48, 28, 36)),
    bg_panel: Some(rgb(31, 35, 53)),
    bg_selected: Some(rgb(59, 66, 97)),
    heading: rgb(224, 175, 104),
    tool_title: rgb(224, 175, 104),
    diff_add: rgb(158, 206, 106),
    diff_del: rgb(247, 118, 142),
    diff_hunk: rgb(125, 207, 255),
    syn_keyword: rgb(187, 154, 247),
    syn_string: rgb(158, 206, 106),
    syn_number: rgb(255, 158, 100),
    syn_comment: rgb(86, 95, 137),
    syn_func: rgb(122, 162, 247),
    syn_type: rgb(42, 195, 222),
    colored: true,
};

pub const DRACULA: Theme = Theme {
    name: "dracula",
    text: rgb(248, 248, 242),
    muted: rgb(98, 114, 164),
    accent: rgb(139, 233, 253),
    accent_alt: rgb(255, 121, 198),
    success: rgb(80, 250, 123),
    warning: rgb(241, 250, 140),
    error: rgb(255, 85, 85),
    info: rgb(139, 233, 253),
    border: rgb(68, 71, 90),
    border_focus: rgb(189, 147, 249),
    surface: rgb(68, 71, 90),
    bg_user: Some(rgb(45, 38, 48)),
    bg_tool: Some(rgb(38, 40, 54)),
    bg_tool_err: Some(rgb(54, 34, 40)),
    bg_panel: Some(rgb(40, 42, 54)),
    bg_selected: Some(rgb(68, 71, 90)),
    heading: rgb(241, 250, 140),
    tool_title: rgb(241, 250, 140),
    diff_add: rgb(80, 250, 123),
    diff_del: rgb(255, 85, 85),
    diff_hunk: rgb(139, 233, 253),
    syn_keyword: rgb(255, 121, 198),
    syn_string: rgb(241, 250, 140),
    syn_number: rgb(189, 147, 249),
    syn_comment: rgb(98, 114, 164),
    syn_func: rgb(80, 250, 123),
    syn_type: rgb(139, 233, 253),
    colored: true,
};

/// A vibrant, high-saturation dark palette — electric cyan, hot magenta, and
/// lime on a deep indigo, in the synthwave/neon vein. The most colourful theme
/// koda ships; good for a terminal that can do true colour and a user who wants
/// the UI to pop.
pub const NEON: Theme = Theme {
    name: "neon",
    text: rgb(233, 236, 255),
    muted: rgb(122, 130, 180),
    accent: rgb(0, 245, 212),      // electric cyan-green
    accent_alt: rgb(255, 71, 205), // hot magenta
    success: rgb(88, 255, 156),    // lime
    warning: rgb(255, 199, 64),    // amber
    error: rgb(255, 74, 110),      // neon red-pink
    info: rgb(84, 173, 255),       // bright blue
    border: rgb(58, 62, 110),
    border_focus: rgb(0, 245, 212),
    surface: rgb(26, 27, 54),
    bg_user: Some(rgb(38, 26, 54)),     // faint magenta tint
    bg_tool: Some(rgb(20, 24, 52)),     // deep indigo
    bg_tool_err: Some(rgb(48, 22, 40)), // faint red tint
    bg_panel: Some(rgb(24, 26, 58)),
    bg_selected: Some(rgb(52, 40, 92)),
    heading: rgb(0, 245, 212),
    tool_title: rgb(255, 71, 205),
    diff_add: rgb(88, 255, 156),
    diff_del: rgb(255, 74, 110),
    diff_hunk: rgb(178, 148, 255),
    syn_keyword: rgb(255, 71, 205),
    syn_string: rgb(88, 255, 156),
    syn_number: rgb(255, 199, 64),
    syn_comment: rgb(122, 130, 180),
    syn_func: rgb(0, 245, 212),
    syn_type: rgb(178, 148, 255),
    colored: true,
};

pub const ROSE_PINE: Theme = Theme {
    name: "rose-pine",
    text: rgb(224, 222, 244),
    muted: rgb(110, 106, 134),
    accent: rgb(156, 207, 216),
    accent_alt: rgb(196, 167, 231),
    success: rgb(49, 116, 143),
    warning: rgb(246, 193, 119),
    error: rgb(235, 111, 146),
    info: rgb(156, 207, 216),
    border: rgb(64, 61, 82),
    border_focus: rgb(196, 167, 231),
    surface: rgb(38, 35, 58),
    bg_user: Some(rgb(38, 33, 44)),
    bg_tool: Some(rgb(30, 30, 44)),
    bg_tool_err: Some(rgb(50, 30, 38)),
    bg_panel: Some(rgb(31, 29, 46)),
    bg_selected: Some(rgb(64, 61, 82)),
    heading: rgb(246, 193, 119),
    tool_title: rgb(246, 193, 119),
    diff_add: rgb(156, 207, 216),
    diff_del: rgb(235, 111, 146),
    diff_hunk: rgb(196, 167, 231),
    syn_keyword: rgb(196, 167, 231),
    syn_string: rgb(246, 193, 119),
    syn_number: rgb(235, 188, 186),
    syn_comment: rgb(110, 106, 134),
    syn_func: rgb(49, 116, 143),
    syn_type: rgb(156, 207, 216),
    colored: true,
};

pub const SOLARIZED_LIGHT: Theme = Theme {
    name: "solarized-light",
    // base01 deepened a shade: on the panel fill it measured 4.49:1, just
    // under WCAG AA for body text.
    text: rgb(76, 96, 102),
    muted: rgb(147, 161, 161),
    accent: rgb(38, 139, 210),
    accent_alt: rgb(211, 54, 130),
    success: rgb(133, 153, 0),
    warning: rgb(181, 137, 0),
    error: rgb(220, 50, 47),
    info: rgb(42, 161, 152),
    border: rgb(203, 199, 180),
    border_focus: rgb(38, 139, 210),
    surface: rgb(238, 232, 213),
    bg_user: Some(rgb(245, 238, 219)),
    bg_tool: Some(rgb(238, 236, 224)),
    bg_tool_err: Some(rgb(248, 232, 228)),
    bg_panel: Some(rgb(240, 234, 214)),
    bg_selected: Some(rgb(219, 214, 196)),
    heading: rgb(181, 137, 0),
    tool_title: rgb(181, 137, 0),
    diff_add: rgb(133, 153, 0),
    diff_del: rgb(220, 50, 47),
    diff_hunk: rgb(38, 139, 210),
    syn_keyword: rgb(133, 153, 0),
    syn_string: rgb(42, 161, 152),
    syn_number: rgb(211, 54, 130),
    syn_comment: rgb(147, 161, 161),
    syn_func: rgb(38, 139, 210),
    syn_type: rgb(181, 137, 0),
    colored: true,
};

/// Kanagawa (wave): ink-wash blues and autumn tones, after Hokusai's Great Wave.
pub const KANAGAWA: Theme = Theme {
    name: "kanagawa",
    text: rgb(220, 215, 186),
    muted: rgb(114, 113, 105),
    accent: rgb(126, 156, 216),
    accent_alt: rgb(149, 127, 184),
    success: rgb(152, 187, 108),
    warning: rgb(230, 195, 132),
    error: rgb(228, 104, 118),
    info: rgb(127, 180, 202),
    border: rgb(54, 54, 70),
    border_focus: rgb(126, 156, 216),
    surface: rgb(45, 79, 103),
    bg_user: Some(rgb(39, 38, 50)),
    bg_tool: Some(rgb(25, 25, 33)),
    bg_tool_err: Some(rgb(55, 40, 49)),
    bg_panel: Some(rgb(31, 31, 40)),
    bg_selected: Some(rgb(45, 79, 103)),
    heading: rgb(230, 195, 132),
    tool_title: rgb(230, 195, 132),
    diff_add: rgb(152, 187, 108),
    diff_del: rgb(228, 104, 118),
    diff_hunk: rgb(127, 180, 202),
    syn_keyword: rgb(149, 127, 184),
    syn_string: rgb(152, 187, 108),
    syn_number: rgb(210, 126, 153),
    syn_comment: rgb(114, 113, 105),
    syn_func: rgb(126, 156, 216),
    syn_type: rgb(122, 168, 159),
    colored: true,
};

/// Everforest (dark, medium): soft greens and warm earth, easy on long sessions.
pub const EVERFOREST: Theme = Theme {
    name: "everforest",
    text: rgb(211, 198, 170),
    muted: rgb(133, 146, 137),
    accent: rgb(127, 187, 179),
    accent_alt: rgb(214, 153, 182),
    success: rgb(167, 192, 128),
    warning: rgb(219, 188, 127),
    error: rgb(230, 126, 128),
    info: rgb(131, 192, 146),
    border: rgb(71, 82, 88),
    border_focus: rgb(127, 187, 179),
    surface: rgb(84, 58, 72),
    bg_user: Some(rgb(57, 60, 68)),
    bg_tool: Some(rgb(37, 43, 48)),
    bg_tool_err: Some(rgb(67, 62, 67)),
    bg_panel: Some(rgb(45, 53, 59)),
    bg_selected: Some(rgb(84, 58, 72)),
    heading: rgb(219, 188, 127),
    tool_title: rgb(219, 188, 127),
    diff_add: rgb(167, 192, 128),
    diff_del: rgb(230, 126, 128),
    diff_hunk: rgb(131, 192, 146),
    syn_keyword: rgb(230, 126, 128),
    syn_string: rgb(167, 192, 128),
    syn_number: rgb(214, 153, 182),
    syn_comment: rgb(133, 146, 137),
    syn_func: rgb(167, 192, 128),
    syn_type: rgb(219, 188, 127),
    colored: true,
};

/// One Dark: Atom's classic, balanced and familiar.
pub const ONE_DARK: Theme = Theme {
    name: "one-dark",
    text: rgb(171, 178, 191),
    muted: rgb(92, 99, 112),
    accent: rgb(97, 175, 239),
    accent_alt: rgb(198, 120, 221),
    success: rgb(152, 195, 121),
    warning: rgb(229, 192, 123),
    error: rgb(232, 102, 113),
    info: rgb(86, 182, 194),
    border: rgb(59, 63, 76),
    border_focus: rgb(97, 175, 239),
    surface: rgb(57, 63, 74),
    bg_user: Some(rgb(51, 49, 64)),
    bg_tool: Some(rgb(33, 36, 43)),
    bg_tool_err: Some(rgb(63, 51, 59)),
    bg_panel: Some(rgb(40, 44, 52)),
    bg_selected: Some(rgb(57, 63, 74)),
    heading: rgb(229, 192, 123),
    tool_title: rgb(229, 192, 123),
    diff_add: rgb(152, 195, 121),
    diff_del: rgb(232, 102, 113),
    diff_hunk: rgb(86, 182, 194),
    syn_keyword: rgb(198, 120, 221),
    syn_string: rgb(152, 195, 121),
    syn_number: rgb(209, 154, 102),
    syn_comment: rgb(92, 99, 112),
    syn_func: rgb(97, 175, 239),
    syn_type: rgb(229, 192, 123),
    colored: true,
};

/// GitHub Dark: the colours of github.com at night.
pub const GITHUB_DARK: Theme = Theme {
    name: "github-dark",
    text: rgb(230, 237, 243),
    muted: rgb(132, 141, 151),
    accent: rgb(88, 166, 255),
    accent_alt: rgb(188, 140, 255),
    success: rgb(63, 185, 80),
    warning: rgb(210, 153, 34),
    error: rgb(248, 81, 73),
    info: rgb(121, 192, 255),
    border: rgb(48, 54, 61),
    border_focus: rgb(88, 166, 255),
    surface: rgb(31, 41, 55),
    bg_user: Some(rgb(25, 26, 39)),
    bg_tool: Some(rgb(11, 14, 19)),
    bg_tool_err: Some(rgb(41, 25, 29)),
    bg_panel: Some(rgb(13, 17, 23)),
    bg_selected: Some(rgb(31, 41, 55)),
    heading: rgb(255, 166, 87),
    tool_title: rgb(255, 166, 87),
    diff_add: rgb(63, 185, 80),
    diff_del: rgb(248, 81, 73),
    diff_hunk: rgb(121, 192, 255),
    syn_keyword: rgb(255, 123, 114),
    syn_string: rgb(165, 214, 255),
    syn_number: rgb(121, 192, 255),
    syn_comment: rgb(139, 148, 158),
    syn_func: rgb(210, 168, 255),
    syn_type: rgb(255, 166, 87),
    colored: true,
};

/// Ayu Mirage: muted slate with bright, warm highlights.
pub const AYU_MIRAGE: Theme = Theme {
    name: "ayu-mirage",
    text: rgb(204, 202, 194),
    muted: rgb(112, 122, 140),
    accent: rgb(115, 208, 255),
    accent_alt: rgb(223, 191, 255),
    success: rgb(135, 217, 108),
    warning: rgb(255, 204, 102),
    error: rgb(242, 135, 121),
    info: rgb(149, 230, 203),
    border: rgb(52, 59, 76),
    border_focus: rgb(115, 208, 255),
    surface: rgb(51, 65, 94),
    bg_user: Some(rgb(44, 47, 62)),
    bg_tool: Some(rgb(25, 30, 39)),
    bg_tool_err: Some(rgb(56, 48, 57)),
    bg_panel: Some(rgb(31, 36, 48)),
    bg_selected: Some(rgb(51, 65, 94)),
    heading: rgb(255, 209, 115),
    tool_title: rgb(255, 209, 115),
    diff_add: rgb(135, 217, 108),
    diff_del: rgb(242, 135, 121),
    diff_hunk: rgb(149, 230, 203),
    syn_keyword: rgb(255, 173, 102),
    syn_string: rgb(213, 255, 128),
    syn_number: rgb(223, 191, 255),
    syn_comment: rgb(108, 122, 139),
    syn_func: rgb(255, 209, 115),
    syn_type: rgb(115, 208, 255),
    colored: true,
};

/// Night Owl: deep navy tuned for late-night reading.
pub const NIGHT_OWL: Theme = Theme {
    name: "night-owl",
    text: rgb(214, 222, 235),
    muted: rgb(99, 119, 119),
    accent: rgb(130, 170, 255),
    accent_alt: rgb(199, 146, 234),
    success: rgb(34, 218, 110),
    warning: rgb(236, 196, 141),
    error: rgb(239, 83, 80),
    info: rgb(33, 199, 168),
    border: rgb(29, 59, 83),
    border_focus: rgb(130, 170, 255),
    surface: rgb(29, 59, 83),
    bg_user: Some(rgb(15, 31, 53)),
    bg_tool: Some(rgb(1, 18, 32)),
    bg_tool_err: Some(rgb(30, 29, 44)),
    bg_panel: Some(rgb(1, 22, 39)),
    bg_selected: Some(rgb(29, 59, 83)),
    heading: rgb(255, 203, 139),
    tool_title: rgb(255, 203, 139),
    diff_add: rgb(34, 218, 110),
    diff_del: rgb(239, 83, 80),
    diff_hunk: rgb(33, 199, 168),
    syn_keyword: rgb(199, 146, 234),
    syn_string: rgb(236, 196, 141),
    syn_number: rgb(247, 140, 108),
    syn_comment: rgb(99, 119, 119),
    syn_func: rgb(130, 170, 255),
    syn_type: rgb(255, 203, 139),
    colored: true,
};

/// Catppuccin Latte: the light flavour of the most-used palette of 2026. Its
/// pale yellows and greys are deepened, in the same hue, to 3:1 on the base:
/// in a terminal they are body text, not accents on a page.
pub const CATPPUCCIN_LATTE: Theme = Theme {
    name: "catppuccin-latte",
    text: rgb(76, 79, 105),
    muted: rgb(134, 137, 155),
    accent: rgb(30, 102, 245),
    accent_alt: rgb(136, 57, 239),
    success: rgb(63, 157, 42),
    warning: rgb(195, 122, 24),
    error: rgb(210, 15, 57),
    info: rgb(4, 147, 204),
    border: rgb(188, 192, 204),
    border_focus: rgb(30, 102, 245),
    surface: rgb(204, 208, 218),
    bg_user: Some(rgb(232, 228, 245)),
    bg_tool: Some(rgb(227, 237, 244)),
    bg_tool_err: Some(rgb(236, 214, 222)),
    bg_panel: Some(rgb(239, 241, 245)),
    bg_selected: Some(rgb(204, 208, 218)),
    heading: rgb(234, 92, 11),
    tool_title: rgb(234, 92, 11),
    diff_add: rgb(63, 157, 42),
    diff_del: rgb(210, 15, 57),
    diff_hunk: rgb(4, 147, 204),
    syn_keyword: rgb(136, 57, 239),
    syn_string: rgb(63, 157, 42),
    syn_number: rgb(234, 92, 11),
    syn_comment: rgb(135, 139, 154),
    syn_func: rgb(30, 102, 245),
    syn_type: rgb(195, 122, 24),
    colored: true,
};

/// Rosé Pine Dawn: soho vibes, in daylight. Faint tones deepened to 3:1, as
/// for Latte.
pub const ROSE_PINE_DAWN: Theme = Theme {
    name: "rose-pine-dawn",
    text: rgb(70, 66, 97),
    muted: rgb(143, 138, 156),
    accent: rgb(40, 105, 131),
    accent_alt: rgb(144, 122, 169),
    success: rgb(86, 148, 159),
    warning: rgb(192, 127, 42),
    error: rgb(180, 99, 122),
    info: rgb(86, 148, 159),
    border: rgb(223, 218, 217),
    border_focus: rgb(40, 105, 131),
    surface: rgb(223, 218, 217),
    bg_user: Some(rgb(243, 235, 232)),
    bg_tool: Some(rgb(242, 239, 233)),
    bg_tool_err: Some(rgb(242, 227, 223)),
    bg_panel: Some(rgb(250, 244, 237)),
    bg_selected: Some(rgb(223, 218, 217)),
    heading: rgb(199, 120, 117),
    tool_title: rgb(199, 120, 117),
    diff_add: rgb(86, 148, 159),
    diff_del: rgb(180, 99, 122),
    diff_hunk: rgb(86, 148, 159),
    syn_keyword: rgb(40, 105, 131),
    syn_string: rgb(192, 127, 42),
    syn_number: rgb(199, 120, 117),
    syn_comment: rgb(143, 138, 156),
    syn_func: rgb(199, 120, 117),
    syn_type: rgb(86, 148, 159),
    colored: true,
};

/// Everforest (light, medium): the forest on paper. Faint tones deepened to
/// 3:1, as for Latte.
pub const EVERFOREST_LIGHT: Theme = Theme {
    name: "everforest-light",
    text: rgb(92, 106, 114),
    muted: rgb(132, 144, 130),
    accent: rgb(58, 148, 197),
    accent_alt: rgb(215, 101, 178),
    success: rgb(132, 152, 1),
    warning: rgb(187, 133, 0),
    error: rgb(248, 85, 82),
    info: rgb(50, 158, 118),
    border: rgb(230, 226, 204),
    border_focus: rgb(58, 148, 197),
    surface: rgb(230, 226, 204),
    bg_user: Some(rgb(251, 236, 224)),
    bg_tool: Some(rgb(243, 242, 222)),
    bg_tool_err: Some(rgb(252, 227, 210)),
    bg_panel: Some(rgb(253, 246, 227)),
    bg_selected: Some(rgb(230, 226, 204)),
    heading: rgb(220, 114, 33),
    tool_title: rgb(220, 114, 33),
    diff_add: rgb(132, 152, 1),
    diff_del: rgb(248, 85, 82),
    diff_hunk: rgb(50, 158, 118),
    syn_keyword: rgb(248, 85, 82),
    syn_string: rgb(132, 152, 1),
    syn_number: rgb(215, 101, 178),
    syn_comment: rgb(132, 144, 130),
    syn_func: rgb(132, 152, 1),
    syn_type: rgb(187, 133, 0),
    colored: true,
};

pub const THEMES: &[Theme] = &[
    DARK,
    NEON,
    ANSI,
    CATPPUCCIN_MOCHA,
    TOKYO_NIGHT,
    GRUVBOX_DARK,
    NORD,
    DRACULA,
    ROSE_PINE,
    KANAGAWA,
    EVERFOREST,
    ONE_DARK,
    GITHUB_DARK,
    AYU_MIRAGE,
    NIGHT_OWL,
    SOLARIZED_LIGHT,
    CATPPUCCIN_LATTE,
    ROSE_PINE_DAWN,
    EVERFOREST_LIGHT,
    MONO,
];

pub fn by_name(name: &str) -> Option<Theme> {
    let want = name.trim().to_ascii_lowercase();
    THEMES.iter().copied().find(|t| t.name == want)
}

pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

/// Resolve the theme for this run. `NO_COLOR` and `TERM=dumb` win over config,
/// per the no-color.org convention: an explicit user environment choice is not
/// something an app should override.
pub fn resolve(configured: &str) -> Theme {
    if colour_is_unwelcome() {
        return MONO;
    }
    named(configured)
}

/// Whether the environment has asked for no colour at all.
///
/// Split out from `named` so the palette lookup can be tested without the
/// ambient environment deciding the answer — koda runs child commands with
/// `NO_COLOR=1` and `TERM=dumb`, so its own suite used to fail when run from
/// inside koda, and would fail in any CI that sets either.
fn colour_is_unwelcome() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
        || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false)
}

/// The configured palette, ignoring the environment.
pub fn named(configured: &str) -> Theme {
    match configured.trim() {
        // Default to the vibrant NEON palette: the block fills that give the
        // transcript its shape need colours we can predict, and a colourful
        // default is what most users want out of the box.
        "" | "auto" => NEON,
        other => by_name(other).unwrap_or(NEON),
    }
}

impl Theme {
    pub fn fg(&self, c: Color) -> Style {
        Style::default().fg(c)
    }
    pub fn dim(&self) -> Style {
        Style::default().fg(self.muted)
    }
    pub fn body(&self) -> Style {
        Style::default().fg(self.text)
    }
    pub fn strong(&self) -> Style {
        Style::default().fg(self.text).add_modifier(Modifier::BOLD)
    }
    /// Emphasis that survives a monochrome theme.
    pub fn emphasis(&self, c: Color) -> Style {
        if self.colored {
            Style::default().fg(c)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }
}

// ---------------------------------------------------------------- glyphs

/// Blend two theme colours. Only meaningful for truecolor; indexed and named
/// colours have nothing to interpolate, so the blend snaps at the halfway point
/// rather than inventing a value the terminal would ignore.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let (r, g, bl) = crate::anim::lerp_rgb((r1, g1, b1), (r2, g2, b2), t);
            Color::Rgb(r, g, bl)
        }
        _ if t >= 0.5 => b,
        _ => a,
    }
}

/// Glyph sets. There is no way to detect a Nerd Font, so icons are opt-in and
/// the ASCII set exists for terminals that cannot render box drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    // Frames. Rounded corners read softer than sharp ones and are what modern
    // terminal apps use; the ASCII set falls back to `+`.
    pub corner_tl: &'static str,
    pub corner_tr: &'static str,
    pub corner_bl: &'static str,
    pub corner_br: &'static str,
    /// Vertical rail drawn beside grouped content.
    pub rail: &'static str,
    /// The thinking sweep: an out-and-back cycle whose endpoints dwell longer,
    /// which reads as breathing rather than as a mechanical loop.
    pub thinking: &'static [&'static str],
    /// Tool identity glyphs, shown once a call has settled.
    pub pencil: &'static str,
    pub magnify: &'static str,
    pub branch_arrow: &'static str,
    pub check_on: &'static str,
    /// Tree connectors for grouped lists.
    pub branch: &'static str,
    pub last: &'static str,
    pub ellipsis: &'static str,
    /// Selection marker in lists.
    pub pick: &'static str,
    /// Chevron between status segments.
    pub chevron: &'static str,
    /// Bullet for secondary list rows.
    pub dot: &'static str,
    /// Empty cell of a gauge.
    pub gauge_empty: char,
    /// Whether eighth-block characters are available for sub-cell gauges.
    pub fine_blocks: bool,
    /// The typing cursor at the end of a streaming reply.
    pub caret: &'static str,
    pub ok: &'static str,
    pub fail: &'static str,
    pub warning: &'static str,
    pub running: &'static str,
    pub pending: &'static str,
    pub prompt: &'static str,
    pub user_bar: &'static str,
    pub bullet: &'static str,
    pub arrow: &'static str,
    pub sep: &'static str,
    pub hline: &'static str,
    pub vline: &'static str,
    pub tree_mid: &'static str,
    pub tree_end: &'static str,
    pub scroll_track: &'static str,
    pub scroll_thumb: &'static str,
    pub spinner: &'static [&'static str],
    pub ready: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    thinking: &["·", "✻", "✽", "✶", "✳", "✢"],
    pencil: "✎",
    magnify: "⌕",
    branch_arrow: "⇶",
    check_on: "☑",
    branch: "├─",
    last: "└─",
    ellipsis: "…",
    corner_tl: "╭",
    corner_tr: "╮",
    corner_bl: "╰",
    corner_br: "╯",
    rail: "│",
    pick: "›",
    chevron: "❯",
    dot: "▪",
    gauge_empty: '░',
    fine_blocks: true,
    caret: "▋",
    ok: "✔",
    fail: "✘",
    warning: "⚠",
    running: "◐",
    pending: "◌",
    prompt: "❯",
    user_bar: "▌",
    bullet: "▪",
    arrow: "→",
    sep: "·",
    hline: "─",
    vline: "│",
    tree_mid: "├─",
    tree_end: "└─",
    scroll_track: "│",
    scroll_thumb: "█",
    // The heavier braille cycle (as used by oh-my-pi's tool status): reads as a
    // denser, busier "working" than the light ⠋⠙ dots.
    spinner: &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
    ready: "●",
};

pub const ASCII: Glyphs = Glyphs {
    thinking: &[".", "*", "+", "x", "*", "."],
    pencil: "~",
    magnify: "?",
    branch_arrow: ">>",
    check_on: "[x]",
    branch: "|-",
    last: "`-",
    ellipsis: "...",
    corner_tl: "+",
    corner_tr: "+",
    corner_bl: "+",
    corner_br: "+",
    rail: "|",
    pick: ">",
    chevron: ">",
    dot: "*",
    gauge_empty: '-',
    fine_blocks: false,
    caret: "_",
    ok: "+",
    fail: "x",
    warning: "!",
    running: "*",
    pending: "-",
    prompt: ">",
    user_bar: "|",
    bullet: "*",
    arrow: "->",
    sep: "-",
    hline: "-",
    vline: "|",
    tree_mid: "|-",
    tree_end: "`-",
    scroll_track: "|",
    scroll_thumb: "#",
    spinner: &["|", "/", "-", "\\"],
    ready: "o",
};

/// Unicode unless the locale says the terminal cannot handle it (Windows
/// Terminal says so by being there).
pub fn glyphs(configured: &str) -> Glyphs {
    match configured.trim() {
        "ascii" => ASCII,
        "unicode" => UNICODE,
        _ => {
            let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| {
                std::env::var(k)
                    .map(|v| {
                        v.to_ascii_uppercase().contains("UTF-8")
                            || v.to_ascii_uppercase().contains("UTF8")
                    })
                    .unwrap_or(false)
            });
            // Windows sets no locale variables at all. Windows Terminal draws
            // Unicode and emoji well; the old console host does not, and gets
            // the ASCII set.
            let windows_terminal = std::env::var_os("WT_SESSION").is_some();
            if utf8 || windows_terminal {
                UNICODE
            } else {
                ASCII
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_is_findable_by_name() {
        for t in THEMES {
            assert_eq!(by_name(t.name).map(|x| x.name), Some(t.name));
        }
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn unknown_theme_falls_back_to_the_default() {
        // `named`, not `resolve`: the latter answers MONO whenever NO_COLOR or
        // TERM=dumb is set, which is true of every command koda itself runs.
        assert_eq!(named("nonexistent").name, "neon");
        assert_eq!(named("").name, "neon");
        assert_eq!(named("auto").name, "neon");
        assert_eq!(named("nord").name, "nord");
        assert_eq!(named("ansi").name, "ansi");
    }

    #[test]
    fn only_themes_with_known_colours_use_block_fills() {
        // ANSI and mono cannot know whether the terminal is light or dark, so a
        // fill would be a coin flip.
        assert!(ANSI.bg_user.is_none());
        assert!(MONO.bg_user.is_none());
        assert!(DARK.bg_user.is_some());
        for t in THEMES {
            if t.bg_user.is_some() {
                assert!(t.bg_tool.is_some(), "{} has a partial fill set", t.name);
                assert!(t.bg_tool_err.is_some(), "{} has a partial fill set", t.name);
            }
        }
    }

    #[test]
    fn mono_theme_uses_no_colour() {
        const { assert!(!MONO.colored) };
        // Emphasis degrades to bold rather than vanishing.
        assert_eq!(MONO.emphasis(MONO.error).fg, None);
        assert!(MONO
            .emphasis(MONO.error)
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn ascii_glyphs_are_pure_ascii() {
        for g in [
            ASCII.ok,
            ASCII.fail,
            ASCII.running,
            ASCII.prompt,
            ASCII.hline,
            ASCII.tree_mid,
            ASCII.corner_tl,
            ASCII.corner_br,
            ASCII.rail,
            ASCII.pick,
        ] {
            assert!(g.is_ascii(), "{g:?} is not ascii");
        }
        assert!(ASCII.spinner.iter().all(|f| f.is_ascii()));
        assert!(ASCII.gauge_empty.is_ascii());
        const { assert!(!ASCII.fine_blocks, "ascii mode has no eighth blocks") };
    }

    #[test]
    fn explicit_glyph_choice_wins() {
        assert_eq!(glyphs("ascii").ok, ASCII.ok);
        assert_eq!(glyphs("unicode").ok, UNICODE.ok);
    }
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("not an rgb colour: {c:?}")
        };
        let ch = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Body text meets WCAG AA on every palette theme, and nothing it draws
    /// with nearly vanishes into the background -- which is what a mistyped
    /// hex, or a background colour pasted into a text slot, looks like.
    #[test]
    fn every_palette_theme_is_readable() {
        for t in THEMES {
            let Some(bg) = t.bg_panel else { continue };
            let ratio = contrast(t.text, bg);
            assert!(ratio >= 4.5, "{}: text is {ratio:.1}:1", t.name);
            for (slot, c) in [
                ("muted", t.muted),
                ("accent", t.accent),
                ("success", t.success),
                ("warning", t.warning),
                ("error", t.error),
                ("info", t.info),
                ("heading", t.heading),
                ("keyword", t.syn_keyword),
                ("string", t.syn_string),
                ("number", t.syn_number),
                ("comment", t.syn_comment),
                ("function", t.syn_func),
                ("type", t.syn_type),
            ] {
                let r = contrast(c, bg);
                assert!(r >= 2.0, "{}: {slot} is {r:.1}:1", t.name);
            }
            // A selected row and a tool block stand apart from the panel.
            for (slot, fill) in [("selected", t.bg_selected), ("tool", t.bg_tool)] {
                assert_ne!(
                    fill,
                    Some(bg),
                    "{}: {slot} fill is the panel colour",
                    t.name
                );
            }
        }
    }

    #[test]
    fn theme_names_are_unique_kebab_case() {
        let mut seen = std::collections::HashSet::new();
        for t in THEMES {
            assert!(seen.insert(t.name), "duplicate theme {}", t.name);
            assert!(
                t.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} is not kebab-case",
                t.name
            );
        }
        assert_eq!(names().len(), THEMES.len());
        assert_eq!(by_name(" Kanagawa ").map(|t| t.name), Some("kanagawa"));
    }

    #[test]
    fn light_themes_have_light_panels_and_dark_text() {
        for name in [
            "solarized-light",
            "catppuccin-latte",
            "rose-pine-dawn",
            "everforest-light",
        ] {
            let t = by_name(name).unwrap();
            let bg = t.bg_panel.unwrap();
            assert!(luminance(bg) > luminance(t.text), "{name}");
            assert!(luminance(bg) > 0.5, "{name} panel is not light");
        }
    }

    #[test]
    fn mix_blends_rgb_and_snaps_named_colours() {
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 50), 0.0), rgb(0, 0, 0));
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 50), 1.0), rgb(200, 100, 50));
        assert_eq!(mix(Color::Red, Color::Blue, 0.2), Color::Red);
        assert_eq!(mix(Color::Red, Color::Blue, 0.7), Color::Blue);
    }
}
