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
    // "notch://" links: clicking a toast runs `notch.exe --activate <link>`, which tells the
    // running notch which session to bring to the front.
    let exe = crate::util::exe_path();
    crate::winutil::reg_set_string("Software\\Classes\\notch", "", "URL:Notch");
    crate::winutil::reg_set_string("Software\\Classes\\notch", "URL Protocol", "");
    crate::winutil::reg_set_string("Software\\Classes\\notch\\shell\\open\\command", "", &format!("\"{}\" --activate \"%1\"", exe.display()));
}

/// The link a toast for `session_id` opens.
pub fn focus_link(session_id: &str) -> String {
    let id: String = session_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    format!("notch://focus/{id}")
}

/// "notch://focus/<id>" -> "<id>"
pub fn parse_link(url: &str) -> Option<String> {
    let id = url.trim().trim_end_matches('/').strip_prefix("notch://focus/")?;
    (!id.is_empty()).then(|| id.to_string())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn show_now(title: &str, body: &str, silent: bool, link: &str) -> Result<()> {
    let audio = if silent { "<audio silent=\"true\"/>" } else { "<audio src=\"ms-winsoundevent:Notification.Default\"/>" };
    let xml = format!("<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{audio}</toast>", esc(link), esc(title), esc(body));
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

/// Show a toast without blocking the UI thread. Clicking it opens `link`.
pub fn show(title: String, body: String, silent: bool, link: String) {
    let _ = std::thread::Builder::new().name("toast".into()).stack_size(256 * 1024).spawn(move || {
        if let Err(e) = show_now(&title, &body, silent, &link) {
            crate::logln!("toast failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links() {
        assert_eq!(focus_link("ab-12_c"), "notch://focus/ab-12_c");
        assert_eq!(parse_link("notch://focus/ab-12_c/").as_deref(), Some("ab-12_c"));
        assert_eq!(parse_link(&focus_link("x\"><y")).as_deref(), Some("xy"));
        assert_eq!(parse_link("notch://other"), None);
    }
}
