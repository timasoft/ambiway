use clap::Parser;
use directories::ProjectDirs;
use opencv::{
    core,
    prelude::*,
    videoio::{self, VideoCapture},
};
use openrgb2::{OpenRgbClient, Zone};
use rgb::RGB8;
use serde::Deserialize;
use std::{
    fmt::Display,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time,
};
use tokio::{
    io::AsyncWriteExt,
    runtime::Builder,
    select,
    signal::unix::{SignalKind, signal},
    sync::Mutex,
};
use tokio_serial::SerialStream;
use xrandr::XHandle;

const SHUTDOWN_BLACK_REPEATS: u32 = 5;

/// Ambilight with OpenRGB
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Set config file
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Start in paused state
    #[arg(short = 'p', long = "paused")]
    paused: bool,
}

#[derive(Debug, Deserialize)]
struct Config {
    led: Led,
    indent: Indent,
    settings: Settings,
    serial: Option<SerialConfig>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Protocol {
    #[default]
    Awa,
    Adalight,
}

impl Protocol {
    fn header(self) -> &'static [u8; 3] {
        match self {
            Protocol::Awa => b"Awa",
            Protocol::Adalight => b"Ada",
        }
    }
}

impl Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Awa => write!(f, "awa"),
            Protocol::Adalight => write!(f, "adalight"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SerialConfig {
    port: String,
    #[serde(default = "default_serial_baud")]
    baud_rate: u32,
    #[serde(default)]
    protocol: Protocol,
}

fn default_serial_baud() -> u32 {
    2_000_000
}

#[derive(Debug, Deserialize)]
struct Led {
    left: Vec<i32>,
    up: Vec<i32>,
    right: Vec<i32>,
    down: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct Indent {
    left_up: Vec<i32>,
    left_down: Vec<i32>,
    up_left: Vec<i32>,
    up_right: Vec<i32>,
    right_up: Vec<i32>,
    right_down: Vec<i32>,
    down_left: Vec<i32>,
    down_right: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct Settings {
    #[serde(default = "default_size")]
    size: i32,
    #[serde(default = "default_brightness")]
    brightness: f32,
    #[serde(default = "default_delay_ms")]
    delay_ms: u64,
    #[serde(default = "default_smooth")]
    smooth: bool,
    cams: Vec<i32>,
    device_id: usize,
    zone_id_list: Vec<usize>,
    monitor_id_list: Option<Vec<usize>>,
}

fn default_size() -> i32 {
    50
}

fn default_brightness() -> f32 {
    1.0
}

fn default_delay_ms() -> u64 {
    95
}

fn default_smooth() -> bool {
    true
}

pub type Color = RGB8;

#[derive(Debug)]
struct MonitorRes {
    width: i32,
    height: i32,
}

fn get_monitors_info(
    monitor_id_list: Option<Vec<usize>>,
) -> Result<Vec<MonitorRes>, Box<dyn std::error::Error>> {
    // Create an XHandle instance
    let mut xh = XHandle::open()?;
    // Get a list of monitors
    let monitors = xh.monitors()?;

    // Filter and collect the results based on monitor_id_list
    let info = match monitor_id_list {
        None => {
            // Return all monitors if no filter specified
            monitors
                .iter()
                .map(|m| MonitorRes {
                    width: m.width_px,
                    height: m.height_px,
                })
                .collect()
        }
        Some(ids) => {
            // Return only monitors with specified indices
            ids.into_iter()
                .filter_map(|i| {
                    monitors.get(i).map(|m| MonitorRes {
                        width: m.width_px,
                        height: m.height_px,
                    })
                })
                .collect()
        }
    };
    Ok(info)
}

fn load_config() -> Config {
    let config_path = get_config_path().expect("Failed to get config path");
    let config_str = fs::read_to_string(&config_path)
        .unwrap_or_else(|_| panic!("Failed to read config file: {config_path:?}"));
    toml::from_str(&config_str).expect("Failed to parse config TOML")
}

fn load_config_from_file(path: &PathBuf) -> Config {
    let config_str =
        fs::read_to_string(path).unwrap_or_else(|_| panic!("Failed to read config file: {path:?}"));
    toml::from_str(&config_str).expect("Failed to parse config TOML")
}

fn get_config_path() -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "timasoft", "ambiway")?;
    Some(proj_dirs.config_dir().join("config.toml"))
}

fn round_rgb(r: f32, g: f32, b: f32, brightness: f32) -> [u8; 3] {
    [
        (r * brightness).clamp(0.0, 255.0).round() as u8,
        (g * brightness).clamp(0.0, 255.0).round() as u8,
        (b * brightness).clamp(0.0, 255.0).round() as u8,
    ]
}

fn average_rgb(rgb1: [u8; 3], rgb2: [u8; 3]) -> [u8; 3] {
    [
        ((rgb1[0] as u16 + rgb2[0] as u16) / 2) as u8,
        ((rgb1[1] as u16 + rgb2[1] as u16) / 2) as u8,
        ((rgb1[2] as u16 + rgb2[2] as u16) / 2) as u8,
    ]
}

fn get_average_colors(
    regions: &[[i32; 4]],
    cap: &mut VideoCapture,
    previous_avg_colors: &[[u8; 3]],
    brightness: f32,
    smooth: bool,
) -> Result<Vec<[u8; 3]>, Box<dyn std::error::Error>> {
    let mut img = Mat::default();
    let ret = cap.read(&mut img)?;
    if !ret {
        return Ok(vec![]);
    }

    let mut avg_colors = Vec::with_capacity(regions.len());

    for (i, region) in regions.iter().enumerate() {
        let x1 = region[0];
        let y1 = region[1];
        let x2 = region[2];
        let y2 = region[3];

        // Cut ROI from image
        let roi = Mat::roi(&img, core::Rect::new(x1, y1, x2 - x1, y2 - y1))?;

        // mean returns Scalar(B, G, R, A)
        let mean = core::mean(&roi, &core::no_array())?;
        let b = mean[0] as f32;
        let g = mean[1] as f32;
        let r = mean[2] as f32;

        let rounded = round_rgb(r, g, b, brightness);
        if smooth {
            let avg = if previous_avg_colors.is_empty() {
                average_rgb([0, 0, 0], rounded)
            } else {
                average_rgb(previous_avg_colors[i], rounded)
            };
            avg_colors.push(avg);
        } else {
            avg_colors.push(rounded);
        }
    }

    Ok(avg_colors)
}

#[allow(clippy::too_many_arguments)]
fn calculate_regions(
    monitors: &[MonitorRes],
    left_led: &[i32],
    up_led: &[i32],
    right_led: &[i32],
    down_led: &[i32],
    left_up_indent: &[i32],
    left_down_indent: &[i32],
    up_left_indent: &[i32],
    up_right_indent: &[i32],
    right_up_indent: &[i32],
    right_down_indent: &[i32],
    down_left_indent: &[i32],
    down_right_indent: &[i32],
    size: i32,
) -> Vec<Vec<[i32; 4]>> {
    let mut regions_list = Vec::with_capacity(monitors.len());

    for (i, monitor) in monitors.iter().enumerate() {
        // Main sizes
        let inner_width_up = monitor.width - up_left_indent[i] - up_right_indent[i];
        let inner_width_down = monitor.width - down_left_indent[i] - down_right_indent[i];
        let inner_height_left = monitor.height - left_up_indent[i] - left_down_indent[i];
        let inner_height_right = monitor.height - right_up_indent[i] - right_down_indent[i];
        let main_width = monitor.width;
        let main_height = monitor.height;

        // Steps between LEDs
        let left_step = inner_height_left as f32 / left_led[i] as f32;
        let up_step = inner_width_up as f32 / up_led[i] as f32;
        let right_step = inner_height_right as f32 / right_led[i] as f32;
        let down_step = inner_width_down as f32 / down_led[i] as f32;

        let mut monitor_regions: Vec<[i32; 4]> = Vec::new();

        // Left side (from bottom to top)
        {
            let mut b = left_down_indent[i];
            for a in 0..=left_led[i] {
                let value = (left_step * a as f32).round() as i32 + left_down_indent[i];
                if a > 0 {
                    monitor_regions.push([
                        0,
                        inner_height_left - value + left_up_indent[i],
                        size,
                        inner_height_left - b + left_up_indent[i],
                    ]);
                }
                b = value;
            }
        }

        // Top side (from left to right)
        {
            let mut b = up_left_indent[i];
            for a in 0..=up_led[i] {
                let value = (up_step * a as f32).round() as i32 + up_left_indent[i];
                if a > 0 {
                    monitor_regions.push([b, 0, value, size]);
                }
                b = value;
            }
        }

        // Right side (from top to bottom)
        {
            let mut b = right_up_indent[i];
            for a in 0..=right_led[i] {
                let value = (right_step * a as f32).round() as i32 + right_up_indent[i];
                if a > 0 {
                    monitor_regions.push([main_width - size, b, main_width, value]);
                }
                b = value;
            }
        }

        // Bottom side (from right to left)
        {
            let mut b = down_right_indent[i];
            for a in 0..=down_led[i] {
                let value = (down_step * a as f32).round() as i32 + down_right_indent[i];
                if a > 0 {
                    monitor_regions.push([
                        inner_width_down - value + down_left_indent[i],
                        main_height - size,
                        inner_width_down - b + down_left_indent[i],
                        main_height,
                    ]);
                }
                b = value;
            }
        }

        regions_list.push(monitor_regions);
    }

    regions_list
}

async fn send_data<'a>(
    zone: &Zone<'a>,
    data: &[[u8; 3]],
) -> Result<(), Box<dyn std::error::Error>> {
    let colors: Vec<RGB8> = data
        .iter()
        .map(|rgb| RGB8::new(rgb[0], rgb[1], rgb[2]))
        .collect();

    // Send data
    zone.set_leds(colors).await?;

    Ok(())
}

