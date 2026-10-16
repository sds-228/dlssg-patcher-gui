use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;

use crate::download::{check_for_update, ensure_mod_files, version_cmp, ModFiles, UpdateInfo};
use crate::gpu::{driver_note, query_gpu, Arch, GpuInfo};
use crate::manifest::{
    exe_key, find_manifest_game, find_manifest_game_by_appid, load_manifest_reporting, load_state,
    save_state, ManualGame, Manifest, ManifestGame, PatchRecord, State,
};
use crate::patcher::{self, InstallRequest};
use crate::anticheat;
use crate::launchers;
use crate::steam::{autodetect_render_exe, has_any_exe, InstalledGame};

// ---- theme palette ----
const BG: egui::Color32 = egui::Color32::from_rgb(30, 34, 40);
const PANEL: egui::Color32 = egui::Color32::from_rgb(34, 38, 45);
const CARD: egui::Color32 = egui::Color32::from_rgb(41, 47, 56);
const CARD_HOVER: egui::Color32 = egui::Color32::from_rgb(49, 55, 65);
const BORDER: egui::Color32 = egui::Color32::from_rgb(66, 72, 82);
const TEXT: egui::Color32 = egui::Color32::from_rgb(236, 240, 244);
const MUTED: egui::Color32 = egui::Color32::from_rgb(168, 175, 184);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(118, 185, 0);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(74, 194, 107);
const DETECTED: egui::Color32 = egui::Color32::from_rgb(96, 168, 255);
const WARN: egui::Color32 = egui::Color32::from_rgb(210, 153, 34);
const ERROR: egui::Color32 = egui::Color32::from_rgb(248, 81, 73);
const NEO_GREEN: egui::Color32 = egui::Color32::from_rgb(72, 232, 132);

// activity console (terminal-style) palette
const TERM_BG: egui::Color32 = egui::Color32::from_rgb(26, 26, 26); // #1a1a1a
const TERM_BORDER: egui::Color32 = egui::Color32::from_rgb(56, 70, 56);
const TERM_GREEN: egui::Color32 = egui::Color32::from_rgb(139, 224, 139);
const TERM_DIM: egui::Color32 = egui::Color32::from_rgb(116, 138, 116);

/// Decide what we can honestly claim about Frame Generation support.
///
/// Two independent signals, neither of which promises the patch will work:
///
/// 1. NVIDIA's own list says the title ships Frame Generation.
/// 2. `nvngx_dlssg.dll` sits next to the render exe.
///
/// Either earns "Supported". Neither means "Unknown" -- never "unsupported",
/// because this check produces false negatives (renamed folder, DLL in a sibling
/// directory, an unusual loader).
fn support_tag(
    manifest_game: Option<&ManifestGame>,
    render_exe: Option<&PathBuf>,
) -> (String, egui::Color32, String) {
    if let Some(m) = manifest_game {
        if !crate::download::PROXY_NAMES.contains(&m.proxy.as_str()) {
            return (
                format!("Needs {}", m.proxy),
                WARN,
                format!("Requires {} (not one of the bundled proxy DLLs).", m.proxy),
            );
        }
        let tip = if m.confident_match() {
            format!("Listed in NVIDIA's DLSS Frame Generation catalog ({}).", m.name)
        } else {
            format!("Matched to \"{}\" in NVIDIA's catalog (fuzzy match).", m.name)
        };
        return ("Supported".to_string(), SUCCESS, tip);
    }

    let Some(exe) = render_exe else {
        return (
            "\u{2014}".to_string(),
            MUTED,
            "Executable not located yet. Click Locate to select the game's .exe.".to_string(),
        );
    };

    if has_fg_dll(exe) {
        (
            "Supported".to_string(),
            SUCCESS,
            "nvngx_dlssg.dll found next to the game executable.".to_string(),
        )
    } else {
        (
            "Unknown".to_string(),
            DETECTED,
            "Not in NVIDIA's catalog and nvngx_dlssg.dll was not found next to the .exe. Patching may still work."
                .to_string(),
        )
    }
}

/// Classify a console line by its content, terminal edition: bright green for
/// completed actions and positives, dim for in-progress / informational lines,
/// red for errors.
fn log_line_color(line: &str) -> egui::Color32 {
    let lower = line.to_ascii_lowercase();
    if line.starts_with("Error") || lower.contains("failed") || lower.contains("unavailable") || lower.contains("cannot") || lower.contains("missing:") {
        ERROR
    } else if is_positive_action(&lower) {
        TERM_GREEN
    } else {
        TERM_DIM
    }
}

/// True for lines reporting a finished positive action (green).
fn is_positive_action(lower: &str) -> bool {
    lower.contains("patched") || lower.contains("unpatched") || lower.contains("placed")
        || lower.contains("complete") || lower.contains("up to date")
        || lower.contains("ready") || lower.contains("found") || lower.contains("added")
        || lower.contains("gpu:") || lower.contains("started")
        || lower.contains("games (") || lower.contains("source:")
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = BG;
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.code_bg_color = CARD;
    v.hyperlink_color = DETECTED;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.28);

    let wv = |bg: egui::Color32, stroke: egui::Color32| {
        use egui::style::WidgetVisuals;
        WidgetVisuals {
            bg_fill: bg,
            weak_bg_fill: bg,
            bg_stroke: egui::Stroke::new(1.0_f32, stroke),
            corner_radius: egui::CornerRadius::same(6),
            fg_stroke: egui::Stroke::new(1.0_f32, TEXT),
            expansion: 0.0,
        }
    };
    v.widgets.noninteractive = wv(CARD, BORDER);
    v.widgets.inactive = wv(CARD, BORDER);
    v.widgets.hovered = wv(CARD_HOVER, ACCENT.gamma_multiply(0.5));
    v.widgets.active = wv(CARD_HOVER, ACCENT);
    v.widgets.open = wv(CARD_HOVER, ACCENT);

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.indent = 16.0;
    style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(22.0));
    style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    ctx.set_style(style);
}

fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(egui::CornerRadius::same(8))
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .inner_margin(egui::Margin::symmetric(14, 10))
}

/// Short amber line naming the integrated GPU the display is currently on, shown
/// when the supported NVIDIA card was found only through the fallback.
fn hybrid_note(igpu: &str) -> String {
    let clean = igpu.replace("(TM)", "").replace("(R)", "");
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("Running on {clean} (iGPU)")
}

/// Short amber line for the rare case where a supported card was found through
/// the Windows fallback but no integrated adapter was identified.
fn wmi_only_note() -> String {
    "Detected via Windows - nvidia-smi could not query it".to_string()
}

/// Visual tone for a tinted button.
///
/// `egui::Button::fill` / `.stroke` *replace* the state visuals
/// (`button.rs`: `fill.unwrap_or(visuals.weak_bg_fill)`, `stroke.unwrap_or(visuals.bg_stroke)`),
/// so a hand-tinted button never changes on hover. Instead a tone drives
/// `ui.visuals_mut().widgets`, which `Button` resolves per interaction state - so
/// tinted buttons glow exactly like the plain ones.
#[derive(Clone, Copy)]
enum BtnTone {
    /// Green filled affirmative button (Patch, Repatch, Relink, Update).
    AccentFill,
    /// Dark card button with a red border and red text; glows bright red on
    /// hover (Unpatch, Forget, X, Eraser…).
    DangerOutline,
    /// Solid red confirmation button for destructive modals (Erase DLSSG).
    DangerFill,
}

fn apply_tone(ui: &mut egui::Ui, tone: BtnTone) {
    let w = &mut ui.visuals_mut().widgets;
    match tone {
        BtnTone::AccentFill => {
            let hover_fill = egui::Color32::from_rgb(138, 212, 16);
            w.inactive.bg_fill = ACCENT;
            w.inactive.weak_bg_fill = ACCENT;
            w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
            w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, BG);

            w.hovered.bg_fill = hover_fill;
            w.hovered.weak_bg_fill = hover_fill;
            w.hovered.bg_stroke = egui::Stroke::new(1.5_f32, NEO_GREEN);
            w.hovered.fg_stroke = egui::Stroke::new(1.0_f32, BG);

            w.active.bg_fill = ACCENT;
            w.active.weak_bg_fill = ACCENT;
            w.active.bg_stroke = egui::Stroke::new(1.5_f32, TEXT);
            w.active.fg_stroke = egui::Stroke::new(1.0_f32, BG);
        }
        BtnTone::DangerOutline => {
            w.inactive.bg_fill = CARD;
            w.inactive.weak_bg_fill = CARD;
            w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, ERROR.gamma_multiply(0.55));
            w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, ERROR);

            w.hovered.bg_fill = CARD_HOVER;
            w.hovered.weak_bg_fill = CARD_HOVER;
            w.hovered.bg_stroke = egui::Stroke::new(1.5_f32, ERROR);
            w.hovered.fg_stroke = egui::Stroke::new(1.0_f32, ERROR);

            w.active.bg_fill = CARD_HOVER;
            w.active.weak_bg_fill = CARD_HOVER;
            w.active.bg_stroke = egui::Stroke::new(1.5_f32, TEXT);
            w.active.fg_stroke = egui::Stroke::new(1.0_f32, ERROR);
        }
        BtnTone::DangerFill => {
            let hover_fill = egui::Color32::from_rgb(255, 108, 100);
            w.inactive.bg_fill = ERROR;
            w.inactive.weak_bg_fill = ERROR;
            w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, ERROR);
            w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

            w.hovered.bg_fill = hover_fill;
            w.hovered.weak_bg_fill = hover_fill;
            w.hovered.bg_stroke = egui::Stroke::new(1.5_f32, TEXT);
            w.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

            w.active.bg_fill = ERROR;
            w.active.weak_bg_fill = ERROR;
            w.active.bg_stroke = egui::Stroke::new(1.5_f32, TEXT);
            w.active.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
        }
    }
}

/// A tinted button that still reacts to hover/press (see [`BtnTone`]).
fn add_toned_button(
    ui: &mut egui::Ui,
    tone: BtnTone,
    label: &str,
    min_size: Option<egui::Vec2>,
) -> egui::Response {
    add_toned_button_enabled(ui, tone, label, min_size, true)
}

/// [`add_toned_button`] plus the disabled case. A disabled widget is dimmed by
/// `Ui::disable` multiplying the painter opacity (`disabled_alpha`), not by
/// swapping `WidgetVisuals`, so the tone still shows through - just faded.
fn add_toned_button_enabled(
    ui: &mut egui::Ui,
    tone: BtnTone,
    label: &str,
    min_size: Option<egui::Vec2>,
    enabled: bool,
) -> egui::Response {
    ui.scope(|ui| {
        apply_tone(ui, tone);
        let mut btn = egui::Button::new(label);
        if let Some(sz) = min_size {
            btn = btn.min_size(sz);
        }
        if enabled {
            ui.add(btn)
        } else {
            ui.add_enabled(false, btn)
        }
    })
    .inner
}


pub enum Msg {
    Gpu(Box<Result<GpuInfo, String>>),
    Files(Box<Result<ModFiles, String>>),
    Progress(String),
    ScanProgress(f32),
    Scanned(Box<Result<Vec<InstalledGame>, String>>),
    Patched(Box<PatchRecord>),
    Unpatched(String, String, patcher::UninstallReport), // (render_exe, game_name, report)
    Erased(Result<String, String>, bool),
    Failed(String),
    Update(Box<Result<UpdateInfo, String>>),
}

pub struct DlssgApp {
    state_dir: PathBuf,
    cache_root: PathBuf,
    local_dirs: Vec<PathBuf>,

    gpu: Option<Result<GpuInfo, String>>,
    files: Option<Result<ModFiles, String>>,
    update: Option<Result<UpdateInfo, String>>,
    scanned: Vec<InstalledGame>,
    scan_progress: Option<f32>,

    manifest: Manifest,
    state: State,

    logs: Vec<String>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    busy_tasks: i32,

    search: String,
    sort: SortMode,
    show_all: bool,
    /// Whether the full activity log is open. It defaults to collapsed so the
    /// expanded panel does not squeeze the games table.
    show_log: bool,
    eraser_confirm: bool,
    /// Whether the "About" credits modal is open. The upstream credits live here
    /// now instead of the header, so the header can stay a compact single line.
    show_about: bool,
    /// Set when the user hits Patch on a game with a detected anti-cheat, so the
    /// install only proceeds after an explicit "Patch anyway".
    patch_confirm: Option<Row>,

    // Whether each patched render exe currently has our files on disk. Rebuilt
    // only when patches change: computing it live means hashing the 15 MB proxy
    // DLL for every row on every frame, which made the UI hang after patching.
    patch_ready: HashMap<String, bool>,
    patch_ready_dirty: bool,

    // The games table rows. Building rows means stat()-ing render executables
    // and checking for nvngx_dlssg.dll per game, so it only happens when the
    // underlying data (scan / manual games / patches) changes - never per frame.
    rows_cache: Vec<Row>,
    rows_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    NameAsc,
    NameDesc,
    /// "Likely" and "Supported" first, then the unknown ones. There is deliberately
    /// no "Patched first" option: patched games are already forced into their own
    /// section at the top of the table regardless of the chosen order, so offering
    /// it in the dropdown did nothing.
    Compatible,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            SortMode::NameAsc => "Name (A–Z)",
            SortMode::NameDesc => "Name (Z–A)",
            SortMode::Compatible => "Compatible first",
        }
    }
}

