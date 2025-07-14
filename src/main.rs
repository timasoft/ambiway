use clap::Parser;
use directories::ProjectDirs;
use opencv::prelude::*;
use opencv::{core, videoio};
use openrgb::OpenRGB;
use rgb::RGB8;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time;
use tokio::net::TcpStream;
use xrandr::XHandle;

/// Ambilight with OpenRGB
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Run in GUI mode
    #[arg(short = 'g', long = "gui")]
    gui: bool,

    /// Set config file
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Config {
    led: Led,
    indent: Indent,
    settings: Settings,
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
    left: Vec<i32>,
    up: Vec<i32>,
    right: Vec<i32>,
    down: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct Settings {
    size: i32,
    brightness: f32,
    cams: Vec<i32>,
}

pub type Color = RGB8;

pub struct VideoCaptureAsync {
    shared: Arc<Mutex<Shared>>,
    handle: Option<thread::JoinHandle<()>>,
    running: Arc<AtomicBool>, // Atomic bool for controlling thread
}

struct Shared {
    frame: Arc<Mat>, // Frame in Arc for fast cloning
    ret: bool,       // Status of last read
}

#[derive(Debug)]
struct MonitorRes {
    width: i32,
    height: i32,
}

impl VideoCaptureAsync {
    fn new(source: i32) -> opencv::Result<Self> {
        let mut cap = videoio::VideoCapture::new(source, videoio::CAP_V4L2)?;
        if !cap.is_opened()? {
            panic!("Can't open camera {}", source);
        }

        let mut frame = Mat::default();
        let ret = cap.read(&mut frame)?;

        let shared = Arc::new(Mutex::new(Shared {
            frame: Arc::new(frame), // Initial frame in Arc
            ret,
        }));

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread_shared = Arc::clone(&shared);

        let handle = thread::spawn(move || {
            loop {
                // Check the flag without locking the mutex
                if !thread_running.load(Ordering::Relaxed) {
                    break;
                }

                // Read the frame without locking the mutex
                let mut mat = Mat::default();
                let ret = cap.read(&mut mat);

                // Check the flag after reading
                if !thread_running.load(Ordering::Relaxed) {
                    break;
                }

                let mut s = match thread_shared.lock() {
                    Ok(guard) => guard,
                    Err(_) => break, // If mutex is poisoned
                };

                match ret {
                    Ok(r) => {
                        s.ret = r;
                        if r {
                            // Update frame (fast creation of Arc)
                            s.frame = Arc::new(mat);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading frame: {}", e);
                        s.ret = false; //Important: update status!
                    }
                }
            }
        });

        Ok(VideoCaptureAsync {
            shared,
            handle: Some(handle),
            running,
        })
    }

    fn read(&self) -> opencv::Result<(bool, Arc<Mat>)> {
        let s = self.shared.lock().unwrap();
        Ok((s.ret, s.frame.clone())) // Cloning Arc (cheap)
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed); // Signal thread to stop
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap(); // Waiting for thread to finish
        }
    }
}

impl Drop for VideoCaptureAsync {
    fn drop(&mut self) {
        self.stop();
    }
}

fn get_monitors_info() -> Result<Vec<MonitorRes>, Box<dyn std::error::Error>> {
    // Create an XHandle instance
    let mut xh = XHandle::open()?;
    // Get a list of monitors
    let monitors = xh.monitors()?;
    // Collect the results
    let info = monitors
        .iter()
        .map(|m| MonitorRes {
            width: m.width_px,
            height: m.height_px,
        })
        .collect();
    Ok(info)
}

fn load_config() -> Config {
    let config_path = get_config_path().expect("Failed to get config path");
    let config_str = fs::read_to_string(&config_path)
        .unwrap_or_else(|_| panic!("Failed to read config file: {:?}", config_path));
    toml::from_str(&config_str).expect("Failed to parse config TOML")
}

fn load_config_from_file(path: &PathBuf) -> Config {
    let config_str = fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("Failed to read config file: {:?}", path));
    toml::from_str(&config_str).expect("Failed to parse config TOML")
}

fn get_config_path() -> Option<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "timasoft", "ambiway")?;
    Some(proj_dirs.config_dir().join("config.toml"))
}

fn round_rgb(r: f32, g: f32, b: f32, brightness: f32) -> [u8; 3] {
    [
        (r * brightness).round() as u8,
        (g * brightness).round() as u8,
        (b * brightness).round() as u8,
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
    cap: &VideoCaptureAsync,
    previous_avg_colors: &[[u8; 3]],
    brightness: f32,
) -> Result<Vec<[u8; 3]>, Box<dyn std::error::Error>> {
    let (ret, img) = cap.read()?;
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
        let roi = Mat::roi(img.as_ref(), core::Rect::new(x1, y1, x2 - x1, y2 - y1))?;

        // mean returns Scalar(B, G, R, A)
        let mean = core::mean(&roi, &core::no_array())?;
        let b = mean[0] as f32;
        let g = mean[1] as f32;
        let r = mean[2] as f32;

        let rounded = round_rgb(r, g, b, brightness);

        let avg = if previous_avg_colors.is_empty() {
            average_rgb([0, 0, 0], rounded)
        } else {
            average_rgb(previous_avg_colors[i], rounded)
        };
        avg_colors.push(avg);
    }

    Ok(avg_colors)
}

