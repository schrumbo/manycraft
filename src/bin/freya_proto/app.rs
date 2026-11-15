use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use freya::engine::prelude::{AlphaType, ColorType, IPoint, ISize, ImageInfo};
use freya::elements::image::ImageHandle;
use freya::prelude::*;
use freya::winit::event::WindowEvent;
use freya_winit::plugins::{FreyaPlugin, PluginEvent, PluginHandle};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;

use super::preview::{Preview, PreviewChanges, read_preview, read_preview_from};
use super::swap;

const GAP: f32 = 12.0;
const PAD: f32 = 16.0;
const STRIP: f32 = 28.0;
const PREVIEW_POLL: Duration = Duration::from_micros(16_667);

static FEEDING: AtomicBool = AtomicBool::new(true);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static PREVIEWS: AtomicU64 = AtomicU64::new(0);
static RENDERS: AtomicU64 = AtomicU64::new(0);
static READS: AtomicU64 = AtomicU64::new(0);
static READ_US: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static SAME: AtomicU64 = AtomicU64::new(0);
static PRESSES: AtomicU64 = AtomicU64::new(0);
static FRAME_TIMES: Mutex<Vec<f32>> = Mutex::new(Vec::new());
static PHASE_US: [AtomicU64; PHASES.len()] = [const { AtomicU64::new(0) }; PHASES.len()];
const PHASES: [&str; 9] = ["tree", "tasks", "layout", "events", "accessibility", "render", "present", "collect", "redraw"];
static OS_SCALE: Mutex<f64> = Mutex::new(1.0);

type Batch = Vec<(usize, Preview)>;

thread_local! {
    static FEEDS: RefCell<HashMap<usize, async_channel::Receiver<Preview>>> = RefCell::new(HashMap::new());
    static BATCH: RefCell<Option<async_channel::Receiver<Batch>>> = const { RefCell::new(None) };
    static TILE_FRAMES: RefCell<HashMap<usize, State<Option<ImageHandle>>>> = RefCell::new(HashMap::new());
    static PAINT_START: RefCell<Option<Instant>> = const { RefCell::new(None) };
    static PHASE_START: RefCell<[Option<Instant>; PHASES.len()]> = const { RefCell::new([None; PHASES.len()]) };
}

/// Command line of the prototype.
#[derive(Clone, Debug)]
pub struct Options {
    pub tiles: usize,
    pub stand_ins: usize,
    pub tile_width: f32,
    pub size: (f64, f64),
    pub pos: Option<(i32, i32)>,
    pub device_scale: Option<f64>,
    pub stats: Option<PathBuf>,
    pub control: Option<u16>,
    pub dir: PathBuf,
    pub games: Vec<(usize, u32)>,
    pub previews: Vec<(usize, PathBuf)>,
    pub asynchronous: bool,
    pub per_tile: bool,
}