impl DlssgApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let state_dir = crate::manifest::state_dir().expect("app data directory unavailable");
        let cache_root = state_dir.clone();
        let mut local_dirs = Vec::new();
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
            local_dirs.push(exe_dir.join("files"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            local_dirs.push(cwd.join("files"));
        }
        local_dirs.push(state_dir.join("files"));

        let (manifest, manifest_warning) = load_manifest_reporting();
        let (state, state_warning) = crate::manifest::load_state_recovering();

        let (tx, rx) = mpsc::channel();
        let mut app = DlssgApp {
            state_dir,
            cache_root,
            local_dirs,
            gpu: None,
            files: None,
            update: None,
            scanned: Vec::new(),
            scan_progress: None,
            manifest,
            state,
            logs: Vec::new(),
            tx,
            rx,
            busy_tasks: 0,

            search: String::new(),
            sort: SortMode::Compatible,
            show_all: false,
            show_log: false,
            eraser_confirm: false,
            show_about: false,
            patch_confirm: None,
            patch_ready: HashMap::new(),
            patch_ready_dirty: true,
            rows_cache: Vec::new(),
            rows_dirty: true,
        };
        app.log("Started DLSSG Patcher".into());
        if let Some(w) = manifest_warning {
            app.log(w);
        }
        if let Some(w) = state_warning {
            app.log(w);
        }
        app.refresh_gpu();
        app.refresh_files(false);
        app.scan_games();
        app.check_for_updates();
        app
    }

    fn log(&mut self, line: String) {
        eprintln!("[dlssg-patcher] {line}");
        self.logs.push(line);
        if self.logs.len() > 400 {
            self.logs.drain(0..(self.logs.len() - 400));
        }
    }

    fn spawn<F>(&mut self, f: F)
    where
        F: FnOnce() -> Msg + Send + 'static,
    {
        self.busy_tasks += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = f();
            let _ = tx.send(msg);
        });
    }

    fn refresh_gpu(&mut self) {
        self.log("Querying GPU…".into());
        self.spawn(|| Msg::Gpu(Box::new(query_gpu())));
    }

    fn refresh_files(&mut self, force: bool) {
        self.log(if force { "Refreshing mod files…" } else { "Checking mod files…" }.into());
        let cache_root = self.cache_root.clone();
        let local_dirs = self.local_dirs.clone();
        let tx = self.tx.clone();
        self.spawn(move || {
            let progress = Box::new(move |line: String| {
                let _ = tx.send(Msg::Progress(line));
            });
            let result = ensure_mod_files(&cache_root, &local_dirs, force, Some(progress));
            Msg::Files(Box::new(result))
        });
    }

    fn scan_games(&mut self) {
        self.scan_progress = Some(0.0);
        self.log("Scanning Steam, Epic, GOG, Ubisoft and EA…".into());
        let tx = self.tx.clone();
        self.spawn(move || {
            let (mut games, notes) = launchers::scan_all(&mut |done, total| {
                let _ = tx.send(Msg::ScanProgress(scan_launcher_fraction(done, total)));
            });
            for note in &notes {
                let _ = tx.send(Msg::Progress(format!("  {note}")));
            }
            let (manifest, manifest_warning) = load_manifest_reporting();
            if let Some(w) = manifest_warning {
                let _ = tx.send(Msg::Progress(format!("  {w}")));
            }
            let catalog_exe_present = |g: &InstalledGame| {
                find_manifest_game_by_appid(&manifest, g.appid, &g.folder_name)
                    .filter(|m| !m.render_exe.is_empty())
                    .map(|m| g.install_dir.join(&m.render_exe).is_file())
                    .unwrap_or(false)
            };
            let total = games.len();
            for (i, g) in games.iter_mut().enumerate() {
                g.anti_cheat = anticheat::detect(&g.install_dir);
                // Catalogued games normally get their render path straight from
                // games.json, so only pay for a full autodetect walk when that
                // path is missing or stale (reinstall / moved folder / different
                // exe). Without this the `detected_exe` fallback in
                // `render_exe_for_scanned` is dead code for catalogued titles and
                // they show up as "not located" even when the exe is right there.
                if !catalog_exe_present(g) {
                    g.detected_exe = autodetect_render_exe(&g.install_dir);
                }
                let _ = tx.send(Msg::ScanProgress(scan_game_fraction(i + 1, total)));
            }
            // Drop orphan folders left behind by uninstalled games: with no exe
            // anywhere there is nothing to patch, and nothing to locate manually,
            // so they would only clutter the list (and the "show not-located"
            // view). Real installs whose exe we could not auto-pick are kept.
            let scanned = games.len();
            games.retain(|g| {
                catalog_exe_present(g) || g.detected_exe.is_some() || has_any_exe(&g.install_dir)
            });
            if games.len() < scanned {
                let _ = tx.send(Msg::Progress(format!(
                    "  Skipped {} folder(s) with no executable.",
                    scanned - games.len()
                )));
            }
            let result = if games.is_empty() {
                Err(format!(
                    "No games found in any launcher ({}). Use \"Add install folder…\" instead.",
                    notes.join(", ")
                ))
            } else {
                Ok(games)
            };
            Msg::Scanned(Box::new(result))
        });
    }

    fn check_for_updates(&mut self) {
        self.log("Checking upstream for a newer dlssg_for_sm86…".into());
        self.spawn(|| Msg::Update(Box::new(check_for_update())));
    }

    /// Short "vX.Y.Z" chip for the header: green when current, amber when a newer
    /// upstream version exists, muted while unverified.
    fn version_dot(&self, ui: &mut egui::Ui) {
        let local = self
            .files
            .as_ref()
            .and_then(|f| f.as_ref().ok())
            .and_then(|f| f.version.clone());
        let (tip, color, show) = match (&self.update, &local) {
            (Some(Ok(u)), Some(l)) => {
                if version_cmp(&u.remote_version, l).is_gt() {
                    (
                        format!("dlssg_for_sm86: v{l} -> v{} available", u.remote_version),
                        WARN,
                        true,
                    )
                } else {
                    (
                        format!("dlssg_for_sm86: up to date (v{})", u.remote_version),
                        SUCCESS,
                        true,
                    )
                }
            }
            (_, Some(l)) => (format!("dlssg_for_sm86 v{l}"), MUTED, true),
            _ => (String::new(), MUTED, false),
        };
        if !show {
            return;
        }
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, color);
        resp.on_hover_text(tip);
    }

    fn poll_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Gpu(r) => {
                    match &*r {
                        Ok(g) => {
                            if g.driver.trim().is_empty() {
                                self.log(format!("GPU: {} (detected via Windows)", g.name));
                            } else {
                                self.log(format!("GPU: {} | driver {}", g.name, g.driver));
                            }
                        }
                        Err(e) => self.log(format!("GPU query failed: {e}")),
                    }
                    self.gpu = Some(*r);
                }
                Msg::Progress(line) => {
                    self.log(line);
                    continue;
                }
                Msg::ScanProgress(f) => {
                    self.scan_progress = Some(f);
                    continue;
                }
                Msg::Files(r) => {
                    match &*r {
                        Ok(f) => self.log(format!(
                            "Mod files ready (source: {}, version: {})",
                            f.source.label(),
                            f.version.clone().unwrap_or_else(|| "unknown".into())
                        )),
                        Err(e) => self.log(format!("Mod files unavailable: {e}")),
                    }
                    self.files = Some(*r);
                }
                Msg::Scanned(r) => {
                    self.scan_progress = None;
                    match *r {
                        Ok(games) => {
                            let n = games.len();
                            let known = games
                                .iter()
                                .filter(|g| find_manifest_game_by_appid(&self.manifest, g.appid, &g.folder_name).is_some())
                                .count();
                            let located = games.iter().filter(|g| g.detected_exe.is_some()).count();
                            self.scanned = games;
                            self.patch_ready_dirty = true;
                            self.rows_dirty = true;
                            self.log(format!("Found {n} games ({known} in catalog, {located} with auto-located exe)"));
                        }
                        Err(e) => self.log(format!("Game scan failed: {e}")),
                    }
                }
                Msg::Patched(p) => {
                    let name = p.game_name.clone();
                    let exe = p.render_exe.clone();
                    // Unconditional replace: a repatch refreshes the stored hash
                    // and mod version, which is exactly how a stale record heals.
                    self.state.upsert_patch(*p);
                    // The cached value for this exe (if any) is now stale: it was
                    // presence-scanned as "not patched" before this record existed.
                    // Drop it so the next rebuild re-hashes just this one game
                    // instead of trusting a false cached value.
                    self.patch_ready.retain(|k, _| !k.eq_ignore_ascii_case(&exe));
                    self.patch_ready_dirty = true;
                    self.rows_dirty = true;
                    // Log exactly what landed where so users can verify the bundle.
                    if let Some(rec) = self.state.patches.iter().find(|r| r.render_exe.eq_ignore_ascii_case(&exe)) {
                        let dir = std::path::Path::new(&rec.render_exe)
                            .parent()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default();
                        let iv = rec.mod_version.clone().unwrap_or_else(|| "unknown".to_string());
                        let proxy_dll = rec.proxy_dll.clone();
                        let files = rec
                            .installed_files
                            .iter()
                            .map(|f| dir.clone() + "\\" + f + " (" + &patched_size(dir.clone(), f) + ")")
                            .collect::<Vec<_>>()
                            .join(" and ");
                        self.log(format!("Placed {files}"));
                        self.log(format!("Patched {name} with DLSSG v{iv} ({proxy_dll} + dlssg_sm86)"));
                    }
                    if let Err(e) = save_state(&self.state) {
                        self.log(format!("Patched {name} but failed to save state: {e}"));
                    }
                }
                Msg::Unpatched(exe, name, report) => {
                    patcher::remove_patch_record(&mut self.state, &exe);
                    self.patch_ready_dirty = true;
                    self.rows_dirty = true;
                    if report.deleted_replaced_proxy {
                        self.log(format!(
                            "{name}: removed a DLL that no longer matched the patch record."
                        ));
                    }
                    if report.kept_game_original {
                        self.log(format!(
                            "{name}: the game's own DLL was already back; left it in place."
                        ));
                    }
                    for b in &report.kept_backups {
                        self.log(format!("Kept a backup at {b} - it could not be restored automatically."));
                    }
                    match save_state(&self.state) {
                        Ok(_) => self.log(format!("Unpatched {name}")),
                        Err(e) => self.log(format!("Unpatched {name} but failed to save state: {e}")),
                    }
                }
                Msg::Erased(r, data_removed) => {
                    match &r {
                        Ok(s) => self.log(s.clone()),
                        Err(s) => self.log(format!("Cleanup finished with problems: {s}")),
                    }
                    if data_removed {
                        // Everything on disk is gone: drop in-memory state so the UI
                        // stops showing games as patched and disables patching until
                        // the mod files are downloaded again.
                        self.state = State::default();
                        self.files = None;
                        self.patch_ready.clear();
                    } else {
                        // state.json deliberately survived (unrestorable backups were
                        // kept), so the in-memory state must stay in sync with disk or
                        // the next launch would resurrect rows the user just erased.
                        self.state = load_state().unwrap_or_default();
                    }
                    self.patch_ready_dirty = true;
                    self.rows_dirty = true;
                    self.eraser_confirm = false;
                }
                Msg::Failed(e) => self.log(format!("Error: {e}")),
                Msg::Update(r) => {
                    let installed = self
                        .files
                        .as_ref()
                        .and_then(|f| f.as_ref().ok())
                        .and_then(|f| f.version.clone());
                    let line = match &*r {
                        Ok(u) => {
                            let pushed = u.pushed_hours_ago.map(|h| h.to_string()).unwrap_or_else(|| "?".into());
                            match installed {
                                Some(ins) => format!("Upstream is at v{} (pushed {pushed}h ago) · installed: v{ins}", u.remote_version),
                                None => format!("Upstream is at v{} (pushed {pushed}h ago)", u.remote_version),
                            }
                        }
                        Err(e) => format!("Update check failed: {e}"),
                    };
                    self.log(line);
                    self.update = Some(*r);
                }
            }
            self.busy_tasks = (self.busy_tasks - 1).max(0);
        }
    }

    fn router(&self) -> Option<&'static str> {
        match &self.gpu {
            Some(Ok(g)) if !g.arch.router().is_empty() => Some(g.arch.router()),
            _ => None,
        }
    }

    fn resolve_game_proxy(
        &self,
        install_dir: &Path,
        render_exe: Option<&PathBuf>,
        catalog: Option<&ManifestGame>,
    ) -> String {
        let exe_key_str = render_exe.map(|e| e.to_string_lossy().to_string());
        let recorded = exe_key_str
            .as_deref()
            .and_then(|e| self.state.patch_for_exe(e))
            .map(|r| r.proxy_dll.as_str());
        let detected = render_exe.map(|e| patcher::detect_installed_proxy(e)).flatten();
        pick_proxy(
            self.state.proxy_override_for(&install_dir.to_string_lossy()),
            recorded,
            detected.as_deref(),
            catalog.map(|m| m.proxy.as_str()),
            self.state.effective_default_proxy(),
        )
    }

    fn proxy_for_game(&self, g: &crate::steam::InstalledGame) -> String {
        let catalog = find_manifest_game_by_appid(&self.manifest, g.appid, &g.folder_name);
        let render_exe = self.render_exe_for_scanned(g);
        self.resolve_game_proxy(&g.install_dir, render_exe.as_ref(), catalog)
    }

    /// Resolve the actual renderer exe for a scanned game, exactly as the games
    /// list does: a catalogued game gets its path from the manifest (install_dir +
    /// render_exe), anything else falls back to the autodetected exe.
    fn render_exe_for_scanned(&self, g: &crate::steam::InstalledGame) -> Option<PathBuf> {
        let manifest_game = find_manifest_game_by_appid(&self.manifest, g.appid, &g.folder_name);
        let dir_key = exe_key(&g.install_dir.to_string_lossy());
        manifest_game
            // Most NVIDIA entries carry no curated exe path; the filter drops the
            // empty join so autodetection runs instead of pinning the install dir.
            .filter(|m| !m.render_exe.is_empty())
            .map(|m| fix_seps(g.install_dir.join(&m.render_exe)))
            .filter(|p| p.is_file())
            .or_else(|| g.detected_exe.clone())
            .or_else(|| {
                self.state
                    .manual_games
                    .iter()
                    .find(|m| exe_key(&m.install_dir) == dir_key)
                    .map(|m| PathBuf::from(&m.render_exe))
                    .filter(|p| p.is_file())
            })
    }

    /// Recompute which games are currently patched. This covers both patches we
    /// installed ourselves (verified by recorded hash + INI marker) AND patches
    /// pre-installed on disk that we have no record for (applied manually or by
    /// another tool), so those show up as "Patched (detected)" and can be removed.
    ///
    /// State-owned patches are only hashed the first time we see them; their
    /// results are stored in `patch_ready` and trusted afterwards. Otherwise
    /// every patch/unpatch would re-read a 15 MB proxy DLL once per record on
    /// the UI thread, which is what made the tool freeze after clicking Patch.
    fn rebuild_patch_cache(&mut self) {
        let mut ready: HashMap<String, bool> = HashMap::new();
        for p in &self.state.patches {
            let key = exe_key(&p.render_exe);
            // Only a POSITIVE result is ever reused. It saves re-hashing the
            // 15 MB proxy DLL for every record on the UI thread, which is what
            // froze the tool after clicking Patch. A negative is deliberately
            // re-probed on every rebuild: the folder may have been remounted,
            // moved back, or restored, and a cached `false` made a genuinely
            // patched game report the wrong patch state until the app was restarted.
            if self.patch_ready.get(&key) == Some(&true) {
                ready.insert(key, true);
                continue;
            }
            ready.insert(key, patcher::looks_patched(p));
        }
        for g in &self.scanned {
            if let Some(exe) = self.render_exe_for_scanned(g) {
                let key = exe_key(&exe.to_string_lossy());
                if ready.contains_key(&key) {
                    continue;
                }
                let proxy = self.proxy_for_game(g);
                ready.insert(key, patcher::dir_looks_patched(&exe, &proxy));
            }
        }
        for m in &self.state.manual_games {
            let key = exe_key(&m.render_exe);
            if ready.contains_key(&key) {
                continue;
            }
            let exe = std::path::PathBuf::from(&m.render_exe);
            ready.insert(key, patcher::dir_looks_patched(&exe, &m.proxy));
        }
        self.patch_ready = ready;
        self.patch_ready_dirty = false;
    }

    /// (Re)build the rows backing the games table. Must run after the patch
    /// cache is fresh so each Row carries an accurate `patched` flag.
    fn rebuild_rows(&mut self) {
        self.rows_cache = self.compute_rows();
        self.rows_dirty = false;
    }

    fn compute_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for g in &self.scanned {
            let manifest_game = find_manifest_game_by_appid(&self.manifest, g.appid, &g.folder_name);
            let render_exe = self.render_exe_for_scanned(g);
            let name = manifest_game.map(|m| m.name.clone()).unwrap_or_else(|| g.folder_name.clone());
            let proxy = self.resolve_game_proxy(&g.install_dir, render_exe.as_ref(), manifest_game);
            let support_tag = support_tag(manifest_game, render_exe.as_ref());
            let patched = self.is_patched_exe(render_exe.as_ref());
            rows.push(Row {
                name,
                install_dir: g.install_dir.clone(),
                render_exe,
                proxy,
                support_tag,
                patched,
                source: g.source.clone(),
                anti_cheat: g.anti_cheat.clone(),
                missing: false,
            });
        }
        for m in &self.state.manual_games {
            let dir_key = exe_key(&m.install_dir);
            if self
                .scanned
                .iter()
                .any(|g| exe_key(&g.install_dir.to_string_lossy()) == dir_key)
            {
                continue; // already represented by its storefront row above
            }
            let dir = PathBuf::from(&m.install_dir);
            let render_exe_path = PathBuf::from(&m.render_exe);
            // A manual entry has no AppID, so try the folder name first (that is
            // what the user picked on disk) and then whatever name they typed.
            // Checking the catalog here is what makes manual games agree with
            // scanned ones instead of always falling back to the DLL probe.
            let folder = dir
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let catalog = find_manifest_game(&self.manifest, &folder)
                .or_else(|| find_manifest_game(&self.manifest, &m.name));
            let support_tag = support_tag(catalog, Some(&render_exe_path));
            let render_exe = Some(PathBuf::from(&m.render_exe)).filter(|p| p.is_file());
            let proxy = self.resolve_game_proxy(&dir, render_exe.as_ref(), catalog);
            let anti_cheat = anticheat::detect(Path::new(&m.install_dir));
            rows.push(Row {
                name: m.name.clone(),
                install_dir: dir,
                render_exe: render_exe.clone(),
                proxy,
                support_tag,
                patched: self.is_patched_exe(render_exe.as_ref()),
                source: "Manual".to_string(),
                anti_cheat,
                missing: false,
            });
        }
        // Patch records whose install folder has moved or been deleted. The table
        // is otherwise built purely from launcher scans, so a vanished folder made
        // its record invisible AND unreachable: no action, not even a log line,
        // while the record sat in state.json forever. Synthesizing a row here is
        // what makes Relink / Forget possible.
        for p in &self.state.patches {
            let exe = PathBuf::from(&p.render_exe);
            if exe.is_file() {
                continue; // a real row above already represents this record
            }
            let key = exe_key(&p.render_exe);
            if rows
                .iter()
                .any(|r| r.render_exe.as_ref().is_some_and(|e| exe_key(&e.to_string_lossy()) == key))
            {
                continue;
            }
            rows.push(Row {
                name: p.game_name.clone(),
                install_dir: PathBuf::from(&p.install_dir),
                render_exe: Some(exe),
                proxy: p.proxy_dll.clone(),
                support_tag: (
                    "—".to_string(),
                    MUTED,
                    "The folder this game was patched into no longer exists.".to_string(),
                ),
                patched: false,
                source: "Missing".to_string(),
                anti_cheat: anticheat::AcScan::default(),
                missing: true,
            });
        }
        rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        rows
    }

    fn is_patched_exe(&self, exe: Option<&PathBuf>) -> bool {
        exe.and_then(|e| self.patch_ready.get(&exe_key(&e.to_string_lossy())))
            .copied()
            .unwrap_or(false)
    }

    /// True when a game we patched needs a Repatch: the user selected a different proxy, the
    /// 0.4.0 runtime tree is not on disk (a 0.3.x patch, or a game verify wiped it), or the
    /// cached bundle is newer than the version this game was patched with.
    fn row_needs_update(&self, row: &Row) -> bool {
        if !row.patched {
            return false;
        }
        let Some(exe) = &row.render_exe else { return false };
        let Some(rec) = self.state.patch_for_exe(&exe.to_string_lossy()) else { return false };
        let cached = match &self.files {
            Some(Ok(f)) => f.version.as_deref(),
            _ => None,
        };
        needs_update(
            rec.proxy_dll != row.proxy,
            patcher::has_runtime_dir(exe),
            cached,
            rec.mod_version.as_deref(),
        )
    }
}