fn prepare_serial_frame(colors: &[[u8; 3]], header: &[u8; 3]) -> Vec<u8> {
    let num_leds = colors.len();
    let count = num_leds.wrapping_sub(1);
    let hi = (count >> 8) as u8;
    let lo = count as u8;
    let checksum = hi ^ lo ^ 0x55;

    let mut buffer = Vec::with_capacity(6 + num_leds * 3 + 3);

    buffer.extend_from_slice(header);
    buffer.push(hi);
    buffer.push(lo);
    buffer.push(checksum);

    for color in colors {
        buffer.extend_from_slice(color);
    }

    let mut f1: u16 = 0;
    let mut f2: u16 = 0;
    let mut fext: u16 = 0;
    for (pos, &byte) in buffer[6..].iter().enumerate() {
        f1 = (f1 + byte as u16) % 255;
        f2 = (f2 + f1) % 255;
        fext = (fext + (byte as u16 ^ (pos & 0xff) as u16)) % 255;
    }
    if fext == 0x41 {
        fext = 0xaa;
    }
    buffer.push(f1 as u8);
    buffer.push(f2 as u8);
    buffer.push(fext as u8);

    buffer
}

async fn send_frame(
    port: &mut SerialStream,
    colors: &[[u8; 3]],
    header: &[u8; 3],
) -> Result<(), Box<dyn std::error::Error>> {
    // 3-byte handshake prefix (triggers HyperHDR/Rp2040 handshake)
    port.write_all(&[0x00, 0x00, 0x00]).await?;

    let buffer = prepare_serial_frame(colors, header);
    port.write_all(&buffer).await?;
    port.flush().await?;

    Ok(())
}

