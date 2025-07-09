use std::time;
use std::thread;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use opencv::prelude::*;
use opencv::{videoio, core};
use xrandr::XHandle;
use rgb::RGB8;
use tokio::net::TcpStream;
use openrgb::OpenRGB;

// struct VideoCaptureAsync {
// 	cap: videoio::VideoCapture,
//     ret: bool,
//     frame: Mat,
//     running: bool,
// }
//
// impl VideoCaptureAsync {
//     fn new(source: i32) -> Self {
//         let mut cap = videoio::VideoCapture::new(source, videoio::CAP_ANY).unwrap();
//         let ret = false;
//         let frame = Mat::default();
//         let running = true;
//     }
//
//     fn update(&mut self) -> Result<()> {
//         loop {
//             let mut frame = Mat::default();
//             let ret = self.cap.read(&mut frame).unwrap();
//             self.ret = ret;
//             if ret {
//                 self.frame = frame;
//             }
//         }
//     }
// }

pub type Color = RGB8;

pub struct VideoCaptureAsync {
    shared: Arc<Mutex<Shared>>,
    handle: Option<thread::JoinHandle<()>>,
    running: Arc<AtomicBool>, // Атомарный флаг для управления потоком
}

struct Shared {
    frame: Arc<Mat>, // Кадр в Arc для быстрого клонирования
    ret: bool,       // Статус последнего чтения
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
            frame: Arc::new(frame), // Начальный кадр в Arc
            ret,
        }));

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread_shared = Arc::clone(&shared);

        let handle = thread::spawn(move || {
            loop {
                // Проверяем флаг без блокировки мьютекса
                if !thread_running.load(Ordering::Relaxed) {
                    break;
                }

                // Читаем кадр БЕЗ блокировки мьютекса
                let mut mat = Mat::default();
                let ret = cap.read(&mut mat);

                // Проверяем флаг после чтения
                if !thread_running.load(Ordering::Relaxed) {
                    break;
                }

                let mut s = match thread_shared.lock() {
                    Ok(guard) => guard,
                    Err(_) => break, // Если мьютекст отравлен
                };

                match ret {
                    Ok(r) => {
                        s.ret = r;
                        if r {
                            // Обновляем кадр (быстрое создание Arc)
                            s.frame = Arc::new(mat);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading frame: {}", e);
                        s.ret = false; // Важно: обновляем статус!
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
        Ok((s.ret, s.frame.clone())) // Клонирование Arc (дешево)
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed); // Сигнал потоку на остановку
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap(); // Ожидание завершения потока
        }
    }
}

impl Drop for VideoCaptureAsync {
    fn drop(&mut self) {
        self.stop();
        // VideoCapture освободится автоматически при drop
    }
}

// fn get_monitors_info() -> Result<Vec<HashMap<String, i32>>, Box<dyn std::error::Error>> {
//     // Подключаемся к X серверу
//     let mut xh = XHandle::open()?;
//     // Получаем информацию о мониторах
//     let monitors = xh.monitors()?;
//
//     // Собираем результат
//     let info = monitors.iter().map(|m| {
//         let mut map = HashMap::new();
//         map.insert("width".to_string(), m.width_px);
//         map.insert("height".to_string(), m.height_px);
//         map
//     }).collect();
//
//     Ok(info)
// }

fn get_monitors_info() -> Result<Vec<MonitorRes>, Box<dyn std::error::Error>> {
    // Подключаемся к X серверу
    let mut xh = XHandle::open()?;
    // Получаем информацию о мониторах
    let monitors = xh.monitors()?;
    // Собираем результат
    let info = monitors.iter().map(|m| {
        MonitorRes {
            width: m.width_px,
            height: m.height_px,
        }
    }).collect();
    Ok(info)
}

fn round_rgb(r: f32, g: f32, b: f32, brightness: f32) -> [u8; 3] {
    [
        (r * brightness).round() as u8,
        (g * brightness).round() as u8,
        (b * brightness).round() as u8,
    ]
}

// Функция для усреднения двух RGB‑цветов.
fn average_rgb(rgb1: [u8; 3], rgb2: [u8; 3]) -> [u8; 3] {
    [
        ((rgb1[0] as u16 + rgb2[0] as u16) / 2) as u8,
        ((rgb1[1] as u16 + rgb2[1] as u16) / 2) as u8,
        ((rgb1[2] as u16 + rgb2[2] as u16) / 2) as u8,
    ]
}


fn get_average_colors(
    regions: &[[i32;4]],
    cap: &VideoCaptureAsync,
    previous_avg_colors: &[[u8;3]],
    brightness: f32,
) -> Result<Vec<[u8;3]>, Box<dyn std::error::Error>> {
    let (ret, img) = cap.read()?;
    if !ret {
        return Ok(vec![]);
    }

    let mut avg_colors = Vec::with_capacity(regions.len());

    for (i, region) in regions.iter().enumerate() {
        let x1 = region[0]; let y1 = region[1];
        let x2 = region[2]; let y2 = region[3];

        // вырезаем ROI:
        let roi = Mat::roi(img.as_ref(), core::Rect::new(x1, y1, x2-x1, y2-y1))?;

        // mean возвращает Scalar(B, G, R, A)
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
        // Основные размеры
        let inner_width = monitor.width - left_indent[i] - right_indent[i];
        let inner_height = monitor.height - up_indent[i] - down_indent[i];
        let main_width = monitor.width;
        let main_height = monitor.height;

        // Шаги между светодиодами
        let left_step = inner_height as f32 / left_led[i] as f32;
        let up_step = inner_width as f32 / up_led[i] as f32;
        let right_step = inner_height as f32 / right_led[i] as f32;
        let down_step = inner_width as f32 / down_led[i] as f32;

        let mut monitor_regions: Vec<[i32; 4]> = Vec::new();

        // Левая сторона (снизу вверх)
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

        // Верхняя сторона (слева направо)
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

        // Правая сторона (сверху вниз)
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

        // Нижняя сторона (справа налево)
        {
            let mut b = right_indent[i];
            for a in 0..=down_led[i] {
                let value = (down_step * a as f32).round() as i32 + right_indent[i];
                if a > 0 {
                    monitor_regions.push([inner_width - value, main_height - size, inner_width - b, main_height]);
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
    // Чёрный первый, затем остальные из data
    let mut colors: Vec<RGB8> = Vec::with_capacity(data.len() + 1);
    colors.push(RGB8::new(0, 0, 0));
    for &(r, g, b) in data {
        colors.push(RGB8::new(r, g, b));
    }

    // Отправляем на устройство
    client.update_leds(0, colors).await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let left_led = vec![36,41];
    let up_led = vec![62,76];
    let right_led = vec![36,42];
    let down_led = vec![62,81];
    let left_indent = vec![0,0];
    let up_indent = vec![0,40];
    let right_indent = vec![0,0];
    let down_indent = vec![0,0];
    let size: i32 = 50;
    let brightness: f32 = 0.5;

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
    let cams = [2,3];
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

        // обновим avg_colors
        for (i, res) in results.iter().enumerate() {
            avg_colors[i] = res.clone();
        }

        let flat: Vec<(u8, u8, u8)> = results.into_iter().flatten().map(|rgb| (rgb[0], rgb[1], rgb[2])).collect();

        // Отправляем данные
        // println!("{:#?}", flat);

        send_data(&client, &flat).await?;

        // Отправляем данные
        // tokio::spawn(send_data(&client, &flat));

        // Ожидаем 10 миллисекунд
        tokio::time::sleep(time::Duration::from_millis(10)).await;
    }

    // caps будут остановлены автоматически при drop
    Ok(())
}

// fn main() -> Result<(), Box<dyn std::error::Error>> {
//     let left_led = vec![36,41];
//     let up_led = vec![62,76];
//     let right_led = vec![36,42];
//     let down_led = vec![62,81];
//     let left_indent = vec![0,0];
//     let up_indent = vec![0,40];
//     let right_indent = vec![0,0];
//     let down_indent = vec![0,0];
//     let size = 50;
//
//     let monitors = get_monitors_info()?;
//
//     let region_list = calculate_regions(
//         &monitors,
//         &left_led,
//         &up_led,
//         &right_led,
//         &down_led,
//         &left_indent,
//         &up_indent,
//         &right_indent,
//         &down_indent,
//         size,
//     );
//
//     let mut avg_colors: Vec<Vec<(u8, u8, u8)>> = vec![Vec::new(); monitors.len()];
//
//     loop {
//
//     }
// }

// fn main() -> Result<(), Box<dyn std::error::Error>> {
//     let mut cap = VideoCaptureAsync::new(0)?;
//     let infos = get_monitors_info()?;
//     println!("{:#?}", infos);
//
//     loop {
//         let (ret, frame) = cap.read()?;
//         if !ret {
//             eprintln!("Failed to capture frame");
//             break;
//         }
//     }
//
//     cap.stop();
//     Ok(())
// }