impl eframe::App for DlssgApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages();
        if self.patch_ready_dirty {
            self.rebuild_patch_cache();
        }
        if self.rows_dirty {
            self.rebuild_rows();
        }

        // One consistent 16px side margin on every panel keeps the header/cards,
        // the log box and the games table on the same vertical guides.
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ctx, |ui| {
                self.ui_header(ui);
                ui.add_space(4.0);
                self.update_card(ui);
                ui.add_space(4.0);
                self.gpu_card(ui);
            });
        egui::TopBottomPanel::bottom("log")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)))
            .show(ctx, |ui| {
                if self.show_log {
                    self.ui_log(ui);
                } else {
                    self.ui_log_strip(ui);
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)))
            .show(ctx, |ui| {
                self.ui_games(ui);
            });

        if self.busy_tasks > 0 {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        if self.eraser_confirm {
            egui::Modal::new(egui::Id::new("eraser_modal"))
                // Heavy dim so the whole app reads as "blurred out" behind the
                // destructive-action prompt.
                .backdrop_color(egui::Color32::from_black_alpha(190))
                .frame(
                    egui::Frame::popup(&ctx.style())
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0_f32, ERROR))
                        .inner_margin(egui::Margin::symmetric(18, 14)),
                )
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(
                        egui::RichText::new("Erase all patches and app data?")
                            .strong()
                            .size(16.0)
                            .color(ERROR),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            r"This unpatches every game (restoring any original files) and deletes %LOCALAPPDATA%\dlssg-patcher.",
                        )
                        .color(TEXT),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Your installed games themselves are not affected.")
                            .color(MUTED),
                    );
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if add_toned_button(ui, BtnTone::DangerFill, "Erase DLSSG", None).clicked() {
                            self.eraser_confirm = false;
                            self.erase_everything();
                        }
                        if ui.button("Cancel").clicked() {
                            self.eraser_confirm = false;
                        }
                    });
                });
        }

        if self.show_about {
            egui::Modal::new(egui::Id::new("about_modal"))
                .backdrop_color(egui::Color32::from_black_alpha(190))
                .frame(
                    egui::Frame::popup(&ctx.style())
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0_f32, BORDER))
                        .inner_margin(egui::Margin::symmetric(18, 14)),
                )
                .show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("DLSSG Patcher")
                                .strong()
                                .size(17.0)
                                .color(NEO_GREEN),
                        );
                        ui.label(
                            egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                .color(MUTED)
                                .size(13.0),
                        );
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "One-click game scanner and patcher for native NVIDIA Frame Generation on RTX 20/30-series GPUs.",
                        )
                        .color(TEXT),
                    );
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(egui::RichText::new("Credits").strong().color(TEXT));
                    ui.add_space(6.0);

                    // The global style sets item_spacing.x = 10.0, which reads as big
                    // gaps between the inline words below; tighten it for these lines.
                    let saved_x = ui.spacing().item_spacing.x;
                    ui.spacing_mut().item_spacing.x = 4.0;

                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("• Core DLSSG Mod (SM86):").color(MUTED));
                        ui.hyperlink_to("sdli1995", "https://github.com/sdli1995/dlssg_for_sm86");
                    });

                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("• RTX 20-Series Port (SM75):").color(MUTED));
                        ui.hyperlink_to("Coldwood1026", "https://github.com/Coldwood1026/dlssg_for_sm75");
                        ui.label(egui::RichText::new("(integrated upstream)").color(MUTED));
                    });

                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("• Patcher & GUI: Built by").color(MUTED));
                        ui.hyperlink_to("sds-228", "https://github.com/sds-228/dlssg-patcher-gui");
                        ui.label(
                            egui::RichText::new(
                                "with OpenCode and a bit of human brain - because the game library was too big and too boring to patch by hand.",
                            )
                            .color(MUTED),
                        );
                    });

                    ui.spacing_mut().item_spacing.x = saved_x;

                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_about = false;
                        }
                    });
                });
        }

        if let Some(row) = self.patch_confirm.clone() {
            egui::Modal::new(egui::Id::new("anticheat_modal"))
                .backdrop_color(egui::Color32::from_black_alpha(190))
                .frame(
                    egui::Frame::popup(&ctx.style())
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0_f32, WARN))
                        .inner_margin(egui::Margin::symmetric(18, 14)),
                )
                .show(ctx, |ui| {
                    ui.set_max_width(400.0);
                    let ac = row
                        .anti_cheat
                        .detected_name()
                        .unwrap_or("Anti-cheat");
                    ui.label(
                        egui::RichText::new(format!("{ac} detected"))
                            .strong()
                            .size(16.0)
                            .color(WARN),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} includes {ac} files. Proxy DLLs may be blocked or flagged in online modes.",
                            row.name
                        ))
                        .color(TEXT),
                    );
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if add_toned_button(ui, BtnTone::AccentFill, "Patch anyway", None).clicked() {
                            self.patch_confirm = None;
                            self.do_patch(&row);
                        }
                        if ui.button("Cancel").clicked() {
                            self.patch_confirm = None;
                        }
                    });
                });
        }
    }
}

// ---------------------------------------------------------------- UI sections

impl DlssgApp {
    /// Extra-bold heading (the stock heading font is light, so paint the text
    /// twice with a one-pixel offset).
    fn bold_heading(&self, ui: &mut egui::Ui, text: &str, color: egui::Color32) {
        let base = egui::FontId::proportional(22.0);
        let galley = ui
            .ctx()
            .fonts(|fonts| fonts.layout_no_wrap(text.to_owned(), base, color));
        let size = galley.size();
        let (rect, _) = ui.allocate_exact_size(size + egui::vec2(2.0, 0.0), egui::Sense::hover());

        let painter = ui.painter();
        painter.galley(
            egui::pos2(
                rect.min.x + 1.0,
                rect.min.y + (rect.height() - size.y) / 2.0 + 1.0,
            ),
            galley.clone(),
            color,
        );
        painter.galley(
            egui::pos2(rect.min.x, rect.min.y + (rect.height() - size.y) / 2.0),
            galley,
            color,
        );
    }