impl Options {
    /// Reads `--name value` pairs; unknown names are ignored.
    pub fn parse(args: &[String]) -> Self {
        let mut o = Options {
            tiles: 16,
            stand_ins: 4,
            tile_width: 320.0,
            size: (1440.0, 900.0),
            pos: None,
            device_scale: None,
            stats: None,
            control: None,
            dir: std::env::temp_dir().join("manycraft-freya-proto"),
            games: Vec::new(),
            previews: Vec::new(),
            asynchronous: false,
            per_tile: false,
        };
        let pair = |v: &str, sep: char| v.split_once(sep).map(|(a, b)| (a.trim().to_owned(), b.trim().to_owned()));
        let mut it = args.iter();
        while let Some(name) = it.next() {
            if name == "--async" {
                o.asynchronous = true;
                continue;
            }
            if name == "--per-tile-feeds" {
                o.per_tile = true;
                continue;
            }
            let Some(value) = it.next() else { break };
            match name.as_str() {
                "--tiles" => o.tiles = value.parse().unwrap_or(o.tiles),
                "--stand-ins" => o.stand_ins = value.parse().unwrap_or(o.stand_ins),
                "--tile-width" => o.tile_width = value.parse().unwrap_or(o.tile_width),
                "--size" => {
                    if let Some((w, h)) = pair(value, 'x') {
                        o.size = (w.parse().unwrap_or(o.size.0), h.parse().unwrap_or(o.size.1));
                    }
                }
                "--pos" => o.pos = pair(value, ',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))),
                "--device-scale" => o.device_scale = value.parse().ok(),
                "--stats" => o.stats = Some(PathBuf::from(value)),
                "--control" => o.control = value.parse().ok(),
                "--dir" => o.dir = PathBuf::from(value),
                "--game" => {
                    if let Some((t, p)) = pair(value, ':')
                        && let (Ok(t), Ok(p)) = (t.parse(), p.parse())
                    {
                        o.games.push((t, p));
                    }
                }
                "--preview" => {
                    if let Some((t, p)) = pair(value, ':')
                        && let Ok(t) = t.parse()
                    {
                        o.previews.push((t, PathBuf::from(p)));
                    }
                }
                _ => {}
            }
        }
        o
    }
}

struct Meter;

impl FreyaPlugin for Meter {
    fn plugin_id(&self) -> &'static str {
        "manycraft-meter"
    }

