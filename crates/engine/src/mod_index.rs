//! The mod index (Weft-Mods-Index, where each mod is built from its source) browsed from the
//! game: what the launcher does on a computer, and all a phone has. A build fits this game when
//! its `abi` is this engine's (`modloader::ABI_VERSION`); installing puts its zip, checked against
//! the SHA-256 the index wrote down, into the player's `mods` with what it needs. A build marked
//! verified had its source read by a maintainer; the others show only when asked for.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::app::{Ctx, Request};
use crate::screens::{BuildCx, Screen, button, frame, label};
use crate::ui::text::Arg;
use crate::ui::{Align, Kind, Style, Ui};

const INDEX_URL: &str = "https://raw.githubusercontent.com/Weft-Rust/Weft-Mods-Index/gh-pages/index.json";

#[derive(Clone, Debug)]
pub struct IndexMod {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub source: String,
    /// Newest first.
    pub builds: Vec<Build>,
}

#[derive(Clone, Debug)]
pub struct Build {
    pub version: String,
    pub abi: String,
    pub url: String,
    pub sha256: String,
    pub deps: Vec<String>,
    pub verified: bool,
}

impl IndexMod {
    /// The newest build that loads in this game.
    pub fn fitting(&self) -> Option<&Build> {
        let abi = format!("{:016x}", crate::modloader::ABI_VERSION);
        self.builds.iter().find(|b| b.abi == abi)
    }
}