    fn ui_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(-2.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    self.bold_heading(ui, "DLSSG Patcher", NEO_GREEN);
                    self.version_dot(ui);
                    ui.label(
                        egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                            .color(MUTED)
                            .size(13.0),
                    );
                    if self.busy_tasks > 0 {
                        ui.add_space(6.0);
                        ui.spinner();
                    }
                });
                ui.label(
                    egui::RichText::new("NVIDIA Frame Generation on RTX 20/30 series · dlssg_for_sm86")
                        .color(MUTED)
                        .size(13.0),
                );
                ui.add_space(2.0);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // No extra inset here: the top panel's own 16px margin puts the right
                // edge of [Eraser…] flush with the outer edge of the cards below.
                if add_toned_button(ui, BtnTone::DangerOutline, "Eraser…", None)
                    .on_hover_text("Remove every patch and all DLSSG Patcher data from this PC")
                    .clicked()
                {
                    self.eraser_confirm = true;
                }
                if ui
                    .button("About")
                    .on_hover_text("Credits and upstream mod links")
                    .clicked()
                {
                    self.show_about = true;
                }
            });
        });
        ui.add_space(4.0);
    }

    fn update_card(&mut self, ui: &mut egui::Ui) {
        card_frame().show(ui, |ui| {
            let local = self
                .files
                .as_ref()
                .and_then(|f| f.as_ref().ok())
                .and_then(|f| f.version.clone());
            let upd = self.update.as_ref().and_then(|r| r.as_ref().ok()).cloned();
            ui.spacing_mut().interact_size.y = row_control_height(ui);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Update (dlssg_for_sm86)").strong().size(15.0));
                match &self.update {
                    None => {
                        ui.spinner();
                        ui.weak("Checking upstream…");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ERROR, format!("Update check failed: {e}"));
                    }
                    Some(Ok(_u)) => {
                        if let Some(u) = &upd {
                            let pushed = u.pushed_hours_ago.map(|h| format!(" · pushed {h}h ago")).unwrap_or_default();
                            match &local {
                                Some(lv) => {
                                    let ord = version_cmp(&u.remote_version, lv);
                                    if ord.is_gt() {
                                        ui.colored_label(WARN, format!("v{lv} -> v{} available{pushed}", u.remote_version));
                                    } else if ord.is_lt() {
                                        ui.colored_label(DETECTED, format!("you have v{lv} · upstream is v{}{pushed}", u.remote_version));
                                    } else {
                                        ui.colored_label(SUCCESS, format!("up to date (v{}){pushed}", u.remote_version));
                                    }
                                }
                                None => {
                                    ui.colored_label(MUTED, format!("upstream v{}{pushed}", u.remote_version));
                                }
                            }
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has_update = match (&upd, &local) {
                        (Some(u), Some(lv)) => version_cmp(&u.remote_version, lv).is_gt(),
                        _ => false,
                    };
                    if has_update {
                        let rem = upd.as_ref().map(|u| u.remote_version.clone()).unwrap_or_default();
                        let label = format!("Update to v{rem}");
                        if add_toned_button(ui, BtnTone::AccentFill, &label, None).clicked() {
                            self.refresh_files(true);
                        }
                    }
                    if ui.button("Check for updates").clicked() {
                        self.check_for_updates();
                    }
                });
            });
        });
    }

    fn gpu_card(&mut self, ui: &mut egui::Ui) {
        card_frame().show(ui, |ui| {
            ui.spacing_mut().interact_size.y = row_control_height(ui);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Graphics card").strong().size(15.0));
                match &self.gpu {
                    None => {
                        ui.spinner();
                        ui.weak("Querying nvidia-smi…");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ERROR, "No NVIDIA GPU detected");
                        ui.monospace(egui::RichText::new(e.clone()).color(MUTED));
                    }
                    Some(Ok(g)) => {
                        let arch_label = match g.arch {
                            Arch::Sm86 => "Supported (Ampere SM86)".to_string(),
                            Arch::Sm75 => "Supported (Turing SM75)".to_string(),
                            Arch::Unsupported => "Unsupported card - targets RTX 20/30 series".to_string(),
                        };
                        if g.from_wmi {
                            let note = match &g.igpu {
                                Some(igpu) => hybrid_note(igpu),
                                None => wmi_only_note(),
                            };
                            ui.colored_label(WARN, format!("{} · {arch_label} · {note}", g.name));
                        } else {
                            let vram = g.vram_mib
                                .map(|m| format!("{:.0} GB", m as f64 / 1024.0))
                                .unwrap_or_else(|| "?".into());
                            ui.colored_label(SUCCESS, format!("{} · {vram} · driver {} · {}", g.name, g.driver, arch_label));
                            if let Some(note) = driver_note(&g.driver) {
                                ui.colored_label(WARN, note);
                            }
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.refresh_gpu();
                    }
                });
            });
        });
    }

    fn ui_log(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(TERM_BG)
            .corner_radius(egui::CornerRadius::same(8))
            .stroke(egui::Stroke::new(1.0_f32, TERM_BORDER))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Activity").strong().size(14.0).color(TERM_GREEN));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Hide").clicked() {
                            self.show_log = false;
                        }
                        ui.label(egui::RichText::new("output · log").small().color(TERM_DIM));
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("log_scroll")
                    .max_height(120.0)
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if self.logs.is_empty() {
                            ui.monospace(egui::RichText::new("No activity yet.").color(TERM_DIM));
                        }
                        for line in &self.logs {
                            ui.monospace(
                                egui::RichText::new(line).color(log_line_color(line)).size(13.0),
                            );
                        }
                    });
            });
    }

    /// Collapsed form of the activity panel: a slim strip showing the most recent
    /// line plus a button to open the full log. Keeps the games table in charge of
    /// the screen by default.
    fn ui_log_strip(&mut self, ui: &mut egui::Ui) {
        let last = self
            .logs
            .last()
            .cloned()
            .unwrap_or_else(|| "No activity yet.".to_string());
        let color = log_line_color(&last);
        egui::Frame::new()
            .fill(TERM_BG)
            .corner_radius(egui::CornerRadius::same(8))
            .stroke(egui::Stroke::new(1.0_f32, TERM_BORDER))
            .inner_margin(egui::Margin::symmetric(14, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Activity").strong().size(13.0).color(TERM_GREEN));
                    ui.add_space(4.0);
                    // Painted hairline instead of a box-drawing glyph: the bundled
                    // font has no glyph for "│" and rendered it as a missing-glyph box.
                    let (sep, _) = ui.allocate_exact_size(egui::vec2(1.0, 14.0), egui::Sense::hover());
                    ui.painter().vline(sep.center().x, sep.y_range(), egui::Stroke::new(1.0_f32, TERM_BORDER));
                    ui.add_space(6.0);

                    // The message is laid out inside a rect measured up front, with
                    // the toggle's width subtracted. Letting the label size itself
                    // first (as this did before) meant a long line pushed into - and
                    // sometimes over - the button.
                    const TOGGLE_W: f32 = 26.0;
                    let msg_w = (ui.available_width() - TOGGLE_W - 8.0).max(40.0);
                    let (msg_rect, _) =
                        ui.allocate_exact_size(egui::vec2(msg_w, 20.0), egui::Sense::hover());
                    let mut msg_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(msg_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    msg_ui.add(egui::Label::new(egui::RichText::new(&last).color(color).size(13.0)).truncate());

                    ui.add_space(4.0);
                    // Compact icon toggle in place of a full "Show log" button, which
                    // was wide enough to squeeze the message out of a narrow window.
                    let (btn_rect, btn) =
                        ui.allocate_exact_size(egui::vec2(TOGGLE_W, 20.0), egui::Sense::click());
                    let hot = btn.hovered();
                    if btn.clicked() {
                        self.show_log = true;
                    }
                    btn.on_hover_text("Show activity log");
                    let tint = if hot { MUTED } else { TERM_DIM };
                    ui.painter().rect_filled(
                        btn_rect,
                        egui::CornerRadius::same(4),
                        if hot { CARD } else { TERM_BG },
                    );
                    ui.painter().rect_stroke(
                        btn_rect,
                        egui::CornerRadius::same(4),
                        egui::Stroke::new(1.0_f32, if hot { TERM_BORDER } else { TERM_DIM }),
                        egui::StrokeKind::Inside,
                    );
                    // Hamburger drawn as three painted lines rather than a "≡"/"☰"
                    // glyph - the bundled font is missing several symbol codepoints,
                    // which is what turned the earlier box-drawing separator into a
                    // missing-glyph square.
                    let stroke = egui::Stroke::new(1.2_f32, tint);
                    let bar_w = 12.0;
                    let x0 = btn_rect.center().x - bar_w / 2.0;
                    for i in 0..3 {
                        let y = btn_rect.center().y - 4.0 + i as f32 * 4.0;
                        ui.painter().hline(egui::Rangef::new(x0, x0 + bar_w), y, stroke);
                    }
                });
            });
    }

    fn ui_games(&mut self, ui: &mut egui::Ui) {
        // Games are presented as a data table, not prose. egui makes every label
        // selectable by default and swaps the pointer to the text I-beam whenever
        // one is hovered; because the data cells are justified, each label's hover
        // rect stretches to the whole column, so the arrow turned into an I-beam
        // over empty space. Disable label selection for the tab - the Search box is
        // a TextEdit, so it keeps its own caret and selection regardless.
        ui.style_mut().interaction.selectable_labels = false;
        let mut rows: Vec<Row> = self.rows_cache.clone();
        if !self.show_all {
            rows.retain(|r| r.render_exe.is_some());
        }
        let q = self.search.trim().to_lowercase();
        if !q.is_empty() {
            rows.retain(|r| r.name.to_lowercase().contains(&q));
        }
        match self.sort {
            SortMode::NameDesc => {
                rows.sort_by(|a, b| b.name.to_lowercase().cmp(&a.name.to_lowercase()));
            }
            SortMode::Compatible => {
                rows.sort_by_key(|r| (!(r.support_tag.1 == SUCCESS), r.name.to_lowercase()));
            }
            SortMode::NameAsc => {
                rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            }
        }
        let supported = rows.iter().filter(|r| r.support_tag.1 == SUCCESS).count();
        let patched_count = rows.iter().filter(|r| r.patched).count();
        let missing_count = rows.iter().filter(|r| r.missing).count();

        // Keep whatever relative order the selected sort produced, but always move
        // currently-patched games into their own section on top, and orphaned
        // records (patched into a folder that no longer exists) just under them.
        let mut patched = Vec::with_capacity(patched_count);
        let mut missing_rows = Vec::with_capacity(missing_count);
        let mut rest = Vec::with_capacity(rows.len() - patched_count - missing_count);
        for row in rows.drain(..) {
            if row.missing {
                missing_rows.push(row);
            } else if row.patched {
                patched.push(row);
            } else {
                rest.push(row);
            }
        }
        let mut rows = patched;
        rows.extend(missing_rows);
        rows.extend(rest);

        // Two aligned toolbar rows that share one label-column width (`label_col_w`):
        // the "Games" and "Search" labels both occupy a fixed 48px cell, so the
        // Search box can start flush with [Scan games] and end flush with [Add…].
        //
        // One height for the whole toolbar row: the buttons size to it, and the
        // `ComboBox` derives both its frame and the `ui.horizontal` it wraps itself
        // in from `spacing().interact_size.y`. If that is smaller than the row, the
        // combo's child gets vertically centered and its frame sits lower than the
        // buttons beside it.
        let toolbar_btn_h = row_control_height(ui);
        let label_col_w = 48.0;
        let mut scan_min_x = 0.0_f32;
        let mut add_max_x = 0.0_f32;

        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), toolbar_btn_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spacing_mut().interact_size.y = toolbar_btn_h;
                let (lbl_rect, _) =
                    ui.allocate_exact_size(egui::vec2(label_col_w, toolbar_btn_h), egui::Sense::hover());
                ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(lbl_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                )
                .label(egui::RichText::new("Games").strong().size(13.0));

                ui.spacing_mut().button_padding.x = 14.0;
                let scan_rect = if let Some(p) = self.scan_progress {
                    let track = ui.visuals().extreme_bg_color;
                    ui.visuals_mut().extreme_bg_color = CARD;
                    let bar = ui.add(
                        egui::ProgressBar::new(p)
                            .desired_width(toolbar_button_width(ui, "Scan games"))
                            .desired_height(toolbar_btn_h)
                            .corner_radius(egui::CornerRadius::same(6))
                            .fill(ACCENT),
                    );
                    ui.visuals_mut().extreme_bg_color = track;
                    ui.painter().rect_stroke(
                        bar.rect,
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(1.0_f32, BORDER),
                        egui::StrokeKind::Inside,
                    );
                    bar.rect
                } else {
                    let resp = ui.button("Scan games");
                    if resp.clicked() {
                        self.scan_games();
                    }
                    resp.rect
                };
                scan_min_x = scan_rect.min.x;

                let add_resp = ui.button("Add…");
                add_max_x = add_resp.rect.max.x;
                if add_resp.clicked() {
                    self.add_install_folder();
                }

                egui::ComboBox::from_id_salt("games_sort")
                    .selected_text(self.sort.label())
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.sort, SortMode::NameAsc, SortMode::NameAsc.label());
                        ui.selectable_value(&mut self.sort, SortMode::NameDesc, SortMode::NameDesc.label());
                        ui.selectable_value(&mut self.sort, SortMode::Compatible, SortMode::Compatible.label());
                    });

                let mut def = self.state.effective_default_proxy().to_string();
                egui::ComboBox::from_id_salt("default_proxy_combo")
                    .selected_text(format!("Proxy: {def}"))
                    .width(138.0)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for &p in crate::download::PROXY_NAMES {
                            ui.selectable_value(&mut def, p.to_string(), p);
                        }
                    })
                    .response
                    .on_hover_text("Default proxy DLL for games without a per-game override");
                if def != self.state.effective_default_proxy() {
                    self.state.default_proxy = def;
                    match save_state(&self.state) {
                        Ok(_) => self.log("Default proxy updated".into()),
                        Err(e) => self.log(format!("Default proxy changed but state could not be saved: {e}")),
                    }
                    self.patch_ready_dirty = true;
                    self.rows_dirty = true;
                }

                ui.add_space(4.0);
                ui.checkbox(&mut self.show_all, "Show not-located");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let installed = rows.len() - missing_count;
                    ui.weak(format!("{installed} installed · {supported} compatible"));
                });
            },
        );

        // The Search box spans exactly [Scan games] .. [Add…]: its left edge lines
        // up with the Scan button and its right edge with the Add button, measured
        // from the row above so it tracks the real button positions.
        let search_outer_w = (add_max_x - scan_min_x).max(120.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), toolbar_btn_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let (lbl_rect, _) =
                    ui.allocate_exact_size(egui::vec2(label_col_w, toolbar_btn_h), egui::Sense::hover());
                ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(lbl_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                )
                .label(egui::RichText::new("Search").strong().size(13.0));

                let (box_rect, _) = ui
                    .allocate_exact_size(egui::vec2(search_outer_w, toolbar_btn_h - 2.0), egui::Sense::hover());
                ui.painter().rect_filled(box_rect, egui::CornerRadius::same(6), BG);
                ui.painter().rect_stroke(
                    box_rect,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(1.0_f32, BORDER),
                    egui::StrokeKind::Inside,
                );
                let text_rect = box_rect.shrink2(egui::vec2(8.0, 3.0));
                let mut text_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                text_ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .frame(false)
                        .hint_text("filter…")
                        .desired_width(text_rect.width()),
                );
            },
        );
        // Ground the table header: a divider with a balanced 4px gap above and
        // below. egui adds `item_spacing.y` right after the separator and the
        // separator's own band pads the line, so set that spacing to 4.0 just
        // across the divider (the cursor advances during the separator's
        // allocation, so restoring immediately still leaves the tightened gap)
        // and keep the grid's cells untouched.
        ui.add_space(4.0);
        let saved_item_spacing_y = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.add(egui::Separator::default().spacing(4.0));
        ui.spacing_mut().item_spacing.y = saved_item_spacing_y;

        if rows.is_empty() {
            card_frame().show(ui, |ui| {
                ui.label(if self.show_all || !q.is_empty() {
                    "No games match. Try a different search or click \"Scan games\"."
                } else {
                    "No games have a render executable yet. Click \"Scan games\" or \"Add install folder…\"."
                });
            });
            return;
        }

        // Column widths are derived from the available width so the table can never
        // grow wider than the update/GPU cards above it, no matter how long a title
        // or path is. Long text is truncated (tooltip shows the full value).
        // Column widths are derived so the six columns plus the five inter-column
        // gaps sum to exactly the available width: no horizontal scrollbar at any
        // window size, and no dead gap that shifts Status away from the exe text.
        // See `column_widths` for the priority-based shrinking.
        // Reserve ~20px for the vertical scrollbar + right margin; sizing the
        // columns to the full available width otherwise overflows by exactly that
        // much and spawns a horizontal scrollbar (and lets it overlap [Open dir]).
        let avail = (ui.available_width() - 20.0).max(0.0);
        let ColumnWidths {
            name: name_w,
            compat: compat_w,
            exe: exe_w,
            status: status_w,
            proxy: proxy_w,
            actions: actions_w,
        } = column_widths(avail);
        egui::ScrollArea::vertical()
            .id_salt("games_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("games_grid")
                    .num_columns(6)
                    .spacing([GRID_SPACING_X, 4.0])
                    .min_col_width(24.0)
                    .show(ui, |ui| {
                        grid_header(ui, name_w, "Game", None);
                        grid_header_c(ui, compat_w, "Compatibility", None);
                        grid_header_c(ui, exe_w, "Game exe", None);
                        grid_header_c(ui, status_w, "Status", None);
                        grid_header_c(
                            ui,
                            proxy_w,
                            "Proxy",
                            Some("Proxy DLL placed next to the game executable. Click a row's proxy to change it."),
                        );
                        grid_header_c(ui, actions_w, "Actions", None);
                        ui.end_row();

                        let mut patched_header_shown = false;
                        let mut rest_header_shown = false;
                        let mut missing_header_shown = false;
                        // Whether any group label has been emitted yet: the first
                        // one sits right under the column titles and gets a
                        // smaller top pad so it doesn't ring off the header.
                        let mut any_section_shown = false;
                        for i in 0..rows.len() {
                            let row = &rows[i];
                            let is_p = row.patched;
                            if is_p && !patched_header_shown {
                                section_header_row(
                                    ui,
                                    name_w,
                                    "Patched",
                                    patched_count,
                                    SUCCESS,
                                    !any_section_shown,
                                );
                                patched_header_shown = true;
                                any_section_shown = true;
                            }
                            if row.missing && !missing_header_shown {
                                section_header_row(
                                    ui,
                                    name_w,
                                    "Missing folders",
                                    missing_count,
                                    WARN,
                                    !any_section_shown,
                                );
                                missing_header_shown = true;
                                any_section_shown = true;
                            }
                            if !is_p
                                && !row.missing
                                && patched_count + missing_count > 0
                                && !rest_header_shown
                            {
                                // No spacer row: the section header's own top
                                // allocation separates the block, and a dummy
                                // grid row would double egui's row spacing.
                                section_header_row(
                                    ui,
                                    name_w,
                                    "All other games",
                                    rows.len() - patched_count - missing_count,
                                    MUTED,
                                    !any_section_shown,
                                );
                                rest_header_shown = true;
                                any_section_shown = true;
                            }
                            let mut clicked: Option<GameAction> = None;

                            fixed_cell(ui, name_w, |ui| {
                                let (code, badge_color) = store_badge(&row.source);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(code).strong().monospace().small().color(badge_color),
                                    ),
                                )
                                .on_hover_text(format!("Found via {}", row.source));
                                ui.add(egui::Label::new(&row.name).truncate());
                                if let Some(ac) = row.anti_cheat.detected_name() {
                                    ui.add(egui::Label::new(egui::RichText::new("⚠").color(WARN)))
                                        .on_hover_text(format!("{ac} detected"));
                                }
                            });
                            {
                                let (text, color, tip) = &row.support_tag;
                                let r = fixed_cell_c(ui, compat_w, |ui| {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(text).color(*color)).truncate(),
                                    );
                                });
                                r.on_hover_text(tip);
                            }
                            match &row.render_exe {
                                Some(exe) if row.missing => {
                                    let full = exe.display().to_string();
                                    let r = fixed_cell_c(ui, exe_w, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(short_path(exe)).monospace().color(WARN),
                                            )
                                            .truncate(),
                                        );
                                    });
                                    r.on_hover_text(format!("Recorded location, now missing:\n{full}"));
                                }
                                Some(exe) => {
                                    let full = exe.display().to_string();
                                    let r = fixed_cell_c(ui, exe_w, |ui| {
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(short_path(exe)).monospace())
                                                .truncate(),
                                        );
                                    });
                                    r.on_hover_text(full);
                                }
                                None => {
                                    fixed_cell_c(ui, exe_w, |ui| {
                                        ui.colored_label(WARN, "not located");
                                    });
                                }
                            }
                            fixed_cell_c(ui, status_w, |ui| {
                                self.status_cell(ui, row);
                            });
                            {
                                let (cell_rect, _) =
                                    ui.allocate_exact_size(egui::vec2(proxy_w, 28.0), egui::Sense::hover());
                                if row.missing {
                                    let font = egui::FontId::proportional(14.0);
                                    let galley = ui.ctx().fonts(|f| {
                                        f.layout_no_wrap(row.proxy.clone(), font, MUTED)
                                    });
                                    let pos = egui::pos2(
                                        cell_rect.center().x - galley.size().x * 0.5,
                                        cell_rect.center().y - galley.size().y * 0.5,
                                    );
                                    ui.painter().with_clip_rect(cell_rect).galley(pos, galley, MUTED);
                                } else {
                                    let popup_id = ui.make_persistent_id(("row_proxy_popup", i));
                                    let is_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
                                    let btn_rect = egui::Rect::from_center_size(
                                        cell_rect.center(),
                                        egui::vec2((proxy_w - 2.0).max(76.0), 22.0),
                                    );
                                    let resp = ui.interact(btn_rect, popup_id.with("btn"), egui::Sense::click());
                                    if resp.hovered() || is_open {
                                        ui.painter().rect_filled(
                                            btn_rect,
                                            egui::CornerRadius::same(5),
                                            CARD_HOVER,
                                        );
                                        ui.painter().rect_stroke(
                                            btn_rect,
                                            egui::CornerRadius::same(5),
                                            egui::Stroke::new(
                                                1.0_f32,
                                                if is_open { ACCENT } else { ACCENT.gamma_multiply(0.5) },
                                            ),
                                            egui::StrokeKind::Inside,
                                        );
                                    }
                                    let font = egui::FontId::proportional(14.0);
                                    let galley = ui.ctx().fonts(|f| {
                                        f.layout_no_wrap(row.proxy.clone(), font, TEXT)
                                    });
                                    let text_x = (cell_rect.center().x - galley.size().x * 0.5 - 3.0)
                                        .max(btn_rect.min.x + 4.0);
                                    let text_y = cell_rect.center().y - galley.size().y * 0.5;
                                    ui.painter().with_clip_rect(btn_rect).galley(
                                        egui::pos2(text_x, text_y),
                                        galley.clone(),
                                        TEXT,
                                    );
                                    let cx = (text_x + galley.size().x + 7.0).min(btn_rect.max.x - 5.0);
                                    let cy = cell_rect.center().y + 1.0;
                                    let arrow_color = if resp.hovered() || is_open { TEXT } else { MUTED };
                                    let stroke = egui::Stroke::new(1.3_f32, arrow_color);
                                    ui.painter().line_segment([egui::pos2(cx - 3.0, cy - 1.5), egui::pos2(cx, cy + 1.5)], stroke);
                                    ui.painter().line_segment([egui::pos2(cx, cy + 1.5), egui::pos2(cx + 3.0, cy - 1.5)], stroke);

                                    let resp = resp.on_hover_text("Click to change proxy DLL for this game.");
                                    egui::Popup::menu(&resp)
                                        .id(popup_id)
                                        .show(|ui| {
                                            ui.set_min_width(96.0);
                                            for &p in crate::download::PROXY_NAMES {
                                                if ui.selectable_label(row.proxy == p, p).clicked() {
                                                    if p != row.proxy {
                                                        clicked = Some(GameAction::SetProxy(i, p.to_string()));
                                                    }
                                                    egui::Popup::close_id(ui.ctx(), popup_id);
                                                }
                                            }
                                        });
                                }
                            }
                            fixed_cell(ui, actions_w, |ui| {
                                // Cell-local spacing/padding, matched to the ACTION_*
                                // constants that `action_widths` sizes against.
                                ui.spacing_mut().item_spacing.x = ACTION_GAP_X;
                                ui.spacing_mut().button_padding.x = ACTION_PAD_X;
                                // The theme sets button_padding.y = 7.0, and egui
                                // sizes a button as max(min_size.y, text + 2*pad.y),
                                // which silently inflated every action button to
                                // ~30px and ignored btn_h. Drop the vertical pad so
                                // btn_h is respected and the buttons stay in-row.
                                ui.spacing_mut().button_padding.y = 2.0;
                                let btn_h = 22.0;
                                // Row buttons carry one of two tones; a tone drives the
                                // hover/active visuals, so they glow like every other
                                // button (see `add_toned_button`).
                                let row_sz = |w: f32| Some(egui::vec2(w, btn_h));
                                if row.missing {
                                    // Nothing to patch or unpatch on disk - the only
                                    // useful moves are pointing the record at the new
                                    // location or giving up on it.
                                    let recorded = row
                                        .render_exe
                                        .as_ref()
                                        .map(|e| e.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let (w, _) = action_widths(actions_w, 2, false);
                                    if add_toned_button(ui, BtnTone::AccentFill, "Relink", row_sz(w))
                                        .on_hover_text("Select the game's .exe in its new folder.")
                                        .clicked()
                                    {
                                        clicked = Some(GameAction::Relink(recorded.clone()));
                                    }
                                    if add_toned_button(ui, BtnTone::DangerOutline, "Forget", row_sz(w))
                                        .on_hover_text("Remove this missing entry from the list.")
                                        .clicked()
                                    {
                                        clicked = Some(GameAction::Forget(recorded.clone()));
                                    }
                                } else if let Some(exe) = &row.render_exe {
                                    let exe_str = exe.to_string_lossy().to_string();
                                    let patched = row.patched;
                                    let has_record = self.state.patch_for_exe(&exe_str).is_some();
                                    let is_manual = row.source == "Manual";
                                    let (w, x_w) = action_widths(actions_w, 2, is_manual);
                                    let bundled = crate::download::PROXY_NAMES.contains(&row.proxy.as_str());
                                    let can_patch = bundled
                                        && self.router().is_some()
                                        && self.files.as_ref().map(|f| f.is_ok()).unwrap_or(false);

                                    // The tips for the disabled variants read the
                                    // same for Patch and Repatch.
                                    let disabled_tip = || {
                                        if !bundled {
                                            format!(
                                                "{} proxy is not bundled; this title can't be patched yet. Bundled proxies: {}",
                                                row.proxy,
                                                crate::download::PROXY_NAMES.join(", ")
                                            )
                                        } else {
                                            "Needs a supported GPU and mod files (refresh above)".to_string()
                                        }
                                    };
                                    let needs = self.row_needs_update(row);
                                    let primary = primary_action(patched, has_record, can_patch, needs);
                                    match primary {
                                        RowAction::Unpatch => {
                                            let tip = if has_record && !patched {
                                                "The patch files are already gone; Unpatch clears the leftover record."
                                            } else {
                                                "Remove the patch from this game and restore the original files."
                                            };
                                            if add_toned_button(ui, BtnTone::DangerOutline, "Unpatch", row_sz(w))
                                                .on_hover_text(tip)
                                                .clicked()
                                            {
                                                clicked = Some(GameAction::Unpatch(
                                                    exe.clone(),
                                                    row.proxy.clone(),
                                                    row.name.clone(),
                                                ));
                                            }
                                        }
                                        RowAction::Repatch => {
                                            let tip = if needs {
                                                "Update this game's patch to the current mod version and the selected proxy DLL."
                                            } else {
                                                "Overwrite existing mod files with the current version."
                                            };
                                            if add_toned_button(ui, BtnTone::AccentFill, "Repatch", row_sz(w))
                                                .on_hover_text(tip)
                                                .clicked()
                                            {
                                                clicked = Some(GameAction::Patch(i));
                                            }
                                        }
                                        RowAction::Patch => {
                                            if add_toned_button(ui, BtnTone::AccentFill, "Patch", row_sz(w)).clicked() {
                                                clicked = Some(GameAction::Patch(i));
                                            }
                                        }
                                        RowAction::DisabledRepatch => {
                                            add_toned_button_enabled(ui, BtnTone::AccentFill, "Repatch", row_sz(w), false)
                                                .on_disabled_hover_text(disabled_tip());
                                        }
                                        RowAction::DisabledPatch => {
                                            add_toned_button_enabled(ui, BtnTone::AccentFill, "Patch", row_sz(w), false)
                                                .on_disabled_hover_text(disabled_tip());
                                        }
                                    }

                                    match second_slot(primary, needs) {
                                        SecondSlot::OpenDir => {
                                            if ui.add(egui::Button::new("Open dir").min_size(egui::vec2(w, btn_h))).clicked()
                                                && row.install_dir.is_dir()
                                            {
                                                let _ = std::process::Command::new("explorer").arg(&row.install_dir).spawn();
                                            }
                                        }
                                        SecondSlot::Unpatch => {
                                            if add_toned_button(ui, BtnTone::DangerOutline, "Unpatch", row_sz(w))
                                                .on_hover_text("Remove the patch from this game and restore the original files, without upgrading it first.")
                                                .clicked()
                                            {
                                                clicked = Some(GameAction::Unpatch(
                                                    exe.clone(),
                                                    row.proxy.clone(),
                                                    row.name.clone(),
                                                ));
                                            }
                                        }
                                    }
                                    if is_manual {
                                        if add_toned_button(ui, BtnTone::DangerOutline, "X", row_sz(x_w))
                                            .on_hover_text("Remove from list (files on disk are not changed).")
                                            .clicked()
                                        {
                                            clicked = Some(GameAction::RemoveManual(
                                                row.install_dir.to_string_lossy().to_string(),
                                            ));
                                        }
                                    }
                                } else {
                                    // Not located: no render exe discovered yet.
                                    let is_manual = row.source == "Manual";
                                    let (w, x_w) = action_widths(actions_w, 2, is_manual);
                                    if ui.add(egui::Button::new("Locate").min_size(egui::vec2(w, btn_h))).clicked() {
                                        clicked = Some(GameAction::Locate {
                                            name: row.name.clone(),
                                            install_dir: row.install_dir.clone(),
                                            proxy: row.proxy.clone(),
                                        });
                                    }
                                    if ui.add(egui::Button::new("Open dir").min_size(egui::vec2(w, btn_h))).clicked()
                                        && row.install_dir.is_dir()
                                    {
                                        let _ = std::process::Command::new("explorer").arg(&row.install_dir).spawn();
                                    }
                                    if is_manual {
                                        if add_toned_button(ui, BtnTone::DangerOutline, "X", row_sz(x_w))
                                            .on_hover_text("Remove from list (files on disk are not changed).")
                                            .clicked()
                                        {
                                            clicked = Some(GameAction::RemoveManual(
                                                row.install_dir.to_string_lossy().to_string(),
                                            ));
                                        }
                                    }
                                }
                            });
                            ui.end_row();

                            if let Some(action) = clicked {
                                match action {
                                    GameAction::Patch(idx) => {
                                        let row = rows[idx].clone();
                                        if row.anti_cheat.is_detected() {
                                            self.patch_confirm = Some(row);
                                        } else {
                                            self.do_patch(&row);
                                        }
                                    }
                                    GameAction::SetProxy(idx, p) => {
                                        let row = rows[idx].clone();
                                        let key = row.install_dir.to_string_lossy().to_string();
                                        self.state.set_proxy_override(&key, p.clone());
                                        if let Some(m) = self.state.manual_games.iter_mut()
                                            .find(|m| m.install_dir.eq_ignore_ascii_case(&key))
                                        {
                                            m.proxy = p.clone();
                                        }
                                        match save_state(&self.state) {
                                            Ok(_) => self.log(format!("{} will use {p}", row.name)),
                                            Err(e) => self.log(format!("Proxy changed but state could not be saved: {e}")),
                                        }
                                        self.patch_ready_dirty = true;
                                        self.rows_dirty = true;
                                    }
                                    GameAction::Unpatch(exe, proxy, name) => {
                                        self.do_unpatch(&exe.to_string_lossy(), &proxy, &name)
                                    }
                                    GameAction::Locate { name, install_dir, proxy } => {
                                        self.do_locate(&name, &install_dir, &proxy)
                                    }
                                    GameAction::Relink(old_exe) => self.do_relink(&old_exe),
                                    GameAction::Forget(exe) => {
                                        let name = self
                                            .state
                                            .patch_for_exe(&exe)
                                            .map(|r| r.game_name.clone())
                                            .unwrap_or_else(|| exe.clone());
                                        patcher::remove_patch_record(&mut self.state, &exe);
                                        self.patch_ready_dirty = true;
                                        self.rows_dirty = true;
                                        match save_state(&self.state) {
                                            Ok(_) => self.log(format!("Removed missing entry for {name}")),
                                            Err(e) => self.log(format!(
                                                "Forgot {name} but failed to save state: {e}"
                                            )),
                                        }
                                    }
                                    GameAction::RemoveManual(dir) => {
                                        let before = self.state.manual_games.len();
                                        let key = dir.to_lowercase();
                                        self.state
                                            .manual_games
                                            .retain(|m| m.install_dir.to_lowercase() != key);
                                        if self.state.manual_games.len() == before {
                                            self.log("That folder is no longer tracked.".into());
                                        } else {
                                            self.patch_ready_dirty = true;
                                            self.rows_dirty = true;
                                            match save_state(&self.state) {
                                                Ok(_) => self.log(format!("Removed {dir} from list")),
                                                Err(e) => self.log(format!(
                                                    "Removed {dir} but failed to save state: {e}"
                                                )),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    });
            });
    }

    fn status_cell(&self, ui: &mut egui::Ui, row: &Row) {
        let status_label = |ui: &mut egui::Ui, color: egui::Color32, text: String, tip: Option<&str>| {
            let r = ui.add(egui::Label::new(egui::RichText::new(text).color(color)));
            if let Some(tip) = tip {
                r.on_hover_text(tip);
            }
        };
        let Some(exe) = &row.render_exe else {
            status_label(ui, WARN, "pending".into(), None);
            return;
        };
        if row.missing {
            status_label(
                ui,
                WARN,
                "missing".into(),
                Some("Game folder not found at its previous path."),
            );
            return;
        }
        match self.state.patch_for_exe(&exe.to_string_lossy()) {
            Some(r) if self.is_patched_exe(Some(exe)) => {
                let cached = match &self.files {
                    Some(Ok(f)) => f.version.as_deref(),
                    _ => None,
                };
                if r.proxy_dll != row.proxy {
                    status_label(
                        ui, WARN,
                        format!("Patched (-> {})", row.proxy),
                        Some("Click Repatch to switch to the selected proxy DLL."),
                    );
                } else if needs_update(false, patcher::has_runtime_dir(exe), cached, r.mod_version.as_deref())
                {
                    let old = r.mod_version.as_deref().unwrap_or("unknown");
                    let new = cached.unwrap_or("latest");
                    status_label(
                        ui, WARN,
                        format!("Patched (v{old} -> v{new})"),
                        Some("Outdated patch on disk. Click Repatch to upgrade."),
                    );
                } else {
                    match r.mod_version.as_deref() {
                        Some(v) => status_label(ui, SUCCESS, format!("Patched ({} \u{b7} v{v})", r.router), None),
                        None => status_label(ui, SUCCESS, format!("Patched ({})", r.router), None),
                    }
                }
            }
            Some(_) => {
                status_label(
                    ui,
                    WARN,
                    "needs repatch".into(),
                    Some("Mod files were removed or overwritten (e.g. by a game update)."),
                );
            }
            None if self.is_patched_exe(Some(exe)) => {
                status_label(
                    ui,
                    SUCCESS,
                    "Patched (detected)".into(),
                    Some("Installed manually or by another tool. Click Repatch to update."),
                );
            }
            None => match &self.files {
                Some(Ok(_)) => {
                    status_label(ui, MUTED, "ready".into(), None);
                }
                Some(Err(_)) => {
                    status_label(ui, ERROR, "bundle missing".into(), None);
                }
                None => {
                    status_label(ui, MUTED, "checking…".into(), None);
                }
            },
        }
    }

    fn do_patch(&mut self, row: &Row) {
        let Some(exe) = &row.render_exe else { return };
        let Some(router) = self.router() else { return };
        match &self.files {
            Some(Ok(_)) => {}
            Some(Err(err)) => {
                self.log(format!("Cannot patch: mod files unavailable ({err}). Refresh them first."));
                return;
            }
            None => {
                self.log("Cannot patch: mod files unavailable (no source configured). Refresh them first.".into());
                return;
            }
        }
        let mod_version = match &self.files {
            Some(Ok(f)) => f.version.clone(),
            _ => None,
        };
        let using = mod_version.clone().unwrap_or_else(|| "unknown".to_string());
        self.log(format!("Patching {} ({}) with DLSSG v{using}…", row.name, row.proxy));
        let req = InstallRequest {
            game_name: row.name.clone(),
            install_dir: row.install_dir.clone(),
            render_exe: exe.clone(),
            proxy_dll: row.proxy.clone(),
            router: router.to_string(),
            cache_dir: self.cache_root.join("cache"),
            state_dir: self.state_dir.clone(),
            mod_version,
            previous_record: self.state.patch_for_exe(&exe.to_string_lossy()).cloned(),
        };
        self.spawn(move || match patcher::install(&req) {
            Ok(rec) => Msg::Patched(Box::new(rec)),
            Err(e) => Msg::Failed(format!("patch {}: {e}", req.game_name)),
        });
    }

    fn do_unpatch(&mut self, exe: &str, proxy: &str, game_name: &str) {
        if let Some(record) = self.state.patch_for_exe(exe).cloned() {
            let exe = record.render_exe.clone();
            let name = record.game_name.clone();
            self.log(format!("Unpatching {name}…"));
            self.spawn(move || match patcher::uninstall(&record) {
                Ok(report) => Msg::Unpatched(exe, name, report),
                Err(e) => Msg::Failed(format!("unpatch {name}: {e}")),
            });
        } else {
            let exe = exe.to_string();
            let proxy = proxy.to_string();
            let name = game_name.to_string();
            self.log(format!("Removing manually-installed patch from {name}…"));
            self.spawn(move || match patcher::uninstall_manual(&exe, &proxy) {
                Ok(_) => Msg::Unpatched(exe, name, patcher::UninstallReport::default()),
                Err(e) => Msg::Failed(format!("unpatch {name}: {e}")),
            });
        }
    }

    /// Re-point a patch record at the game's new location after the folder moved.
    ///
    /// Without this the record is stranded: the old absolute path no longer exists,
    /// so Unpatch could never restore the game's own DLLs and the row was invisible
    /// anyway. Backups are rebased onto the new folder so a later Unpatch still
    /// restores originals in the right place.
    fn do_relink(&mut self, old_exe: &str) {
        let Some(record) = self.state.patch_for_exe(old_exe).cloned() else {
            self.log("That patch record is already gone.".into());
            return;
        };
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("Executable", &["exe"])
            .pick_file()
        else {
            return;
        };
        if picked.as_os_str().is_empty() {
            return;
        }

        let mut updated = record.clone();
        patcher::relink_record(&mut updated, &picked);
        let name = updated.game_name.clone();

        if let Some(slot) = self
            .state
            .patches
            .iter_mut()
            .find(|p| exe_key(&p.render_exe) == exe_key(old_exe))
        {
            *slot = updated.clone();
        }
        // Seed the cache so the row reads correctly before the next rebuild.
        self.patch_ready
            .insert(exe_key(&updated.render_exe), patcher::looks_patched(&updated));
        self.patch_ready_dirty = true;
        self.rows_dirty = true;

        match save_state(&self.state) {
            Ok(_) => self.log(format!("Relinked {name} to {}", updated.render_exe)),
            Err(e) => self.log(format!("Relinked {name} but failed to save state: {e}")),
        }
    }

    /// Uninstall every recorded patch (restoring backed-up originals), remove
    /// manual patches, then delete the whole app-data folder. Runs off the UI
    /// thread; the result comes back as `Msg::Erased`.
    fn erase_everything(&mut self) {
        self.log("Erasing: removing every patch and all tool data…".into());
        let state = self.state.clone();
        let state_dir = self.state_dir.clone();
        self.spawn(move || {
            let out = patcher::erase(&state, &state_dir);
            let mut text = format!("Removed {} patch(es) and deleted all tool data.", out.patches_removed);
            if !out.stranded_backups.is_empty() {
                text.push_str(&format!(
                    " Kept {} unrestored backup(s) in {}.",
                    out.stranded_backups.len(),
                    state_dir.join("backups").display()
                ));
            }
            if !out.problems.is_empty() {
                text.push_str(&format!(" Problems: {}", out.problems.join("; ")));
            }
            let stranded = out.stranded_backups.clone();
            Msg::Erased(
                if out.problems.is_empty() { Ok(text) } else { Err(text) },
                stranded.is_empty(),
            )
        });
    }

    fn do_locate(&mut self, name: &str, install_dir: &std::path::Path, proxy: &str) {
        let mut dialog = rfd::FileDialog::new();
        if install_dir.is_dir() {
            dialog = dialog.set_directory(install_dir);
        }
        if let Some(picked) = dialog.add_filter("Executable", &["exe"]).pick_file() {
            self.add_manual_game(name, install_dir, &picked, proxy);
        }
    }

    fn add_install_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
        let folder_name = dir
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "Game".into());
        let manifest_game = find_manifest_game(&self.manifest, &folder_name);
        let name = manifest_game.map(|m| m.name.clone()).unwrap_or_else(|| folder_name.clone());
        let rel = manifest_game.map(|m| m.render_exe.clone());
        let render_exe = rel
            .map(|r| fix_seps(dir.join(&r)))
            .filter(|p| p.is_file());
        let proxy = self.resolve_game_proxy(&dir, render_exe.as_ref(), manifest_game);

        match render_exe {
            Some(exe) => self.add_manual_game(&name, &dir, &exe, &proxy),
            None => {
                let start = {
                    let mut d = rfd::FileDialog::new().set_directory(dir.clone());
                    d = d.add_filter("Executable", &["exe"]);
                    d.pick_file()
                };
                if let Some(exe) = start {
                    self.add_manual_game(&name, &dir, &exe, &proxy);
                }
            }
        }
    }

    fn add_manual_game(&mut self, name: &str, install_dir: &std::path::Path, render_exe: &std::path::Path, proxy: &str) {
        let dir_key = exe_key(&install_dir.to_string_lossy());
        if let Some(g) = self
            .scanned
            .iter_mut()
            .find(|g| exe_key(&g.install_dir.to_string_lossy()) == dir_key)
        {
            g.detected_exe = Some(render_exe.to_path_buf());
        }
        if let Some(existing) = self
            .state
            .manual_games
            .iter_mut()
            .find(|m| exe_key(&m.install_dir) == dir_key)
        {
            existing.render_exe = render_exe.to_string_lossy().to_string();
            existing.proxy = proxy.to_string();
        } else {
            self.state.manual_games.push(ManualGame {
                name: name.to_string(),
                install_dir: install_dir.to_string_lossy().to_string(),
                render_exe: render_exe.to_string_lossy().to_string(),
                proxy: proxy.to_string(),
            });
        }
        self.patch_ready_dirty = true;
        self.rows_dirty = true;
        match save_state(&self.state) {
            Ok(_) => self.log(format!("Added game: {name} ({})", render_exe.display())),
            Err(e) => self.log(format!("Added {name} but failed to save state: {e}")),
        }
    }
}

