//! Native Windows toasts from an unpackaged exe.
//!
//! Toasts need an app identity. Registering an AppUserModelID under
//! HKCU\Software\Classes\AppUserModelId is enough for the notification platform to show
//! and group them; no installer or Start Menu shortcut is required.
use windows::core::*;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

pub const AUMID: &str = "Notch.Island";

pub fn register() {
    let key = format!("Software\\Classes\\AppUserModelId\\{AUMID}");
    crate::winutil::reg_set_string(&key, "DisplayName", "Notch");
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn show_now(title: &str, body: &str, silent: bool) -> Result<()> {
    let audio = if silent { "<audio silent=\"true\"/>" } else { "<audio src=\"ms-winsoundevent:Notification.Default\"/>" };
    let xml = format!("<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{audio}</toast>", esc(title), esc(body));
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

/// Show a toast without blocking the UI thread.
pub fn show(title: String, body: String, silent: bool) {
    let _ = std::thread::Builder::new().name("toast".into()).stack_size(256 * 1024).spawn(move || {
        if let Err(e) = show_now(&title, &body, silent) {
            crate::logln!("toast failed: {e}");
        }
    });
}
