mod assist_window;
mod assistant_button;
mod camera;
mod certificate;
mod controls_page;
mod dashboard;
mod details;
mod menu;
mod onboarding;
mod permission_help;
mod preferences_dialog;
pub mod rendering;
mod rotated;
mod scanner;
mod sensors_page;
mod window;

use gettextrs::gettext;
use gtk::gio;
use gtk::prelude::*;

pub use assist_window::AssistWindow;
pub use certificate::confirm_trust;
pub use onboarding::Onboarding;
pub use preferences_dialog::PreferencesDialog;
pub use window::Window;

use crate::application::Application;
use crate::ha;

/// Widgets are built only after the application is registered.
pub fn rendering_needs_restart() -> bool {
    rendering::needs_restart(app().settings())
}

pub fn app() -> Application {
    gio::Application::default()
        .and_downcast::<Application>()
        .expect("Sanctum application is running")
}

pub fn present_details(parent: &impl IsA<gtk::Widget>, entity: &crate::model::Entity) {
    details::present(parent, entity);
}

pub fn error_text(error: &ha::Error) -> String {
    match error {
        ha::Error::Network(detail) => {
            format!("{}\n{detail}", gettext("Could not reach the server"))
        }
        ha::Error::Auth(_) => gettext("Home Assistant rejected the sign-in"),
        ha::Error::Http { status: 404, body } if body.contains("mobile_app") => {
            gettext("The Mobile App integration is not enabled on this server")
        }
        ha::Error::Http { status, .. } => {
            format!("{}\nHTTP {status}", gettext("The server returned an error"))
        }
        ha::Error::Command { message, code } => {
            if message.is_empty() {
                code.clone()
            } else {
                message.clone()
            }
        }
        ha::Error::Protocol(_) => gettext("The server sent an unexpected response"),
        ha::Error::NotConnected => gettext("Not connected to Home Assistant"),
        ha::Error::InvalidUrl(_) => gettext("Enter a valid server address"),
        ha::Error::UntrustedCertificate(_) => gettext("The server's certificate is not trusted"),
        ha::Error::RegistrationGone => gettext("This device was removed from Home Assistant"),
        ha::Error::Storage(detail) => {
            format!("{}\n{detail}", gettext("Could not access the keyring"))
        }
    }
}

/// Icon-only buttons have no text for screen readers to announce, so their
/// tooltip is given as the accessible label.
pub(crate) fn label_by_tooltip(widget: &impl gtk::prelude::IsA<gtk::Widget>) {
    use gtk::prelude::*;
    if let Some(tooltip) = widget.tooltip_text() {
        widget
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label(&tooltip)]);
    }
}