#[derive(Clone)]
struct Row {
    name: String,
    install_dir: PathBuf,
    render_exe: Option<PathBuf>,
    proxy: String,
    /// (label, color, tooltip) shown in the Compatibility column.
    support_tag: (String, egui::Color32, String),
    /// Whether the mod files are currently on disk for this game. Cached at row
    /// build time so the table never re-hashes DLLs or hits the filesystem per frame.
    patched: bool,
    /// Which launcher/storefront discovered this game ("Steam", "Epic", …).
    source: String,
    /// Detected anti-cheat system, if any. Patching such a game is gated behind a
    /// confirmation.
    anti_cheat: anticheat::AcScan,
    /// This row was synthesized from a patch record whose install folder no longer
    /// exists. Without it the record would have no row at all, and therefore no
    /// Unpatch / Relink / Forget button anywhere in the UI.
    missing: bool,
}

enum GameAction {
    Patch(usize),
    SetProxy(usize, String),
    Unpatch(PathBuf, String, String), // render_exe, proxy, game_name
    Locate {
        name: String,
        install_dir: PathBuf,
        proxy: String,
    },
    /// Re-point an orphaned patch record at the game's new location.
    Relink(String),
    /// Drop an orphaned patch record without touching the filesystem.
    Forget(String),
    /// Drop a manually added folder from the tracked list. Leaves the game and any
    /// patch on disk untouched.
    RemoveManual(String),
}

