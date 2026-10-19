//! Native workspace preparation and descriptor based file access.
//!
//! The application dispatches its worker CLI arguments to [`prepare::run`] and
//! [`files::run`]. These functions use stdout for worker protocol responses.

pub mod files;
pub mod prepare;
