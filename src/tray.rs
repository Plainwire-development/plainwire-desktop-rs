use tray_icon::{TrayIcon, TrayIconBuilder};
use std::fs;
use std::path::PathBuf;

pub struct Tray {
    icon: Option<TrayIcon>,
    icon_path: Option<PathBuf>,
}

impl Tray {
    pub fn new(icon_path: Option<PathBuf>) -> Self {
        Self { icon: None, icon_path }
    }

    pub fn build(&mut self, window_title: &str) {
        if let Some(path) = &self.icon_path {
            if let Ok(bytes) = fs::read(path) {
                if let Ok(icon) = tray_icon::Icon::from_rgba(bytes, 32, 32) {
                    let builder = TrayIconBuilder::new()
                        .with_title(window_title)
                        .with_icon(icon);

                    if let Ok(tray) = builder.build() {
                        self.icon = Some(tray);
                    }
                }
            }
        }
    }

    pub fn show(&self) {
        if let Some(ref icon) = self.icon {
            let _ = icon.set_visible(true);
        }
    }
}