fn fix_seps(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().replace('/', "\\");
    PathBuf::from(s)
}

/// Show just the file name, truncated, so long absolute paths don't blow out the table.
fn short_path(p: &Path) -> String {
    let name = p.file_name().map(|f| f.to_string_lossy()).unwrap_or_default().to_string();
    let mut out: String = name.chars().take(26).collect();
    if name.chars().count() > 26 {
        out.push('\u{2026}');
    }
    out
}

/// Short, color-coded storefront tag shown before the game name ("STE", "EPI", …).
fn store_badge(source: &str) -> (&'static str, egui::Color32) {
    match source {
        "Steam" => ("STE", egui::Color32::from_rgb(102, 170, 255)),
        "Epic" => ("EPI", egui::Color32::from_rgb(214, 214, 214)),
        "GOG" => ("GOG", egui::Color32::from_rgb(167, 139, 250)),
        "Ubisoft" => ("UBI", egui::Color32::from_rgb(0, 170, 230)),
        "EA" => ("EA", egui::Color32::from_rgb(255, 120, 110)),
        "Manual" => ("USR", MUTED),
        "Missing" => ("???", WARN),
        _ => ("•", MUTED),
    }
}

/// True when the render exe's directory ships the real DLSS Frame Generation DLL.
/// That's a reliable, offline signal that the game truly supports FG and our
/// version.dll patch will work.
fn has_fg_dll(exe: &Path) -> bool {
    exe.parent()
        .map(|d| d.join("nvngx_dlssg.dll").is_file())
        .unwrap_or(false)
}

/// A column title inside the games grid, muted so the data rows stand out.
/// A grid cell fixed to a given width so the total table width can't exceed the
/// panel. Content clips; callers add tooltips via the returned Response.
///
/// IMPORTANT (egui 0.32 internals): a Grid sizes each column by the _actually
/// used_ rect of its cells, not the rect you ask for. `allocate_ui_with_layout`
/// records the child's final `min_rect` (content width), so cells that don't
/// fill their allocation (left-aligned truncate labels) collapse the column to
/// the text and everything packs together. We therefore allocate the exact cell
/// rect first - which records `width` into the grid via `advance_after_rects` -
/// and then paint the content in a `new_child` `Ui`. `new_child` does NOT
/// allocate or advance the parent's grid cursor, so we must not advance again
/// afterwards (that would skip a column / double-advance).
/// Six grid columns whose widths plus the five inter-column gaps sum to exactly
/// `avail`. Wide window: the title column absorbs the surplus. Narrow window:
/// columns shrink by priority so nothing overflows (no horizontal scroll).
pub(crate) const GRID_SPACING_X: f32 = 12.0;

pub(crate) struct ColumnWidths {
    pub name: f32,
    pub compat: f32,
    pub exe: f32,
    pub status: f32,
    pub proxy: f32,
    pub actions: f32,
}

pub(crate) fn column_widths(avail: f32) -> ColumnWidths {
    const GAPS: f32 = 5.0 * GRID_SPACING_X;
    let budget = (avail - GAPS).max(0.0);
    let mut w = [
        (avail * 0.28).clamp(140.0, 2000.0), // 0 name
        (avail * 0.11).clamp(72.0, 130.0),   // 1 compat
        (avail * 0.15).clamp(142.0, 185.0),  // 2 exe
        (avail * 0.14).clamp(180.0, 200.0),  // 3 status
        (avail * 0.10).clamp(102.0, 114.0),  // 4 proxy: fits the per-game selector
        (avail * 0.19).clamp(168.0, 210.0),  // 5 actions
    ];
    let mut over = w.iter().sum::<f32>() - budget;
    for (idx, floor) in [(0usize, 78.0f32), (2, 104.0), (3, 174.0), (4, 102.0), (1, 64.0), (5, 150.0)] {
        if over <= 0.0 {
            break;
        }
        let take = (w[idx] - floor).max(0.0).min(over);
        w[idx] -= take;
        over -= take;
    }
    if over < 0.0 {
        w[0] -= over;
    } else {
        w[0] = (w[0] - over).max(0.0);
    }
    ColumnWidths { name: w[0], compat: w[1], exe: w[2], status: w[3], proxy: w[4], actions: w[5] }
}

/// The enabled/disabled primary action shown in a located row's actions cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowAction {
    Patch,
    Repatch,
    Unpatch,
    DisabledPatch,
    DisabledRepatch,
}

/// What the actions cell's second text slot shows. Normally "Open dir"; on a row whose primary
/// was promoted to Repatch it is the Unpatch escape hatch instead, so a row that merely needs
/// updating can still be removed without being upgraded first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecondSlot {
    OpenDir,
    Unpatch,
}