fn calculate_regions(
    monitors: &[MonitorRes],
    left_led: &[i32],
    up_led: &[i32],
    right_led: &[i32],
    down_led: &[i32],
    left_indent: &[i32],
    up_indent: &[i32],
    right_indent: &[i32],
    down_indent: &[i32],
    size: i32,
) -> Vec<Vec<[i32; 4]>> {
    let mut regions_list = Vec::with_capacity(monitors.len());

    for (i, monitor) in monitors.iter().enumerate() {
        // Main sizes
        let inner_width = monitor.width - left_indent[i] - right_indent[i];
        let inner_height = monitor.height - up_indent[i] - down_indent[i];
        let main_width = monitor.width;
        let main_height = monitor.height;

        // Steps between LEDs
        let left_step = inner_height as f32 / left_led[i] as f32;
        let up_step = inner_width as f32 / up_led[i] as f32;
        let right_step = inner_height as f32 / right_led[i] as f32;
        let down_step = inner_width as f32 / down_led[i] as f32;

        let mut monitor_regions: Vec<[i32; 4]> = Vec::new();

        // Left side (from bottom to top)
        {
            let mut b = down_indent[i];
            for a in 0..=left_led[i] {
                let value = (left_step * a as f32).round() as i32 + down_indent[i];
                if a > 0 {
                    monitor_regions.push([0, inner_height - value, size, inner_height - b]);
                }
                b = value;
            }
        }

        // Top side (from left to right)
        {
            let mut b = left_indent[i];
            for a in 0..=up_led[i] {
                let value = (up_step * a as f32).round() as i32 + left_indent[i];
                if a > 0 {
                    monitor_regions.push([b, 0, value, size]);
                }
                b = value;
            }
        }

        // Right side (from top to bottom)
        {
            let mut b = up_indent[i];
            for a in 0..=right_led[i] {
                let value = (right_step * a as f32).round() as i32 + up_indent[i];
                if a > 0 {
                    monitor_regions.push([main_width - size, b, main_width, value]);
                }
                b = value;
            }
        }

        // Bottom side (from right to left)
        {
            let mut b = right_indent[i];
            for a in 0..=down_led[i] {
                let value = (down_step * a as f32).round() as i32 + right_indent[i];
                if a > 0 {
                    monitor_regions.push([
                        inner_width - value,
                        main_height - size,
                        inner_width - b,
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

async fn send_data(
    client: &OpenRGB<TcpStream>,
    data: &[(u8, u8, u8)],
) -> Result<(), Box<dyn std::error::Error>> {
    // Black first, then the rest of data
    let mut colors: Vec<RGB8> = Vec::with_capacity(data.len() + 1);
    colors.push(RGB8::new(0, 0, 0));
    for &(r, g, b) in data {
        colors.push(RGB8::new(r, g, b));
    }

    // Send data
    client.update_leds(0, colors).await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let config = match args.config {
        Some(path) => {
            println!("Using user config: {:?}", path);
            load_config_from_file(&path)
        }
        None => load_config(),
    };

    // Process GUI mode
    if args.gui {
        println!("Not implemented yet");
        return Ok(());
    }

    let left_led = config.led.left;
    let up_led = config.led.up;
    let right_led = config.led.right;
    let down_led = config.led.down;

    let left_indent = config.indent.left;
    let up_indent = config.indent.up;
    let right_indent = config.indent.right;
    let down_indent = config.indent.down;

    let size = config.settings.size;
    let brightness = config.settings.brightness;
    let cams = config.settings.cams;

    println!(
        "Loaded config: size = {}, brightness = {}",
        size, brightness
    );

    let monitors = get_monitors_info()?;

    let region_list = calculate_regions(
        &monitors,
        &left_led,
        &up_led,
        &right_led,
        &down_led,
        &left_indent,
        &up_indent,
        &right_indent,
        &down_indent,
        size,
    );

    let mut avg_colors: Vec<Vec<[u8; 3]>> = vec![Vec::new(); monitors.len()];

    let mut caps: Vec<VideoCaptureAsync> = Vec::new();
    for i in cams {
        caps.push(VideoCaptureAsync::new(i as i32)?);
    }

    let client = OpenRGB::connect().await?;

    loop {
        let tasks: Vec<_> = region_list
            .iter()
            .enumerate()
            .map(|(i, regions)| {
                let cap = &caps[i];
                let prev = &avg_colors[i];
                get_average_colors(regions, cap, prev, brightness)
            })
            .collect();

        let results: Vec<_> = tasks.into_iter().map(|r| r.unwrap_or_default()).collect();

        // Update avg_colors
        for (i, res) in results.iter().enumerate() {
            avg_colors[i] = res.clone();
        }

        let flat: Vec<(u8, u8, u8)> = results
            .into_iter()
            .flatten()
            .map(|rgb| (rgb[0], rgb[1], rgb[2]))
            .collect();

        send_data(&client, &flat).await?;

        // Sleep 10 ms
        tokio::time::sleep(time::Duration::from_millis(10)).await;
    }
}
