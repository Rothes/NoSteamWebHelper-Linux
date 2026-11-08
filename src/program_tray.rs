use std::sync::atomic::Ordering;
use std::sync::Mutex;
use crate::{ACTIVATED_GAME_MODE, DISABLE_CEF_ON_GAMING, kill_cef, term_steam, stop_game_mode, is_no_game_process, start_game_mode};

static TRAY_INSTANCE: Mutex<Option<ksni::blocking::Handle<ProgramTray>>> = Mutex::new(None);

struct ProgramTray;
impl ProgramTray {
    fn new() -> Self {
        Self {}
    }
}

impl ksni::Tray for ProgramTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }
    fn title(&self) -> String {
        "Steam-NoSteamWebHelper".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        use ksni::Icon;
        fn create_icon(size: u32, color: [u8; 4]) -> Icon {
            let mut data = Vec::with_capacity((size * size * 4) as usize);
            let center = size as f32 / 2.0;
            let radius = center * 0.6;

            for y in 0..size {
                for x in 0..size {
                    let dx = x as f32 + 0.5 - center;
                    let dy = y as f32 + 0.5 - center;
                    let distance = (dx * dx + dy * dy).sqrt();

                    let alpha = ((radius + 0.5 - distance).clamp(0.0, 1.0)
                        * color[3] as f32) as u8;

                    data.extend_from_slice(&[alpha, color[0], color[1], color[2]]);
                }
            }

            Icon {
                width: size as i32,
                height: size as i32,
                data,
            }
        }
        let color = if ACTIVATED_GAME_MODE.load(Ordering::Relaxed) {
            [46, 204, 113, 255]
        } else if DISABLE_CEF_ON_GAMING.load(Ordering::Relaxed) {
            [241, 196, 15, 255]
        } else {
            [149, 165, 166, 255]
        };
        vec![
            create_icon(64, color),
            create_icon(32, color),
        ]
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            CheckmarkItem {
                label: "Disable CEF On Gaming".into(),
                checked: DISABLE_CEF_ON_GAMING.load(Ordering::Relaxed),
                activate: Box::new(|_| update_tray_options()),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Restart CEF".into(),
                activate: Box::new(|_| kill_cef()),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Exit Tray".into(),
                activate: Box::new(|_| { TRAY_INSTANCE.lock().unwrap().take().unwrap().shutdown(); }),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Exit Steam".into(),
                activate: Box::new(|_| term_steam()),
                ..Default::default()
            }.into(),
        ]
    }
}

pub fn start_tray() {
    match std::env::var("DISABLE_NOSTEAMWEBHELPER_TRAY") {
        Ok(value) => {
            if value == "1" || value == "true" {
                crate::log!("DISABLE_NOSTEAMWEBHELPER_TRAY env is true");
                return;
            }
        }
        Err(_) => {}
    }

    use ksni::blocking::TrayMethods;
    if let Ok(tray) = ProgramTray::new().spawn() {
        let mut guard = TRAY_INSTANCE.lock().unwrap();
        *guard = Some(tray);
    }
}

pub fn update_tray() {
    if let Some(tray) = TRAY_INSTANCE.lock().unwrap().as_ref() {
        tray.update(|_| {}); // Update tray icon
    }
}

fn update_tray_options() {
    let old = DISABLE_CEF_ON_GAMING.fetch_xor(true, Ordering::Relaxed);
    if old {
        stop_game_mode();
    } else if !is_no_game_process() {
        start_game_mode();
    }
}