pub(crate) fn second_slot(primary: RowAction, needs_update: bool) -> SecondSlot {
    if needs_update && primary == RowAction::Repatch {
        SecondSlot::Unpatch
    } else {
        SecondSlot::OpenDir
    }
}

/// The upgrade decision, free of the app so it can be tested: true when a patched game's on-disk
/// state or saved record differs from what the UI would install now — the selected proxy changed,
/// the runtime tree is missing, or the cached bundle is newer than the record's version.
pub(crate) fn needs_update(
    proxy_changed: bool,
    has_runtime: bool,
    cached_version: Option<&str>,
    recorded_version: Option<&str>,
) -> bool {
    if proxy_changed || !has_runtime {
        return true;
    }
    match (cached_version, recorded_version) {
        (Some(c), Some(r)) => crate::download::version_cmp(c, r).is_gt(),
        (Some(_), None) => true,
        _ => false,
    }
}

/// The primary button for a located row. `needs_update` promotes Repatch over the Unpatch that
/// `row_action` would otherwise pick — but only when we can actually patch, so a record can
/// never get stuck with no way out.
pub(crate) fn primary_action(
    patched: bool,
    has_record: bool,
    can_patch: bool,
    needs_update_flag: bool,
) -> RowAction {
    if patched && has_record && can_patch && needs_update_flag {
        RowAction::Repatch
    } else {
        row_action(patched, has_record, can_patch)
    }
}

/// Pick the primary action for a located row. `patched` is "looks patched on
/// disk", `has_record` is "we have a patch record for this exe", and
/// `can_patch` is "mod files are loaded and a supported GPU was found".
///
/// The important case is the stale record (`!patched && has_record`): if we
/// can reinstall, Patch heals it, otherwise the only escape is Unpatch, which
/// clears the record (`patcher::uninstall` tolerates the already-missing
/// files). It must never resolve to a disabled action, or the record is stuck.
pub(crate) fn row_action(patched: bool, has_record: bool, can_patch: bool) -> RowAction {
    match (patched, has_record, can_patch) {
        // Ours, on disk: the way out is to remove it.
        (true, true, _) => RowAction::Unpatch,
        // Detected on disk but not installed by us, so the version is unknown.
        (true, false, true) => RowAction::Repatch,
        (true, false, false) => RowAction::DisabledRepatch,
        // Stale record: files gone but the record remains. Reinstall if we can,
        // otherwise Unpatch is the only escape and must stay enabled.
        (false, true, true) => RowAction::Patch,
        (false, true, false) => RowAction::Unpatch,
        (false, false, true) => RowAction::Patch,
        (false, false, false) => RowAction::DisabledPatch,
    }
}

/// Horizontal padding inside each actions-cell button. The cell sets
/// `button_padding.x` to this, and the label-fit test measures real labels
/// with it.
pub(crate) const ACTION_PAD_X: f32 = 3.0;
/// Gap between buttons in an actions cell (the cell's `item_spacing.x`).
pub(crate) const ACTION_GAP_X: f32 = 4.0;
/// Width reserved for the trailing `[X]` (manual rows only).
pub(crate) const ACTION_X_W: f32 = 18.0;

/// `(text_button_width, x_button_width)` for one row's actions cell. Equal text
/// buttons; the optional `X` is a fixed `ACTION_X_W`. Gaps use `ACTION_GAP_X`.
pub(crate) fn action_widths(actions_w: f32, text_buttons: usize, has_x: bool) -> (f32, f32) {
    let x_w = if has_x { ACTION_X_W } else { 0.0 };
    let gaps = (text_buttons.saturating_sub(1) + has_x as usize) as f32 * ACTION_GAP_X;
    let text_w = ((actions_w - x_w - gaps) / text_buttons.max(1) as f32).floor().max(0.0);
    (text_w, x_w)
}

/// Height of a toolbar/row control: button text height + vertical button padding.
fn row_control_height(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y
}

const SCAN_LAUNCHER_WEIGHT: f32 = 0.4;

fn ratio(done: usize, total: usize) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn scan_launcher_fraction(done: usize, total: usize) -> f32 {
    SCAN_LAUNCHER_WEIGHT * ratio(done, total)
}

fn scan_game_fraction(done: usize, total: usize) -> f32 {
    SCAN_LAUNCHER_WEIGHT + (1.0 - SCAN_LAUNCHER_WEIGHT) * ratio(done, total)
}

fn toolbar_button_width(ui: &egui::Ui, label: &str) -> f32 {
    let font = ui.style().text_styles[&egui::TextStyle::Button].clone();
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE));
    galley.size().x + 2.0 * ui.spacing().button_padding.x
}

fn fixed_cell(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    add(&mut child);
    response
}

/// Height of a column-title row. Shorter than the 28px data rows - a header is a
/// single line, so a full-height cell just floats the label a few px down inside
/// it (which, with the divider's band and egui's item spacing, read as a large
/// gap under the toolbar).
const HEADER_ROW_H: f32 = 20.0;

/// Paint a single-line string in a fake-bold weight inside an allocated `rect`.
///
/// egui's stock proportional font ships one weight, and `RichText::strong()`
/// only maps to `strong_text_color()` (it is dropped entirely when an explicit
/// `.color()` is also set - see `WidgetText::get_text_color`). So, like
/// `bold_heading`, fake bold by painting the same galley twice a hair apart.
/// The galley is truncated to the cell width, so a header wider than a shrunk
/// column gets an ellipsis rather than being cut off.
fn paint_bold_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    size: f32,
    color: egui::Color32,
    centered: bool,
    y_override: Option<f32>,
) {
    let rich = egui::RichText::new(text.to_owned()).size(size).color(color);
    let galley = egui::WidgetText::from(rich).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    let gsize = galley.size();
    let x = if centered {
        rect.min.x + ((rect.width() - gsize.x) * 0.5).max(0.0)
    } else {
        rect.min.x
    };
    let y = y_override.unwrap_or_else(|| rect.min.y + (rect.height() - gsize.y) * 0.5);
    let painter = ui.painter().with_clip_rect(rect);
    painter.galley(egui::pos2(x + 0.7, y), galley.clone(), color);
    painter.galley(egui::pos2(x, y), galley, color);
}

/// Shared body for the column-title helpers: one fixed-width cell, one
/// `HEADER_ROW_H` tall, left-aligned or horizontally centered, painted in bold
/// `TEXT` so the titles pop above the table, with the tooltip on the whole cell.
fn header_cell(ui: &mut egui::Ui, width: f32, centered: bool, text: &str, tip: Option<&str>) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, HEADER_ROW_H), egui::Sense::hover());
    paint_bold_text(ui, rect, text, 14.0, TEXT, centered, None);
    if let Some(tip) = tip {
        response.on_hover_text(tip);
    }
}

/// Same fixed-width cell as `fixed_cell`, but the content is centered
/// horizontally instead of left-aligned. Used for the columns whose text width
/// swings wildly ("Patched (SM86 · v0.3.5)" vs "ready"), so short and long
/// values both read at the same optical position as the centered header above.
fn fixed_cell_c(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight)),
    );
    add(&mut child);
    response
}

/// Left-aligned column-title cell: the "Game" header sits flush with the
/// left-aligned store-badge / title cells beneath it.
fn grid_header(ui: &mut egui::Ui, width: f32, text: &str, tip: Option<&str>) {
    header_cell(ui, width, false, text, tip);
}

/// Centered column-title cell. The other five columns use this so a header sits
/// over the optical center of its (variably wide) values, matching the centered
/// data cells from `fixed_cell_c`.
fn grid_header_c(ui: &mut egui::Ui, width: f32, text: &str, tip: Option<&str>) {
    header_cell(ui, width, true, text, tip);
}

/// A group label inside the grid ("Patched (3)", "All other games (n)").
///
/// IMPORTANT: the label cell must stay inside the FIRST column (`width` =
/// name_w). A cell wider than the first column would push column 0 to that
/// width (egui Grid sizes columns by their widest cell) and shove every other
/// column off the table.
///
/// The cell is `LABEL_H + top_pad` tall and the label is bottom-aligned, so the
/// padding lands *above* the label. That is what attaches a section to the rows
/// it introduces: a little air above (separating it from the block above) and
/// none below. `first_section` (the first group, sitting right under the column
/// titles) gets a smaller pad so it doesn't ring off the header; later groups
/// get a larger one to read as a break. No horizontal `add_space`: the label
/// must share the exact left edge of the "Game" header and the store badges.
fn section_header_row(
    ui: &mut egui::Ui,
    width: f32,
    caption: &str,
    count: usize,
    color: egui::Color32,
    first_section: bool,
) {
    const LABEL_H: f32 = 17.0;
    let top_pad = if first_section { 2.0 } else { 8.0 };
    let row_h = LABEL_H + top_pad;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
    // Bold (fake weight - the stock fonts have none) and pushed down by `top_pad`
    // so the label sits at the bottom of the cell, hugging the rows it labels.
    let text = format!("{caption}   {count}");
    paint_bold_text(ui, rect, &text, 14.0, color, false, Some(rect.min.y + top_pad));
    ui.end_row();
}

