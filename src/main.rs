use clap::Parser;
use directories::ProjectDirs;
use opencv::prelude::*;
use opencv::videoio::VideoCapture;
use opencv::{core, videoio};
use openrgb2::{OpenRgbClient, Zone};
use rgb::RGB8;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time;
use tokio::runtime::Builder;
use xrandr::XHandle;

/// Ambilight with OpenRGB
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
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
    size: i32,
    brightness: f32,
    smooth: bool,
    cams: Vec<i32>,
    device_id: usize,
    zone_id_list: Vec<usize>,
}

pub type Color = RGB8;

#[derive(Debug)]
struct MonitorRes {
    width: i32,
    height: i32,
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
    let smooth = config.settings.smooth;
    let cams = config.settings.cams;
    let device_id = config.settings.device_id;
    let zone_id_list = config.settings.zone_id_list;

    println!("Loaded config: size = {size}, brightness = {brightness}");

    let monitors = get_monitors_info()?;

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
        .max_blocking_threads(num_threads) // можно регулировать отдельно
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let mut handles = Vec::with_capacity(cams.len());
        let client = OpenRgbClient::connect().await.unwrap();
        for (i, &cam) in cams.iter().enumerate() {
            let region = region_list[i].clone();
            let brightness = brightness;
            let controller = client.get_controller(device_id).await.unwrap();
            let zone_id = zone_id_list[i];

            handles.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let zone = controller.get_zone(zone_id).unwrap();
                    let mut cap =
                        VideoCapture::new(cam, videoio::CAP_V4L2).expect("Failed to open camera");
                    if !cap
                        .is_opened()
                        .expect("Failed to check if camera is opened")
                    {
                        eprintln!("Can't open camera {cam}");
                        return;
                    }
                    let mut avg_colors = Vec::new();
                    loop {
                        let prev = &avg_colors;
                        let res = get_average_colors(&region, &mut cap, prev, brightness, smooth)
                            .unwrap_or_default();
                        avg_colors = res.clone();
                        tokio::runtime::Handle::current()
                            .block_on(send_data(&zone, &res))
                            .expect("Failed to send data");
                        std::thread::sleep(time::Duration::from_millis(95));
                    }
                })
                .await
                .unwrap();
            }));
        }

        for h in handles {
            if let Err(e) = h.await {
                eprintln!("Task failed: {e}");
            }
        }
    });

    Ok(())
}