    fn on_event(&mut self, event: &mut PluginEvent, _: PluginHandle) {
        let phases: &[(usize, bool)] = match event {
            PluginEvent::StartedUpdatingTree { .. } => &[(0, true)],
            PluginEvent::FinishedUpdatingTree { .. } => &[(0, false)],
            PluginEvent::StartedPollingTasks { .. } => &[(1, true)],
            PluginEvent::FinishedPollingTasks { .. } => &[(1, false)],
            PluginEvent::StartedMeasuringLayout { .. } => &[(2, true), (8, true)],
            PluginEvent::FinishedMeasuringLayout { .. } => &[(2, false), (7, true)],
            PluginEvent::StartedMeasuringEvents { .. } => &[(3, true)],
            PluginEvent::FinishedMeasuringEvents { .. } => &[(3, false)],
            PluginEvent::BeforeAccessibility { .. } => &[(4, true)],
            PluginEvent::AfterAccessibility { .. } => &[(4, false)],
            PluginEvent::BeforeRender { .. } => &[(5, true), (7, false)],
            PluginEvent::AfterRender { .. } => &[(5, false)],
            PluginEvent::BeforePresenting { .. } => &[(6, true)],
            PluginEvent::AfterPresenting { .. } => &[(6, false), (8, false)],
            _ => &[],
        };
        for &(index, start) in phases {
            PHASE_START.with(|s| {
                let mut s = s.borrow_mut();
                if start {
                    s[index] = Some(Instant::now());
                } else if let Some(t) = s[index].take() {
                    PHASE_US[index].fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
            });
        }
        match event {
            PluginEvent::BeforeRender { .. } => PAINT_START.with(|s| *s.borrow_mut() = Some(Instant::now())),
            PluginEvent::WindowEvent { window, event: WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(_) } => {
                swap::follow_window(window.is_minimized().unwrap_or(false));
            }
            PluginEvent::BeforePresenting { .. } => {
                FRAMES.fetch_add(1, Ordering::Relaxed);
                if let Some(start) = PAINT_START.with(|s| s.borrow_mut().take()) {
                    let mut times = FRAME_TIMES.lock().unwrap_or_else(|e| e.into_inner());
                    times.push(start.elapsed().as_secs_f32() * 1000.0);
                    if times.len() > 4096 {
                        times.drain(..2048);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Starts the stand-in games, the preview reader, the stats writer and the Freya window.
pub fn run(options: Options) {
    let _ = std::fs::create_dir_all(&options.dir);
    let mut children = Vec::new();
    let mut games: HashMap<usize, u32> = options.games.iter().copied().collect();
    let mut feeds: Vec<(usize, PathBuf)> = options.previews.clone();
    let exe = std::env::current_exe().unwrap_or_default();
    for index in 0..options.stand_ins.min(options.tiles) {
        let path = options.dir.join(format!("preview-{index}.bin"));
        let child = std::process::Command::new(&exe).args(["stand-in", &index.to_string(), &path.to_string_lossy(), &std::process::id().to_string()]).spawn();
        if let Ok(child) = child {
            games.insert(index, child.id());
            feeds.push((index, path));
            children.push(child);
        }
    }
    swap::configure(games, options.asynchronous);
    let mut senders = HashMap::new();
    let mut batch = None;
    if options.per_tile {
        FEEDS.with(|f| {
            let mut f = f.borrow_mut();
            for (index, _) in &feeds {
                let (tx, rx) = async_channel::bounded(1);
                senders.insert(*index, tx);
                f.insert(*index, rx);
            }
        });
    } else {
        let (tx, rx) = async_channel::unbounded();
        BATCH.with(|b| *b.borrow_mut() = Some(rx));
        batch = Some(tx);
    }
    std::thread::spawn(move || read_previews(feeds, senders, batch));
    if let Some(path) = options.stats.clone() {
        std::thread::spawn(move || write_stats(&path));
    }
    let (commands_tx, commands_rx) = async_channel::unbounded::<String>();
    if let Some(port) = options.control {
        std::thread::spawn(move || listen(port, commands_tx));
    }
    let size = options.size;
    let device = options.device_scale;
    let pos = options.pos;
    let window = WindowConfig::new_app(Manager { options: options.clone(), commands: commands_rx })
        .with_size(size.0, size.1)
        .with_title("Manycraft Freya prototype")
        .with_background((24, 24, 26))
        .with_window_attributes(move |attributes, _| {
            let attributes = attributes.with_visible(true);
            match pos {
                Some((x, y)) => attributes.with_position(freya::winit::dpi::PhysicalPosition::new(x, y)),
                None => attributes,
            }
        })
        .with_window_handle(move |window| {
            let os = window.scale_factor();
            *OS_SCALE.lock().unwrap_or_else(|e| e.into_inner()) = os;
            if let Some(device) = device {
                let _ = window.request_inner_size(freya::winit::dpi::PhysicalSize::new((size.0 * device).round() as u32, (size.1 * device).round() as u32));
            }
            if let Ok(RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
                swap::attach(h.hwnd.get() as HWND);
            }
        });
    let crash_log = options.dir.join("crash.log");
    let children = Rc::new(RefCell::new(children));
    let crashed_children = children.clone();
    launch(LaunchConfig::new().with_plugin(Meter).with_window(window).with_fatal_error_handler(move |message| {
        eprintln!("freya_proto crashed: {message}");
        if let Err(error) = std::fs::write(&crash_log, &message) {
            eprintln!("freya_proto could not write {}: {error}", crash_log.display());
        }
        swap::hide();
        for child in crashed_children.borrow_mut().iter_mut() {
            let _ = child.kill();
        }
    }));
    swap::hide();
    for mut child in children.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn read_previews(feeds: Vec<(usize, PathBuf)>, senders: HashMap<usize, async_channel::Sender<Preview>>, batch: Option<async_channel::Sender<Batch>>) {
    let mut changes = PreviewChanges::default();
    let mut files: HashMap<usize, std::fs::File> = HashMap::new();
    let reopen = std::env::var_os("PROTO_REOPEN").is_some();
    let mut next = Instant::now();
    loop {
        let mut fresh = Vec::new();
        for (index, path) in feeds.iter().filter(|_| FEEDING.load(Ordering::Relaxed)) {
            READS.fetch_add(1, Ordering::Relaxed);
            let started = Instant::now();
            let read = if reopen {
                read_preview(path)
            } else {
                if !files.contains_key(index)
                    && let Ok(file) = std::fs::File::open(path)
                {
                    files.insert(*index, file);
                }
                files.get_mut(index).and_then(read_preview_from)
            };
            READ_US.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            let Some(preview) = read else {
                files.remove(index);
                MISSES.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            if !changes.changed(&index.to_string(), &preview) {
                SAME.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            match senders.get(index) {
                Some(tx) => {
                    let _ = tx.force_send(preview);
                }
                None => fresh.push((*index, preview)),
            }
        }
        if let Some(tx) = &batch
            && !fresh.is_empty()
        {
            let _ = tx.send_blocking(fresh);
        }
        next += PREVIEW_POLL;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f32 * p).round() as usize]
}

fn write_stats(path: &PathBuf) {
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let mut times = FRAME_TIMES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        times.sort_by(f32::total_cmp);
        let active = swap::active();
        let tile = active.and_then(swap::tile_on_screen);
        let game = swap::active_window().and_then(swap::client_on_screen);
        let manager = swap::client_on_screen(swap::manager());
        let rect = |r: Option<swap::Px>| r.map_or("null".to_owned(), |r| format!("[{},{},{},{}]", r.x, r.y, r.w, r.h));
        let tiles: Vec<String> = (0..256).filter_map(|i| swap::tile_on_screen(i).map(|r| format!("[{},{},{},{}]", r.x, r.y, r.w, r.h))).collect();
        let phases: Vec<String> = PHASES.iter().zip(&PHASE_US).map(|(name, us)| format!("\"{name}\":{}", us.load(Ordering::Relaxed))).collect();
        let text = format!(
            "{{\"phases_us\":{{{}}},\"read_us\":{},\"reads\":{},\"misses\":{},\"same\":{},\"presses\":{},\"frames\":{},\"renders\":{},\"previews\":{},\"placed\":{},\"p50\":{:.3},\"p95\":{:.3},\"max\":{:.3},\"samples\":{},\"active\":{},\"visible\":{},\"tile\":{},\"game\":{},\"manager\":{},\"tiles\":[{}]}}",
            phases.join(","),
            READ_US.load(Ordering::Relaxed),
            READS.load(Ordering::Relaxed),
            MISSES.load(Ordering::Relaxed),
            SAME.load(Ordering::Relaxed),
            PRESSES.load(Ordering::Relaxed),
            FRAMES.load(Ordering::Relaxed),
            RENDERS.load(Ordering::Relaxed),
            PREVIEWS.load(Ordering::Relaxed),
            swap::PLACED.load(Ordering::Relaxed),
            percentile(&times, 0.5),
            percentile(&times, 0.95),
            times.last().copied().unwrap_or(0.0),
            times.len(),
            active.map_or("null".to_owned(), |a| a.to_string()),
            swap::game_visible(),
            rect(tile),
            rect(game),
            rect(manager),
            tiles.join(","),
        );
        if text != last {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &text).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
            last = text;
        }
    }
}

fn listen(port: u16, commands: async_channel::Sender<String>) {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else { return };
    for stream in listener.incoming().flatten() {
        let Ok(mut out) = stream.try_clone() else { continue };
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let line = line.trim().to_owned();
            if line == "reset" {
                FRAME_TIMES.lock().unwrap_or_else(|e| e.into_inner()).clear();
            }
            let _ = commands.send_blocking(line);
            let _ = out.write_all(b"ok\n");
        }
    }
}

fn snapshot(path: PathBuf) {
    let window = Platform::get().current_window();
    let shown = swap::active().and_then(|i| Some((swap::tile_on_screen(i)?, swap::active_window()?)));
    let manager = swap::client_on_screen(swap::manager());
    let receiver = window.post_render_callback(move |surface| {
        let (w, h) = (surface.width(), surface.height());
        let info = ImageInfo::new(ISize::new(w, h), ColorType::RGBA8888, AlphaType::Unpremul, None);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let ok = surface.read_pixels(&info, &mut pixels, (w * 4) as usize, IPoint::new(0, 0));
        (ok, w as u32, h as u32, pixels)
    });
    spawn(async move {
        let Ok((true, w, h, mut pixels)) = receiver.await else { return };
        if let (Some((tile, game)), Some(manager)) = (shown, manager)
            && let Some((gw, gh, game_pixels)) = capture(game)
        {
            let (ox, oy) = (tile.x - manager.x, tile.y - manager.y);
            for y in 0..gh.min(tile.h as u32) {
                for x in 0..gw.min(tile.w as u32) {
                    let (dx, dy) = (ox + x as i32, oy + y as i32);
                    if dx < 0 || dy < 0 || dx as u32 >= w || dy as u32 >= h {
                        continue;
                    }
                    let s = ((y * gw + x) * 4) as usize;
                    let d = ((dy as u32 * w + dx as u32) * 4) as usize;
                    pixels[d..d + 4].copy_from_slice(&game_pixels[s..s + 4]);
                }
            }
        }
        if let Some(img) = image::RgbaImage::from_raw(w, h, pixels) {
            let _ = img.save_with_format(&path, image::ImageFormat::Png);
        }
    });
}

fn capture(hwnd: HWND) -> Option<(u32, u32, Vec<u8>)> {
    use windows_sys::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject};
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    let client = swap::client_on_screen(hwnd)?;
    let (w, h) = (client.w.max(1), client.h.max(1));
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let old = SelectObject(dc, bitmap);
        let ok = PrintWindow(hwnd, dc, 1 | 2) != 0;
        let mut out = Vec::new();
        if ok && !bits.is_null() {
            let raw = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
            out = raw.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        }
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen);
        (ok && !out.is_empty()).then_some((w as u32, h as u32, out))
    }
}

struct Manager {
    options: Options,
    commands: async_channel::Receiver<String>,
}

impl App for Manager {
    fn render(&self) -> impl IntoElement {
        let mut size = use_state(|| (0.0f32, 0.0f32));
        let mut playing = use_provide_context(|| State::create(None::<usize>));
        let commands = self.commands.clone();
        let device = self.options.device_scale;
        use_hook(move || {
            if let Some(rx) = BATCH.with(|b| b.borrow().clone()) {
                spawn(async move {
                    while let Ok(first) = rx.recv().await {
                        let mut newest: HashMap<usize, Preview> = first.into_iter().collect();
                        while let Ok(more) = rx.try_recv() {
                            newest.extend(more);
                        }
                        for (index, p) in newest {
                            let state = TILE_FRAMES.with(|t| t.borrow().get(&index).copied());
                            if let (Some(mut state), Some(handle)) = (state, ImageHandle::from_rgba(p.width, p.height, Bytes::from(p.rgba), AlphaType::Unpremul)) {
                                state.set(Some(handle));
                                PREVIEWS.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            let window = Platform::get().current_window();
            if let Some(device) = device {
                let os = *OS_SCALE.lock().unwrap_or_else(|e| e.into_inner());
                window.set_custom_scale_factor(device / os);
                swap::set_custom_scale(device / os);
            }
            spawn(async move {
                while let Ok(command) = commands.recv().await {
                    let mut words = command.split_whitespace();
                    match (words.next(), words.next()) {
                        (Some("shot"), Some(path)) => snapshot(PathBuf::from(path)),
                        (Some("zoom"), Some(value)) => {
                            if let Ok(zoom) = value.parse::<f64>() {
                                Platform::get().current_window().set_custom_scale_factor(zoom);
                                swap::set_custom_scale(zoom);
                            }
                        }
                        (Some("play"), Some(index)) => {
                            if let Ok(index) = index.parse::<usize>()
                                && swap::play(index)
                            {
                                playing.set(Some(index));
                            }
                        }
                        (Some("hide"), _) => {
                            swap::hide();
                            playing.set(None);
                        }
                        (Some("previews"), Some(state)) => FEEDING.store(state == "on", Ordering::Relaxed),
                        (Some("redraw"), _) => Platform::get().current_window().request_redraw(),
                        (Some("quit"), _) => Platform::get().exit(),
                        (Some("panic"), _) => panic!("panic requested over the control port"),
                        _ => {}
                    }
                }
            });
        });
        RENDERS.fetch_add(1, Ordering::Relaxed);
        let (width, _) = *size.read();
        let tw = self.options.tile_width;
        let columns = (((width - PAD * 2.0 + GAP) / (tw + GAP)).floor() as usize).max(1);
        let th = tw * 9.0 / 16.0 + STRIP;
        let active = *playing.read();
        let rows: Vec<Vec<usize>> = (0..self.options.tiles).collect::<Vec<_>>().chunks(columns).map(<[usize]>::to_vec).collect();
        rect()
            .width(Size::fill())
            .height(Size::fill())
            .padding(PAD)
            .spacing(GAP)
            .on_sized(move |e: Event<SizedEventData>| size.set((e.area.width(), e.area.height())))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if e.key == Key::Named(NamedKey::Escape) {
                    swap::hide();
                    playing.set(None);
                }
            })
            .children(rows.into_iter().enumerate().map(move |(r, row)| {
                rect()
                    .key(r)
                    .horizontal()
                    .spacing(GAP)
                    .children(row.into_iter().map(move |index| Tile { index, width: tw, height: th, playing: active == Some(index), key: DiffKey::None }.key(index).into_element()))
                    .into_element()
            }))
    }
}

#[derive(PartialEq)]
struct Tile {
    index: usize,
    width: f32,
    height: f32,
    playing: bool,
    key: DiffKey,
}

impl KeyExt for Tile {
    fn key_mut(&mut self) -> &mut DiffKey {
        &mut self.key
    }
}

impl Component for Tile {
    fn render(&self) -> impl IntoElement {
        let index = self.index;
        let mut frame = use_state(|| None::<ImageHandle>);
        use_hook(move || {
            TILE_FRAMES.with(|t| t.borrow_mut().insert(index, frame));
            if let Some(rx) = FEEDS.with(|f| f.borrow().get(&index).cloned()) {
                spawn(async move {
                    while let Ok(p) = rx.recv().await {
                        if let Some(handle) = ImageHandle::from_rgba(p.width, p.height, Bytes::from(p.rgba), AlphaType::Unpremul) {
                            frame.set(Some(handle));
                            PREVIEWS.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        use_drop(move || {
            TILE_FRAMES.with(|t| t.borrow_mut().remove(&index));
        });
        let preview_h = self.height - STRIP;
        let body = match frame.read().clone() {
            Some(handle) => image(handle).width(Size::fill()).height(Size::px(preview_h)).aspect_ratio(AspectRatio::None).into_element(),
            None => rect().width(Size::fill()).height(Size::px(preview_h)).background((40, 40, 44)).into_element(),
        };
        let mut playing = use_consume::<State<Option<usize>>>();
        let edge = if self.playing { (64, 160, 150) } else { (52, 52, 56) };
        rect()
            .width(Size::px(self.width))
            .height(Size::px(self.height))
            .background((34, 34, 37))
            .corner_radius(8.0)
            .border(Border::new().fill(edge).width(1.0).alignment(BorderAlignment::Inner))
            .overflow(Overflow::Clip)
            .on_sized(move |e: Event<SizedEventData>| {
                let a = e.area;
                swap::set_tile(index, swap::Area { x: a.min_x(), y: a.min_y(), w: a.width(), h: a.height() - STRIP });
            })
            .on_press(move |_| {
                PRESSES.fetch_add(1, Ordering::Relaxed);
                if swap::play(index) {
                    playing.set(Some(index));
                }
            })
            .child(body)
            .child(rect().width(Size::fill()).height(Size::px(STRIP)).padding((0.0, 10.0)).center().child(label().text(format!("{}", index + 1)).color((200, 200, 205)).font_size(13.0)))
    }

    fn render_key(&self) -> DiffKey {
        self.key.clone()
    }
}