/// Human-readable file size for the "Placed …" log line.
fn patched_size(dir: String, file: &str) -> String {
    let p = std::path::Path::new(&dir).join(file);
    if p.is_dir() {
        // `dlssg_sm86` is the runtime tree; its own size is the sum of its files, and
        // "0 B" for a directory reads as a failure.
        return "runtime dir".to_string();
    }
    let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

/// Pick the proxy for a row. First match wins; anything not in `PROXY_NAMES` is ignored.
pub(crate) fn pick_proxy(
    override_: Option<&str>,
    recorded: Option<&str>,
    detected: Option<&str>,
    catalog: Option<&str>,
    default: &str,
) -> String {
    for c in [override_, recorded, detected, catalog, Some(default)].into_iter().flatten() {
        // Case-insensitive on purpose: these names are Windows filenames, so a hand-edited
        // state.json (or a value written by an older build) may arrive as "WINMM.DLL". Return the
        // canonical spelling, never the caller's, so install and the UI can never disagree.
        if let Some(name) = crate::download::PROXY_NAMES
            .iter()
            .find(|n| n.eq_ignore_ascii_case(c))
        {
            return (*name).to_string();
        }
    }
    default_proxy() // its one remaining caller: the resolver's documented last resort
}

fn default_proxy() -> String {
    "version.dll".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for the games-table cursor. egui makes labels selectable
    /// by default and swaps the pointer to the text I-beam over a hovered label;
    /// the data cells are justified, so the hover rect covers the whole column
    /// and the arrow turned into an I-beam over empty space. The Games tab turns
    /// `selectable_labels` off - this checks that flip does stop the I-beam, and
    /// that the default would otherwise have caused it.
    #[test]
    fn selectable_labels_control_the_text_cursor() {
        fn hovered_cursor(selectable: bool) -> egui::CursorIcon {
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(240.0, 120.0));
            let frame = |pointer: Option<egui::Pos2>| {
                let mut input = egui::RawInput::default();
                input.screen_rect = Some(screen);
                input.events.push(match pointer {
                    Some(p) => egui::Event::PointerMoved(p),
                    None => egui::Event::PointerGone,
                });
                ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.style_mut().interaction.selectable_labels = selectable;
                        ui.with_layout(
                            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                            |ui| {
                                ui.label("game name");
                            },
                        );
                    });
                })
                .platform_output
                .cursor_icon
            };
            // First frame lays the label out; the second hovers its centre.
            frame(None);
            frame(Some(screen.center()))
        }

        assert_eq!(hovered_cursor(true), egui::CursorIcon::Text);
        assert_ne!(hovered_cursor(false), egui::CursorIcon::Text);
    }

    /// The grid must never need a horizontal scrollbar: the six column widths
    /// plus the five inter-column gaps have to add up to exactly the available
    /// width. The app feeds `available_width() - 20`, and the smallest useful
    /// window (760px minus panel margins/scrollbar reserve) lands near 708, so
    /// the invariant is checked from well below that upward. (`name` is clamped
    /// to 0 below ~654, where the sum can no longer hold — that is below any
    /// reachable window.)
    #[test]
    fn column_widths_sum_to_available() {
        for avail in (660..=2400).step_by(20) {
            let c = column_widths(avail as f32);
            let sum = c.name + c.compat + c.exe + c.status + c.proxy + c.actions
                + 5.0 * GRID_SPACING_X;
            assert!((sum - avail as f32).abs() < 0.5, "avail {avail}: sum {sum}");
            for v in [c.name, c.compat, c.exe, c.status, c.proxy, c.actions] {
                assert!(v >= 0.0);
            }
        }
    }

    #[test]
    fn min_window_keeps_every_column_readable() {
        // 708 is the smallest width the grid actually sees (760px window).
        let c = column_widths(708.0);
        assert!(c.proxy >= 102.0, "proxy column must fit the per-game combo: {}", c.proxy);
        assert!(c.name >= 50.0, "the game name must not vanish: {}", c.name);
        assert!(c.actions >= 150.0);
    }

    /// Every row has the same buttons in the same slots, so their widths must be
    /// equal and the row must fit inside the actions column (the X slot counts).
    #[test]
    fn action_widths_are_equal_and_fit() {
        for actions_w in (150..=240).step_by(8) {
            for has_x in [false, true] {
                let (t, x) = action_widths(actions_w as f32, 2, has_x);
                assert!(t > 0.0);
                let gaps = if has_x { 2.0 } else { 1.0 } * ACTION_GAP_X;
                let used = 2.0 * t + x + gaps;
                assert!(used <= actions_w as f32 + 0.01, "actions_w {actions_w} x {has_x}: {used}");
            }
        }
    }

    /// Natural width of a button label as egui actually renders it in the
    /// actions cell: the `Button` text style plus the cell's horizontal padding.
    fn natural_button_width(ui: &egui::Ui, label: &str, pad_x: f32) -> f32 {
        ui.fonts(|f| {
            f.layout_no_wrap(
                label.to_owned(),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            )
        })
        .size()
        .x
            + 2.0 * pad_x
    }

    /// The actions cell must fit every button it can render, including the
    /// manual rows that carry the trailing `[X]`. `Button` width is
    /// `max(natural, min_size)`, so a label wider than the computed width
    /// overflows the column and collides with the scrollbar. This measures the
    /// real font metrics instead of trusting the estimates.
    #[test]
    fn action_labels_fit_at_min_window() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut nat: Vec<(&str, f32)> = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for l in ["Patch", "Unpatch", "Repatch", "Relink", "Forget", "Locate", "Open dir"] {
                    nat.push((l, natural_button_width(ui, l, ACTION_PAD_X)));
                }
            });
        });
        let width = |l: &str| nat.iter().find(|(k, _)| *k == l).unwrap().1;

        let shapes: &[(&[&str], bool)] = &[
            (&["Patch", "Open dir"], false),
            (&["Repatch", "Open dir"], false),
            (&["Unpatch", "Open dir"], false),
            (&["Patch", "Open dir"], true),
            (&["Repatch", "Open dir"], true),
            (&["Unpatch", "Open dir"], true),
            (&["Locate", "Open dir"], true),
            (&["Relink", "Forget"], false),
        ];
        for actions_w in (150..=240).step_by(2) {
            for (labels, has_x) in shapes {
                let (t, x) = action_widths(actions_w as f32, labels.len(), *has_x);
                let gaps = (labels.len().saturating_sub(1) + *has_x as usize) as f32 * ACTION_GAP_X;
                let mut used = 0.0;
                for l in *labels {
                    used += width(l).max(t);
                }
                used += if *has_x { x } else { 0.0 } + gaps;
                assert!(
                    used <= actions_w as f32 + 0.5,
                    "actions_w={actions_w} shape={labels:?} has_x={has_x}: used {used} > {actions_w}"
                );
            }
        }
    }

    /// A stale record (`patched == false`, `has_record == true`) whose Patch is
    /// disabled must still expose an enabled action so the record can always be
    /// cleared (Unpatch clears the record; `patcher::uninstall` is tolerant).
    #[test]
    fn stale_record_is_always_clearable() {
        assert_eq!(row_action(false, true, false), RowAction::Unpatch);
        assert_eq!(row_action(false, true, true), RowAction::Patch);
        assert_eq!(row_action(true, true, false), RowAction::Unpatch);
        assert_eq!(row_action(true, false, true), RowAction::Repatch);
        assert_eq!(row_action(true, false, false), RowAction::DisabledRepatch);
        assert_eq!(row_action(false, false, true), RowAction::Patch);
        assert_eq!(row_action(false, false, false), RowAction::DisabledPatch);
    }

    /// Guards the toolbar-divider spacing: the column titles must sit right
    /// under the divider line, not float in the default item-spacing gap.
    #[test]
    fn header_sits_close_below_divider() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut line_y = 0.0;
        let mut label_top = 0.0;
        // Two frames: the second is settled (allocated sizes are real then).
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(4.0);
                    let saved = ui.spacing().item_spacing.y;
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let sep = ui.add(egui::Separator::default().spacing(4.0));
                    ui.spacing_mut().item_spacing.y = saved;
                    line_y = sep.rect.center().y;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .id_salt("probe")
                        .show(ui, |ui| {
                            egui::Grid::new("probe_grid")
                                .num_columns(6)
                                .spacing([12.0, 4.0])
                                .min_col_width(24.0)
                                .show(ui, |ui| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(200.0, HEADER_ROW_H),
                                        egui::Sense::hover(),
                                    );
                                    let mut child = ui.new_child(
                                        egui::UiBuilder::new().max_rect(rect).layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                        ),
                                    );
                                    let lr = child.add(egui::Label::new(
                                        egui::RichText::new("Game").strong().color(MUTED),
                                    ));
                                    label_top = lr.rect.top();
                                });
                        });
                });
            });
        }
        let gap = label_top - line_y;
        assert!(
            (2.0..=10.0).contains(&gap),
            "column header floats {gap:.1}px below the divider line"
        );
    }

    /// The scan bar must only ever advance: the launcher phase spans
    /// `0.0..=SCAN_LAUNCHER_WEIGHT` and the per-game phase continues from that
    /// seam up to `1.0`, both non-decreasing over their whole range.
    #[test]
    fn scan_bar_fraction_never_moves_backwards() {
        assert_eq!(scan_launcher_fraction(0, 5), 0.0);
        assert_eq!(scan_launcher_fraction(5, 5), SCAN_LAUNCHER_WEIGHT);
        assert_eq!(scan_game_fraction(0, 7), scan_launcher_fraction(5, 5));
        assert_eq!(scan_game_fraction(7, 7), 1.0);

        let mut prev = f32::NEG_INFINITY;
        for i in 0..=5 {
            let v = scan_launcher_fraction(i, 5);
            assert!(v >= prev, "launcher {i}: {v} < {prev}");
            prev = v;
        }
        let mut prev = scan_launcher_fraction(5, 5);
        for i in 0..=7 {
            let v = scan_game_fraction(i, 7);
            assert!(v >= prev, "game {i}: {v} < {prev}");
            prev = v;
        }
    }

    /// A phase with no work (no launchers / no games) reports its end value
    /// immediately, so the bar never stalls below the seam.
    #[test]
    fn scan_bar_fraction_handles_an_empty_phase() {
        assert_eq!(scan_launcher_fraction(0, 0), SCAN_LAUNCHER_WEIGHT);
        assert_eq!(scan_game_fraction(0, 0), 1.0);
    }

    /// The progress bar has to fill the "Scan games" button's exact slot, so
    /// the toolbar's row height (`interact_size.y`) is reproduced here: both the
    /// button and the bar then size to it.
    #[test]
    fn scan_bar_matches_the_button_slot() {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut btn = egui::Rect::NOTHING;
        let mut bar = egui::Rect::NOTHING;
        // Two frames: the second is settled (allocated sizes are real then).
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.spacing_mut().interact_size.y = row_control_height(ui);
                    btn = ui.button("Scan games").rect;
                    bar = ui
                        .add(
                            egui::ProgressBar::new(0.5)
                                .desired_width(toolbar_button_width(ui, "Scan games")),
                        )
                        .rect;
                });
            });
        }
        assert!(
            (btn.width() - bar.width()).abs() < 0.5,
            "scan bar width {} vs button {}",
            bar.width(),
            btn.width()
        );
        assert!(
            (btn.height() - bar.height()).abs() < 0.5,
            "scan bar height {} vs button {}",
            bar.height(),
            btn.height()
        );
    }

    fn entry(name: &str, conf: Option<&str>, proxy: &str) -> ManifestGame {
        ManifestGame {
            name: name.into(),
            fg: Some("yes".into()),
            steam_appid: Some(1),
            match_confidence: conf.map(|s| s.into()),
            folders: vec![],
            render_exe: String::new(),
            proxy: proxy.into(),
        }
    }

    #[test]
    fn second_slot_keeps_unpatch_reachable_on_a_row_that_needs_updating() {
        assert_eq!(second_slot(RowAction::Repatch, true), SecondSlot::Unpatch);
        assert_eq!(second_slot(RowAction::Repatch, false), SecondSlot::OpenDir); // detected, not ours
        assert_eq!(second_slot(RowAction::Unpatch, false), SecondSlot::OpenDir);
        assert_eq!(second_slot(RowAction::Patch, true), SecondSlot::OpenDir);
        // needs_update && !can_patch: primary_action already falls back to Unpatch, so Open dir stays.
        assert_eq!(second_slot(primary_action(true, true, false, true), true), SecondSlot::OpenDir);
    }

    #[test]
    fn needs_update_covers_proxy_swap_runtime_loss_and_newer_cache() {
        assert!(needs_update(true, true, Some("0.4.0"), Some("0.4.0")), "proxy switched");
        assert!(needs_update(false, false, Some("0.4.0"), Some("0.4.0")), "0.3.x patch: no runtime tree");
        assert!(needs_update(false, true, Some("0.4.0"), Some("0.3.5")), "newer bundle cached");
        assert!(needs_update(false, true, Some("0.4.0"), None), "version never recorded -> update");
        assert!(!needs_update(false, true, Some("0.4.0"), Some("0.4.0")), "up to date");
        assert!(!needs_update(false, true, None, Some("0.4.0")), "no cache version to trust");
    }

    #[test]
    fn primary_action_promotes_repatch_but_never_traps_a_record() {
        // A 0.3.x-patched game with mod files available: one click upgrades it.
        assert_eq!(primary_action(true, true, true, true), RowAction::Repatch);
        // The same row with no usable GPU / no mod files: Unpatch must stay reachable.
        assert_eq!(primary_action(true, true, false, true), RowAction::Unpatch);
        // Up to date: unchanged behaviour.
        assert_eq!(primary_action(true, true, true, false), RowAction::Unpatch);
        // `row_action` itself is untouched by this round.
        assert_eq!(row_action(true, true, true), RowAction::Unpatch);
        assert_eq!(row_action(false, true, true), RowAction::Patch);
    }

    /// A path that does not exist, so `has_fg_dll` is false without touching disk.
    fn absent() -> PathBuf {
        PathBuf::from(r"Z:\definitely\not\here\game.exe")
    }

    /// A real render exe with NVIDIA's Frame Generation library beside it.
    fn with_fg_dll() -> PathBuf {
        let dir = std::env::temp_dir().join("dlssg-patcher-tests-fg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nvngx_dlssg.dll"), b"stub").unwrap();
        dir.join("game.exe")
    }

    /// The local DLL is independent evidence: a game NVIDIA never published can
    /// still be confirmed Supported from disk alone.
    #[test]
    fn local_dll_alone_is_supported() {
        let (label, _, tip) = support_tag(None, Some(&with_fg_dll()));
        assert_eq!(label, "Supported");
        assert_eq!(tip, "nvngx_dlssg.dll found next to the game executable.");
    }

    /// Both signals agreeing must not produce a doubled or malformed sentence:
    /// the catalog line alone carries the message.
    #[test]
    fn both_signals_read_as_one_sentence() {
        let (_, _, tip) = support_tag(Some(&entry("X", Some("exact"), "version.dll")), Some(&with_fg_dll()));
        assert_eq!(tip, "Listed in NVIDIA's DLSS Frame Generation catalog (X).");
    }

    /// The point of the rework: NVIDIA's list alone is enough to say Supported,
    /// even when the DLL is not sitting next to the exe.
    #[test]
    fn nvidia_list_alone_is_supported() {
        let (label, color, tip) = support_tag(Some(&entry("Cyberpunk 2077", Some("exact"), "version.dll")), Some(&absent()));
        assert_eq!(label, "Supported");
        assert_eq!(color, SUCCESS);
        assert_eq!(tip, "Listed in NVIDIA's DLSS Frame Generation catalog (Cyberpunk 2077).");
    }

    /// Absent from both signals must be "Unknown", never "unsupported": a
    /// renamed folder or a DLL in a sibling directory produces a false negative.
    #[test]
    fn no_evidence_is_unknown_not_unsupported() {
        let (label, _, tip) = support_tag(None, Some(&absent()));
        assert_eq!(label, "Unknown");
        assert!(!label.contains("nsupported"), "must not claim unsupported: {label}");
        assert_eq!(
            tip,
            "Not in NVIDIA's catalog and nvngx_dlssg.dll was not found next to the .exe. Patching may still work."
        );
    }

    #[test]
    fn pick_proxy_prefers_override_then_record_then_disk_then_catalog() {
        assert_eq!(pick_proxy(Some("dxgi.dll"), Some("winmm.dll"), None, None, "version.dll"), "dxgi.dll");
        assert_eq!(pick_proxy(None, Some("winmm.dll"), None, None, "version.dll"), "winmm.dll");
        assert_eq!(pick_proxy(None, None, Some("dinput8.dll"), None, "version.dll"), "dinput8.dll");
        assert_eq!(pick_proxy(None, None, None, Some("dbghelp.dll"), "version.dll"), "dbghelp.dll");
        assert_eq!(pick_proxy(None, None, None, None, "d3d12.dll"), "d3d12.dll");
        // Case-insensitive (these are Windows filenames), and the canonical spelling comes back.
        assert_eq!(pick_proxy(Some("WINMM.DLL"), None, None, None, "version.dll"), "winmm.dll");
        assert_eq!(pick_proxy(None, None, None, None, "DBGHELP.DLL"), "dbghelp.dll");
        // Junk in any slot is skipped, never passed through to install.
        assert_eq!(pick_proxy(Some("winhttp.dll"), None, None, None, "version.dll"), "version.dll");
        assert_eq!(pick_proxy(None, None, None, None, "nope.dll"), "version.dll");
    }

    #[test]
    fn all_six_bundled_proxies_are_supported() {
        for p in crate::download::PROXY_NAMES {
            let (label, color, _) = support_tag(Some(&entry("X", Some("exact"), p)), Some(&absent()));
            assert_eq!(label, "Supported", "{p} must be supported");
            assert_eq!(color, SUCCESS);
        }
    }

    /// A proxy we do not bundle outranks every FG signal: the mod cannot work
    /// regardless of what the list says.
    #[test]
    fn unbundled_proxy_wins_over_fg_evidence() {
        let (label, color, tip) = support_tag(Some(&entry("X", Some("exact"), "winhttp.dll")), Some(&absent()));
        assert_eq!(label, "Needs winhttp.dll");
        assert_eq!(color, WARN);
        assert_eq!(tip, "Requires winhttp.dll (not one of the bundled proxy DLLs).");
    }

    /// NVIDIA's list settles the question on its own, so a catalogued game must NOT
    /// be downgraded to "couldn't check" just because the exe autodetector missed
    /// its layout.
    #[test]
    fn catalog_match_without_exe_is_still_supported() {
        let (label, color, tip) = support_tag(Some(&entry("Starfield", Some("exact"), "version.dll")), None);
        assert_eq!(label, "Supported");
        assert_eq!(color, SUCCESS);
        assert_eq!(tip, "Listed in NVIDIA's DLSS Frame Generation catalog (Starfield).");
    }

    /// With no catalog entry and no exe there is genuinely nothing to go on.
    #[test]
    fn no_exe_and_no_catalog_reports_unchecked() {
        let (label, color, tip) = support_tag(None, None);
        assert_eq!(label, "\u{2014}");
        assert_eq!(color, MUTED);
        assert_eq!(tip, "Executable not located yet. Click Locate to select the game's .exe.");
    }

    /// The DLL is a secondary signal only; a missing exe must not hide the list.
    #[test]
    fn catalog_match_with_exe_but_no_dll_is_supported() {
        let (label, _, tip) =
            support_tag(Some(&entry("Stellar Blade", Some("exact"), "version.dll")), Some(&absent()));
        assert_eq!(label, "Supported");
        assert_eq!(tip, "Listed in NVIDIA's DLSS Frame Generation catalog (Stellar Blade).");
    }

    /// A fuzzy AppID match is still evidence, but the tooltip must not present it
    /// as certainty.
    #[test]
    fn fuzzy_catalog_match_is_qualified_in_tooltip() {
        let (_, _, tip) = support_tag(Some(&entry("Serum", Some("fuzzy"), "version.dll")), Some(&absent()));
        assert!(tip.contains("fuzzy"), "tooltip should flag the weak match: {tip}");
    }

    /// An exact catalog match should not be hedged.
    #[test]
    fn exact_catalog_match_is_not_hedged() {
        let (_, _, tip) = support_tag(Some(&entry("A", Some("exact"), "version.dll")), Some(&absent()));
        assert!(!tip.contains("fuzzy"), "should not hedge an exact match: {tip}");
    }

    /// The amber fallback note names only the integrated GPU the display is on.
    #[test]
    fn hybrid_note_names_only_the_current_igpu() {
        let note = hybrid_note("AMD Radeon(TM) Graphics");
        assert_eq!(note, "Running on AMD Radeon Graphics (iGPU)");
    }

    /// The no-iGPU fallback note stays to one short line and never uses an em
    /// dash (the bundled font renders it as a broken glyph).
    #[test]
    fn wmi_only_note_is_a_single_short_line() {
        let note = wmi_only_note();
        assert_eq!(note, "Detected via Windows - nvidia-smi could not query it");
        assert!(!note.contains('\u{2014}'), "must not use an em dash: {note}");
    }
}
