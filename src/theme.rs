//! Interface themes shared by `sb dash` and nvim.
//!
//! The dashboard is the source of truth: choosing a theme writes
//! `data/theme.json` (palette + nvim colorscheme), which the nvim config reads at
//! startup; running nvim instances are told to re-read it right away.
//! Palettes were taken from the colorschemes themselves (Normal, Comment,
//! Statement, String… highlight groups), so both sides show the same colours.

use crate::brain::Brain;
use anyhow::Result;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Theme {
    pub name: &'static str,
    pub label: &'static str,
    /// `:colorscheme` to use in nvim.
    pub colorscheme: &'static str,
    pub light: bool,
    pub bg: &'static str,
    pub panel: &'static str,
    pub sel: &'static str,
    pub text: &'static str,
    pub muted: &'static str,
    pub faint: &'static str,
    pub accent: &'static str,
    pub due: &'static str,
    pub warn: &'static str,
    pub ok: &'static str,
    pub area: &'static str,
    pub theme: &'static str,
    pub session: &'static str,
    pub concept: &'static str,
    pub resource: &'static str,
}

/// Kansō's muted accents, shared by its variants (only the background changes).
const fn kanso(
    name: &'static str,
    label: &'static str,
    cs: &'static str,
    bg: &'static str,
    panel: &'static str,
) -> Theme {
    Theme {
        name,
        label,
        colorscheme: cs,
        light: false,
        bg,
        panel,
        sel: "#393B44",
        text: "#C5C9C7",
        muted: "#909398",
        faint: "#5C6066",
        accent: "#938AA9",
        due: "#c4746e",
        warn: "#c4b28a",
        ok: "#8a9a7b",
        area: "#c4746e",
        theme: "#a292a3",
        session: "#6A9589",
        concept: "#658594",
        resource: "#87a987",
    }
}

