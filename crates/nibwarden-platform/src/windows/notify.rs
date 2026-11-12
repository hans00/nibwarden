//! A toast through the Windows notification service, best effort.
//!
//! Toasts need an application identity (an AUMID). A program that is not installed with a
//! shortcut that sets one has none, so this borrows Windows PowerShell's, which Windows
//! always knows; the toast then carries PowerShell's name and icon. Without the notification
//! service (a server, a Wine) nothing is shown and nothing fails.

use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    core::HSTRING,
};

const POWERSHELL_AUMID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

pub fn notify(summary: &str, body: &str) {
    let xml = toast_xml(summary, body);
    // On a thread of its own: WinRT activation may need to initialise COM there.
    std::thread::spawn(move || {
        if let Err(e) = show(&xml) {
            tracing::debug!(error = %e, "cannot show a notification");
        }
    });
}

fn show(xml: &str) -> windows::core::Result<()> {
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(POWERSHELL_AUMID))?
        .Show(&toast)
}

fn toast_xml(summary: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text>\
         </binding></visual></toast>",
        escape(summary),
        escape(body)
    )
}

fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&apos;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_cannot_add_markup() {
        let xml = super::toast_xml("a <b>", "x & \"y\"");
        assert!(xml.contains("<text>a &lt;b&gt;</text>"));
        assert!(xml.contains("<text>x &amp; &quot;y&quot;</text>"));
    }
}
