//! Build-time configuration.
//!
//! The rendezvous URL is fixed at compile time. Set `NUVIA_SERVER_URL` when
//! building a custom server; there is no server URL field in the GUI.

pub const SERVER_URL: &str = match option_env!("NUVIA_SERVER_URL") {
    Some(value) => value,
    None => "https://167.234.237.6.sslip.io",
};
