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
    crate::winutil::reg_set_string(&key, "DisplayName", "AgentNotch");
    // "notch://" links: clicking a toast runs `AgentNotch.exe --activate <link>`, which tells the
    // running notch which session to bring to the front.
    let exe = crate::util::exe_path();
    crate::winutil::reg_set_string("Software\\Classes\\notch", "", "URL:AgentNotch");
    crate::winutil::reg_set_string("Software\\Classes\\notch", "URL Protocol", "");
    crate::winutil::reg_set_string("Software\\Classes\\notch\\shell\\open\\command", "", &format!("\"{}\" --activate \"%1\"", exe.display()));
}

fn safe_id(session_id: &str) -> String {
    session_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// The link a toast for `session_id` opens.
pub fn focus_link(session_id: &str) -> String {
    format!("notch://focus/{}", safe_id(session_id))
}

/// "notch://focus/<id>" -> ("focus", "<id>"); also "allow" and "deny".
pub fn parse_link(url: &str) -> Option<(String, String)> {
    let rest = url.trim().trim_end_matches('/').strip_prefix("notch://")?;
    let (verb, id) = rest.split_once('/')?;
    (matches!(verb, "focus" | "allow" | "deny") && !id.is_empty()).then(|| (verb.to_string(), id.to_string()))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn show_now(title: &str, body: &str, silent: bool, link: &str, decide: Option<&str>) -> Result<()> {
    let audio = if silent { "<audio silent=\"true\"/>" } else { "<audio src=\"ms-winsoundevent:Notification.Default\"/>" };
    // Approve / Deny buttons for a permission request
    let actions = match decide {
        Some(id) => format!(
            "<actions><action content=\"Approve\" activationType=\"protocol\" arguments=\"notch://allow/{0}\"/><action content=\"Deny\" activationType=\"protocol\" arguments=\"notch://deny/{0}\"/></actions>",
            safe_id(id)
        ),
        None => String::new(),
    };
    let xml = format!("<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{actions}{audio}</toast>", esc(link), esc(title), esc(body));
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

/// Show a toast without blocking the UI thread. Clicking it opens `link`.
/// `decide` = a session id: the toast gets Approve and Deny buttons for it.
pub fn show(title: String, body: String, silent: bool, link: String, decide: Option<String>) {
    let _ = std::thread::Builder::new().name("toast".into()).stack_size(256 * 1024).spawn(move || {
        if let Err(e) = show_now(&title, &body, silent, &link, decide.as_deref()) {
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
        assert_eq!(parse_link("notch://focus/ab-12_c/"), Some(("focus".into(), "ab-12_c".into())));
        assert_eq!(parse_link(&focus_link("x\"><y")), Some(("focus".into(), "xy".into())));
        assert_eq!(parse_link("notch://allow/s1"), Some(("allow".into(), "s1".into())));
        assert_eq!(parse_link("notch://other/s1"), None);
        assert_eq!(parse_link("notch://focus"), None);
    }
}