pub const THEMES: &[Theme] = &[
    kanso("kanso-zen", "Kansō zen · black", "kanso-zen", "#000000", "#16191e"),
    kanso("kanso-ink", "Kansō ink", "kanso-ink", "#14171d", "#22262D"),
    kanso("kanso-mist", "Kansō mist", "kanso-mist", "#22262d", "#2a2c35"),
    Theme {
        name: "kanso-pearl",
        label: "Kansō pearl · light",
        colorscheme: "kanso-pearl",
        light: true,
        bg: "#f2f1ef",
        panel: "#e2e1df",
        sel: "#d2d1cd",
        text: "#22262d",
        muted: "#6d6d69",
        faint: "#9f9f99",
        accent: "#624c83",
        due: "#c84053",
        warn: "#836f4a",
        ok: "#6f894e",
        area: "#b35b79",
        theme: "#766b90",
        session: "#597b75",
        concept: "#4d699b",
        resource: "#6e915f",
    },
    Theme {
        name: "tokyonight-night",
        label: "Tokyo Night",
        colorscheme: "tokyonight-night",
        light: false,
        bg: "#1a1b26",
        panel: "#292e42",
        sel: "#283457",
        text: "#c0caf5",
        muted: "#565f89",
        faint: "#3b4261",
        accent: "#bb9af7",
        due: "#db4b4b",
        warn: "#e0af68",
        ok: "#9ece6a",
        area: "#ff9e64",
        theme: "#2ac3de",
        session: "#73daca",
        concept: "#7aa2f7",
        resource: "#9ece6a",
    },
    Theme {
        name: "tokyonight-moon",
        label: "Tokyo Night moon",
        colorscheme: "tokyonight-moon",
        light: false,
        bg: "#222436",
        panel: "#2f334d",
        sel: "#2d3f76",
        text: "#c8d3f5",
        muted: "#636da6",
        faint: "#3b4261",
        accent: "#c099ff",
        due: "#c53b53",
        warn: "#ffc777",
        ok: "#c3e88d",
        area: "#ff966c",
        theme: "#65bcff",
        session: "#4fd6be",
        concept: "#82aaff",
        resource: "#c3e88d",
    },
    Theme {
        name: "astrodark",
        label: "Astro dark",
        colorscheme: "astrodark",
        light: false,
        bg: "#1a1d23",
        panel: "#1e222a",
        sel: "#26343f",
        text: "#adb0bb",
        muted: "#696c76",
        faint: "#3a3e47",
        accent: "#dd97f1",
        due: "#f8747e",
        warn: "#d09214",
        ok: "#87c05f",
        area: "#dfab25",
        theme: "#5eb7ff",
        session: "#4ac2b8",
        concept: "#5eb7ff",
        resource: "#87c05f",
    },
    Theme {
        name: "eldritch",
        label: "Eldritch",
        colorscheme: "eldritch",
        light: false,
        bg: "#212337",
        panel: "#292e42",
        sel: "#3b4261",
        text: "#ebfafa",
        muted: "#7081d0",
        faint: "#4a5279",
        accent: "#a48cf2",
        due: "#f16c75",
        warn: "#f7c67f",
        ok: "#37f499",
        area: "#f16c75",
        theme: "#04d1f9",
        session: "#04d1f9",
        concept: "#f265b5",
        resource: "#f7c67f",
    },
    Theme {
        name: "morta",
        label: "Morta",
        colorscheme: "morta",
        light: false,
        bg: "#1b1c29",
        panel: "#292b3e",
        sel: "#2f3555",
        text: "#d2dafd",
        muted: "#8086a3",
        faint: "#606980",
        accent: "#bc95f9",
        due: "#f7678c",
        warn: "#e0af68",
        ok: "#9ece6a",
        area: "#f7678c",
        theme: "#bc95f9",
        session: "#2ac3de",
        concept: "#7aa2f7",
        resource: "#9ece6a",
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn current() -> &'static Theme {
    &THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

pub fn index() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn by_name(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name == name)
}

/// Load the saved theme (`data/theme.json`), default kanso-zen.
pub fn load(brain: &Brain) {
    let name = std::fs::read_to_string(brain.home.join("data/theme.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["name"].as_str().map(String::from));
    if let Some(i) = name.as_deref().and_then(by_name) {
        CURRENT.store(i, Ordering::Relaxed);
    }
}

/// Show a theme in this process only (dashboard preview), without saving it.
pub fn preview(i: usize) {
    CURRENT.store(i.min(THEMES.len() - 1), Ordering::Relaxed);
}

/// Switch theme, save it for nvim, and ask running nvim instances to apply it.
pub fn set(brain: &Brain, i: usize) -> Result<usize> {
    let i = i.min(THEMES.len() - 1);
    CURRENT.store(i, Ordering::Relaxed);
    crate::util::atomic_write(&brain.home.join("data/theme.json"), &serde_json::to_string_pretty(&THEMES[i])?)?;
    Ok(notify_nvim())
}

/// Every nvim server socket we can reach ($NVIM and $XDG_RUNTIME_DIR/nvim.*).
fn nvim_sockets() -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = vec![];
    if let Ok(s) = std::env::var("NVIM") {
        v.push(s.into());
    }
    // Linux puts nvim's default sockets in $XDG_RUNTIME_DIR (/run/user/<uid>); other
    // systems only get the $NVIM one (the nvim that runs `sb dash`).
    #[cfg(unix)]
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    #[cfg(not(unix))]
    let run = String::new();
    if let Ok(rd) = std::fs::read_dir(&run) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("nvim.") && !v.contains(&e.path()) {
                v.push(e.path());
            }
        }
    }
    v
}

/// Ask each running nvim to re-read data/theme.json (`SbTheme()` in the nvim config).
/// Returns how many answered. Runs in the background, never blocks the UI.
fn notify_nvim() -> usize {
    let socks = nvim_sockets();
    let n = socks.len();
    std::thread::spawn(move || {
        for s in socks {
            let _ = std::process::Command::new("nvim")
                .arg("--server")
                .arg(&s)
                .args(["--remote-expr", "luaeval('SbTheme and SbTheme() or 0')"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    });
    n
}

fn rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    (p(0), p(2), p(4))
}

/// Mix `a` towards `b` (`t` in 0..1).
pub fn mix(a: &str, b: &str, t: f32) -> (u8, u8, u8) {
    let (a, b) = (rgb(a), rgb(b));
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

pub fn hex(s: &str) -> (u8, u8, u8) {
    rgb(s)
}