fn open_camera(cam: i32) -> VideoCapture {
    let cap = VideoCapture::new(cam, videoio::CAP_V4L2).expect("Failed to open camera");
    if !cap
        .is_opened()
        .expect("Failed to check if camera is opened")
    {
        eprintln!("Can't open camera {cam}");
    }
    cap
}

#[allow(clippy::too_many_arguments)]
fn run_camera_task(
    cam: i32,
    region: Vec<[i32; 4]>,
    brightness: f32,
    smooth: bool,
    delay_ms: u64,
    manual_pause: Arc<AtomicBool>,
    screen_off: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    mut on_frame: impl FnMut(&[[u8; 3]]),
) {
    let mut cap = open_camera(cam);
    let mut avg_colors = Vec::new();
    let mut is_paused = manual_pause.load(Ordering::Relaxed) || screen_off.load(Ordering::Relaxed);

    if is_paused {
        let black = vec![[0u8; 3]; region.len()];
        on_frame(&black);
    }

    while !shutdown.load(Ordering::Relaxed) {
        let currently_paused =
            manual_pause.load(Ordering::Relaxed) || screen_off.load(Ordering::Relaxed);

        if currently_paused && !is_paused {
            let black = vec![[0u8; 3]; region.len()];
            on_frame(&black);
            is_paused = true;
        } else if !currently_paused && is_paused {
            is_paused = false;
        }

        if currently_paused {
            std::thread::sleep(time::Duration::from_millis(100));
            continue;
        }

        let prev = &avg_colors;
        let res =
            get_average_colors(&region, &mut cap, prev, brightness, smooth).unwrap_or_default();
        avg_colors = res.clone();
        on_frame(&res);
        std::thread::sleep(time::Duration::from_millis(delay_ms));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let config = match args.config {
        Some(path) => {
            println!("Using user config: {path:?}",);
            load_config_from_file(&path)
        }
        None => load_config(),
    };

    let size = config.settings.size;
    let brightness = config.settings.brightness;
    let delay_ms = config.settings.delay_ms;
    let smooth = config.settings.smooth;
    let cams = config.settings.cams;
    let device_id = config.settings.device_id;
    let zone_id_list = config.settings.zone_id_list;
    let monitor_id_list = config.settings.monitor_id_list;

    println!("Loaded config: size = {size}, brightness = {brightness}, delay = {delay_ms}ms");
    if args.paused {
        println!("Starting in PAUSED state.");
    }

    let manual_pause = Arc::new(AtomicBool::new(args.paused));
    let screen_off = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));

    let monitors = get_monitors_info(monitor_id_list)?;

    let region_list = calculate_regions(
        &monitors,
        &config.led.left,
        &config.led.up,
        &config.led.right,
        &config.led.down,
        &config.indent.left_up,
        &config.indent.left_down,
        &config.indent.up_left,
        &config.indent.up_right,
        &config.indent.right_up,
        &config.indent.right_down,
        &config.indent.down_left,
        &config.indent.down_right,
        size,
    );

    let num_threads = cams.len().max(1);
    let rt = Builder::new_multi_thread()
        .worker_threads(num_threads)
        .max_blocking_threads(num_threads)
        .enable_all()
        .build()?;

    rt.block_on(async move {
        // Spawn a task to listen for SIGUSR1 to toggle pause
        let paused_signal = manual_pause.clone();
        tokio::spawn(async move {
            let mut sigusr1 =
                signal(SignalKind::user_defined1()).expect("Failed to listen for SIGUSR1");
            loop {
                sigusr1.recv().await;
                let current = paused_signal.load(Ordering::Relaxed);
                paused_signal.store(!current, Ordering::Relaxed);
                println!("[Signal] Pause toggled. New state: {}", !current);
            }
        });

        // Spawn a task to listen for SIGTERM/SIGINT to shutdown
        let shutdown_signal = shutdown.clone();
        tokio::spawn(async move {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to listen for SIGTERM");
            let mut sigint = signal(SignalKind::interrupt()).expect("Failed to listen for SIGINT");
            let sig = select! {
                _ = sigterm.recv() => "SIGTERM",
                _ = sigint.recv() => "SIGINT",
            };
            println!("[Signal] {sig} received, shutting down...");
            shutdown_signal.store(true, Ordering::Relaxed);
        });

        // Spawn a task to poll DRM DPMS state automatically
        let paused_signal = screen_off.clone();
        tokio::spawn(async move {
            loop {
                let mut is_dpms_off = false;

                if let Ok(paths) = std::fs::read_dir("/sys/class/drm") {
                    for path in paths.flatten() {
                        let dpms_path = path.path().join("dpms");
                        if let Ok(state) = std::fs::read_to_string(dpms_path)
                            && state.trim() == "Off"
                        {
                            is_dpms_off = true;
                            break;
                        }
                    }
                }

                paused_signal.store(is_dpms_off, Ordering::Relaxed);

                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });

        let mut handles = Vec::with_capacity(cams.len());

        if let Some(ref serial_cfg) = config.serial {
            println!(
                "Using serial port: {} at {} baud",
                serial_cfg.port, serial_cfg.baud_rate
            );

            let port =
                SerialStream::open(&tokio_serial::new(&serial_cfg.port, serial_cfg.baud_rate))
                    .expect("Failed to open serial port");
            let port = Arc::new(Mutex::new(port));

            let total_leds: usize = region_list.iter().map(|r| r.len()).sum();
            let led_counts: Vec<usize> = region_list.iter().map(|r| r.len()).collect();
            let led_offsets: Vec<usize> = led_counts
                .iter()
                .scan(0, |acc, &x| {
                    let start = *acc;
                    *acc += x;
                    Some(start)
                })
                .collect();

            let header = serial_cfg.protocol.header();
            println!(
                "Using protocol: {} ({:02x} {:02x} {:02x})",
                serial_cfg.protocol, header[0], header[1], header[2]
            );

            let shared_colors: Arc<Mutex<Vec<[u8; 3]>>> =
                Arc::new(Mutex::new(vec![[0u8; 3]; total_leds]));

            let port_clone = port.clone();
            let colors_clone = shared_colors.clone();
            let pause_clone = manual_pause.clone();
            let off_clone = screen_off.clone();
            let sd = shutdown.clone();
            let total = total_leds;
            handles.push(tokio::spawn(async move {
                loop {
                    if sd.load(Ordering::Relaxed) {
                        let black = vec![[0u8; 3]; total];
                        let mut port = port_clone.lock().await;
                        for _ in 0..SHUTDOWN_BLACK_REPEATS {
                            let _ = send_frame(&mut port, &black, header).await;
                        }
                        break;
                    }

                    let is_paused =
                        pause_clone.load(Ordering::Relaxed) || off_clone.load(Ordering::Relaxed);

                    let colors = if is_paused {
                        vec![[0u8; 3]; total]
                    } else {
                        colors_clone.lock().await.clone()
                    };

                    let mut port = port_clone.lock().await;
                    if let Err(e) = send_frame(&mut port, &colors, header).await {
                        eprintln!("Serial send error: {e}");
                    }
                    drop(port);

                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }));

            for (i, &cam) in cams.iter().enumerate() {
                let region = region_list[i].clone();
                let offset = led_offsets[i];
                let colors_out = shared_colors.clone();
                let mp = manual_pause.clone();
                let so = screen_off.clone();
                let sd = shutdown.clone();

                handles.push(tokio::spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        run_camera_task(cam, region, brightness, smooth, delay_ms, mp, so, sd, {
                            let colors_out = colors_out.clone();
                            move |frame| {
                                let mut colors = colors_out.blocking_lock();
                                if !frame.is_empty() {
                                    colors[offset..offset + frame.len()].copy_from_slice(frame);
                                }
                            }
                        });
                    })
                    .await
                    .unwrap();
                }));
            }
        } else {
            let client = OpenRgbClient::connect().await.unwrap();
            for (i, &cam) in cams.iter().enumerate() {
                let region = region_list[i].clone();
                let brightness = brightness;
                let controller = client.get_controller(device_id).await.unwrap();
                let zone_id = zone_id_list[i];

                let manual_pause_clone = manual_pause.clone();
                let screen_off_clone = screen_off.clone();
                let sd = shutdown.clone();

                handles.push(tokio::spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let zone = controller.get_zone(zone_id).unwrap();
                        run_camera_task(
                            cam,
                            region,
                            brightness,
                            smooth,
                            delay_ms,
                            manual_pause_clone,
                            screen_off_clone,
                            sd,
                            |frame| {
                                tokio::runtime::Handle::current()
                                    .block_on(send_data(&zone, frame))
                                    .expect("Failed to send data");
                            },
                        );
                    })
                    .await
                    .unwrap();
                }));
            }
        }

        if shutdown.load(Ordering::Relaxed) {
            if config.serial.is_none()
                && let Ok(client) = OpenRgbClient::connect().await
                && let Ok(controller) = client.get_controller(device_id).await
            {
                for _ in 0..SHUTDOWN_BLACK_REPEATS {
                    for &zone_id in &zone_id_list {
                        if let Ok(zone) = controller.get_zone(zone_id) {
                            let _ = zone.set_all_leds(RGB8::new(0, 0, 0)).await;
                        }
                    }
                }
            }
            println!("Shutdown complete.");
        }

        for h in handles {
            if let Err(e) = h.await {
                eprintln!("Task failed: {e}");
            }
        }
    });

    Ok(())
}
