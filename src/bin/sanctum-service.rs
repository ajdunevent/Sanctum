fn main() -> glib::ExitCode {
    sanctum::init_logging();
    sanctum::init_gettext();
    glib::set_application_name(&gettextrs::gettext("Sanctum"));
    sanctum::service::run()
}