fn parse(bytes: &[u8]) -> Option<Vec<IndexMod>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let mods = v["mods"].as_array()?;
    Some(
        mods.iter()
            .map(|m| IndexMod {
                id: text(m, "id"),
                name: text(m, "name"),
                summary: text(m, "summary"),
                author: text(m, "author"),
                source: text(m, "source"),
                builds: m["builds"]
                    .as_array()
                    .map(|bs| {
                        bs.iter()
                            .map(|b| Build {
                                version: text(b, "version"),
                                abi: text(b, "abi"),
                                url: text(b, "url"),
                                sha256: text(b, "sha256"),
                                deps: b["deps"]
                                    .as_array()
                                    .map(|d| {
                                        d.iter()
                                            .filter_map(|d| d.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                                verified: b["verified"].as_bool().unwrap_or(false),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .filter(|m| valid_id(&m.id))
            .collect(),
    )
}

/// Mod ids name their zip and library: `[a-z0-9_]`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// What the background work has come to.
#[derive(Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Loading,
    /// Downloading this mod.
    Busy(String),
    Failed(String),
}

#[derive(Default)]
struct State {
    index: Option<Vec<IndexMod>>,
    status: Status,
    /// Counts changes, for the screen to build itself again.
    generation: u64,
}

static STATE: parking_lot::Mutex<State> = parking_lot::Mutex::new(State { index: None, status: Status::Idle, generation: 0 });
/// Mods were installed or removed: they load on the next start.
static CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn update(f: impl FnOnce(&mut State)) {
    let mut s = STATE.lock();
    f(&mut s);
    s.generation += 1;
}

pub fn generation() -> u64 {
    STATE.lock().generation
}

pub fn status() -> Status {
    STATE.lock().status.clone()
}

pub fn index() -> Option<Vec<IndexMod>> {
    STATE.lock().index.clone()
}

/// Mods were installed or removed since the start.
pub fn changed() -> bool {
    CHANGED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The certificates come with the game: a phone has none Rust can read without Java.
fn client() -> Result<reqwest::blocking::Client, String> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::blocking::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("weft/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let r = client()?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    r.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Fetches the index in the background; offline, the last copy that came (`cache`).
pub fn refresh(cache: &Path) {
    if matches!(status(), Status::Loading | Status::Busy(_)) {
        return;
    }
    update(|s| s.status = Status::Loading);
    let file = cache.join("mod-index.json");
    std::thread::spawn(move || {
        let fetched = get(INDEX_URL).and_then(|b| parse(&b).map(|i| (i, b)).ok_or("not an index".into()));
        match fetched {
            Ok((index, bytes)) => {
                let _ = file.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(&file, bytes);
                update(|s| (s.index, s.status) = (Some(index), Status::Idle));
            }
            Err(e) => {
                log::warn!("mod index: {e}");
                let cached = std::fs::read(&file).ok().and_then(|b| parse(&b));
                update(|s| {
                    s.status = if cached.is_some() { Status::Idle } else { Status::Failed(e) };
                    s.index = cached.or(s.index.take());
                });
            }
        }
    });
}

fn sha256_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The mods in `dir` as zips, with each one's SHA-256.
pub fn installed(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
        .filter_map(|p| {
            let id = p.file_stem()?.to_string_lossy().into_owned();
            Some((id, sha256_of(&std::fs::read(&p).ok()?)))
        })
        .collect()
}

/// Downloads the build of `id` that fits this game, and what it needs that isn't there (what
/// comes with the game isn't in the index), into `dir`.
pub fn install(id: String, dir: PathBuf) {
    let Some(index) = index() else { return };
    update(|s| s.status = Status::Busy(id.clone()));
    std::thread::spawn(move || {
        let have: Vec<String> = installed(&dir).into_iter().map(|m| m.0).collect();
        let mut todo = vec![id.clone()];
        let mut done: Vec<String> = Vec::new();
        let result = (|| {
            while let Some(want) = todo.pop() {
                // The one asked for is installed again (an update), what it needs only if missing.
                if done.contains(&want) || (want != id && have.contains(&want)) {
                    continue;
                }
                let Some(m) = index.iter().find(|m| m.id == want) else { continue };
                let b = m
                    .fitting()
                    .ok_or(format!("{} has no build for this game", m.name))?;
                let bytes = get(&b.url)?;
                if !sha256_of(&bytes).eq_ignore_ascii_case(&b.sha256) {
                    return Err(format!("{} came different from what the index built", m.name));
                }
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let to = dir.join(format!("{}.zip", m.id));
                let part = to.with_extension("part");
                std::fs::write(&part, &bytes)
                    .and_then(|_| std::fs::rename(&part, &to))
                    .map_err(|e| e.to_string())?;
                todo.extend(b.deps.iter().cloned());
                done.push(want);
            }
            Ok::<_, String>(())
        })();
        if !done.is_empty() {
            CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        update(|s| {
            s.status = match result {
                Ok(()) => Status::Idle,
                Err(e) => Status::Failed(e),
            }
        });
    });
}

pub fn remove(id: &str, dir: &Path) {
    if valid_id(id) && std::fs::remove_file(dir.join(format!("{id}.zip"))).is_ok() {
        CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
        update(|_| {});
    }
}

/// The catalog: the index's mods, the one picked with what it is and what can be done with it.
#[derive(Default)]
pub struct Catalog {
    selected: Rc<std::cell::Cell<usize>>,
    unverified: Rc<std::cell::Cell<bool>>,
    seen: u64,
}

impl Screen for Catalog {
    fn name(&self) -> &str {
        "core:mod_catalog"
    }

    /// Built again when the background work moves on.
    fn pointer(&mut self, _cursor: glam::Vec2, _ui: &mut Ui, ctx: &mut Ctx, _hud: &mut crate::hud::Hud) -> bool {
        if generation() != self.seen {
            ctx.requests.push(Request::Rebuild);
        }
        false
    }

    fn build(&mut self, ui: &mut Ui, cx: &BuildCx) {
        self.seen = generation();
        let dir = cx.settings.run_dir.join("mods");
        if index().is_none() && matches!(status(), Status::Idle) {
            refresh(&cx.settings.run_dir.join("cache"));
        }
        let have = installed(&dir);
        let show_all = self.unverified.get();
        let mods: Vec<IndexMod> = index()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| show_all || m.builds.iter().any(|b| b.verified))
            .collect();
        let sel = self.selected.get().min(mods.len().saturating_sub(1));
        let tall = (cx.size.y - 120.0).clamp(80.0, 300.0);
        let status = status();
        frame(ui, cx, "catalog.title", |ui| {
            ui.container(Style::row().gap(6.0), |ui| {
                ui.panel(
                    "well",
                    Style::default()
                        .pad(4.0)
                        .gap(2.0)
                        .w(184.0)
                        .h(tall)
                        .scroll()
                        .stretch(),
                    |ui| {
                        if mods.is_empty() {
                            let key = match &status {
                                Status::Loading => "catalog.loading",
                                Status::Failed(_) => "catalog.offline",
                                _ => "catalog.empty",
                            };
                            label(ui, cx, key, &[], "dim");
                        }
                        for (i, m) in mods.iter().enumerate() {
                            let s = self.selected.clone();
                            let b = ui.button(
                                "button",
                                Style::row().min_h(22.0).pad_xy(4.0, 2.0).w(172.0),
                                Rc::new(move |c: &mut Ctx| {
                                    s.set(i);
                                    c.requests.push(Request::Rebuild);
                                }),
                                |ui| {
                                    ui.text(crate::ui::text::plain(&m.name), "button");
                                },
                            );
                            if let Kind::Button { selected, .. } = &mut ui.nodes[b].kind {
                                *selected = i == sel;
                            }
                        }
                    },
                );
                ui.panel(
                    "panel",
                    Style::default()
                        .pad(8.0)
                        .gap(4.0)
                        .w(300.0)
                        .h(tall)
                        .scroll()
                        .stretch(),
                    |ui| {
                        let Some(m) = mods.get(sel) else { return };
                        ui.text(crate::ui::text::plain(&m.name), "default");
                        label(ui, cx, "catalog.by", &[Arg::Text(m.author.clone())], "dim");
                        let mine = have.iter().find(|h| h.0 == m.id);
                        let fit = m.fitting();
                        match (fit, mine) {
                            (_, Some((_, sha))) => {
                                let at = m.builds.iter().find(|b| &b.sha256 == sha);
                                let version = at.map_or_else(|| "?".to_string(), |b| b.version.clone());
                                label(ui, cx, "catalog.installed", &[Arg::Text(version)], "dim");
                            }
                            (Some(b), None) => {
                                label(ui, cx, "catalog.version", &[Arg::Text(b.version.clone())], "dim");
                            }
                            (None, None) => {
                                label(ui, cx, "catalog.no_build", &[], "dim");
                            }
                        }
                        if !fit.is_some_and(|b| b.verified) {
                            label(ui, cx, "catalog.unverified", &[], "dim");
                        }
                        if !m.source.is_empty() {
                            ui.text(crate::ui::text::plain(&m.source), "dim");
                        }
                        ui.add(Kind::Container { class: None }, Style::default().h(4.0));
                        ui.text(crate::ui::text::plain(&m.summary), "default");
                    },
                );
            });
            let picked = mods.get(sel).cloned();
            ui.container(Style::row().gap(6.0), |ui| {
                if let Some(m) = picked {
                    let mine = have.iter().find(|h| h.0 == m.id).map(|h| h.1.clone());
                    let newest = m.fitting().map(|b| b.sha256.clone());
                    let busy = matches!(status, Status::Busy(_) | Status::Loading);
                    let (key, offered) = match (&mine, &newest) {
                        (None, Some(_)) => ("catalog.install", true),
                        (Some(a), Some(b)) if a != b => ("catalog.update", true),
                        _ => ("catalog.install", false),
                    };
                    let (id, to) = (m.id.clone(), dir.clone());
                    let b = button(ui, cx, key, &[], 110.0, move |c| {
                        install(id.clone(), to.clone());
                        c.requests.push(Request::Rebuild);
                    });
                    if let Kind::Button { enabled, .. } = &mut ui.nodes[b].kind {
                        *enabled = offered && !busy;
                    }
                    let (id, to) = (m.id.clone(), dir.clone());
                    let b = button(ui, cx, "catalog.remove", &[], 110.0, move |c| {
                        remove(&id, &to);
                        c.requests.push(Request::Rebuild);
                    });
                    if let Kind::Button { enabled, .. } = &mut ui.nodes[b].kind {
                        *enabled = mine.is_some() && !busy;
                    }
                }
                let all = self.unverified.clone();
                let key = if show_all { "catalog.unverified_shown" } else { "catalog.unverified_hidden" };
                button(ui, cx, key, &[], 140.0, move |c| {
                    all.set(!all.get());
                    c.requests.push(Request::Rebuild);
                });
                let cache = cx.settings.run_dir.join("cache");
                button(ui, cx, "catalog.refresh", &[], 90.0, move |c| {
                    refresh(&cache);
                    c.requests.push(Request::Rebuild);
                });
                button(ui, cx, "menu.done", &[], 90.0, |c| c.requests.push(Request::Close));
            });
            ui.container(Style::row().gap(6.0).align(Align::Center), |ui| match &status {
                Status::Busy(id) => {
                    label(ui, cx, "catalog.downloading", &[Arg::Text(id.clone())], "dim");
                }
                Status::Failed(e) => {
                    label(ui, cx, "catalog.failed", &[Arg::Text(e.clone())], "dim");
                }
                _ if changed() => {
                    label(ui, cx, "mods.restart_note", &[], "dim");
                }
                _ => {}
            });
        });
    }
}
