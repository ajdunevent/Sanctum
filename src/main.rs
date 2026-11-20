mod application;
mod furios;
mod launcher;
mod service_link;
mod ui;

// The window app reaches the shared modules through `crate::`.
pub(crate) use sanctum::{
    agent, background, config, control, ha, model, notify, server_settings, service, voice,
};

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::{gio, glib};

use self::application::Application;
use self::config::{MDI_RESOURCES_FILE, RESOURCES_FILE};

fn main() -> glib::ExitCode {
    sanctum::init_logging();
    sanctum::init_gettext();

    for file in [RESOURCES_FILE, MDI_RESOURCES_FILE] {
        let resources = gio::Resource::load(file).expect("Could not load resources");
        gio::resources_register(&resources);
    }

    glib::set_application_name(&gettext("Sanctum"));
    ui::rendering::prepare_environment();

    Application::new().run()
}
