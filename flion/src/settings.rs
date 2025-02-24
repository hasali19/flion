use std::mem;

use serde_json::json;
use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

use crate::engine::FlutterEngine;

pub fn send_to_engine(engine: &FlutterEngine) -> eyre::Result<()> {
    let mut use_light_theme = 0u32;
    let mut use_light_theme_size = mem::size_of_val(&use_light_theme) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut use_light_theme as *mut _ as _),
            Some(&mut use_light_theme_size),
        )
        .ok()?;
    }

    let message = json!({
        "platformBrightness": if use_light_theme == 0 { "dark" } else { "light" },
        "alwaysUse24HourFormat": false,
        "textScaleFactor": 1.0f32,
    });

    engine.send_platform_message(c"flutter/settings", &serde_json::to_vec(&message)?)?;

    Ok(())
}
