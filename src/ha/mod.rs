//! All Home Assistant networking; libsoup is used only here.

pub mod assist;
pub mod auth;
mod client;
pub mod credentials;
pub mod device;
pub mod discovery;
pub mod endpoints;
mod error;
pub mod history;
mod http;
pub mod registration;
pub mod registry;
mod server;
pub mod services;
pub mod states;
pub mod trust;
pub mod webhook;

pub use client::{Client, ConnectionState};
pub use credentials::{Auth, Credentials};
pub use error::{Error, Result};
pub use http::get as http_get;
pub use server::ServerUrl;
