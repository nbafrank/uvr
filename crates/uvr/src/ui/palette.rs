//! Semantic styling for uvr output.
//!
//! Warm, inviting palette with magenta reserved for upgrades.

#![allow(dead_code)]

use console::{style, StyledObject};
use std::fmt::Display;

pub fn success<D: Display>(d: D) -> StyledObject<D> {
    style(d).green().bold()
}

pub fn fail<D: Display>(d: D) -> StyledObject<D> {
    style(d).red().bold()
}

pub fn warn<D: Display>(d: D) -> StyledObject<D> {
    style(d).yellow().bold()
}

pub fn info<D: Display>(d: D) -> StyledObject<D> {
    style(d).cyan().bold()
}

pub fn hint<D: Display>(d: D) -> StyledObject<D> {
    style(d).dim()
}

pub fn dim<D: Display>(d: D) -> StyledObject<D> {
    style(d).dim()
}

pub fn pkg<D: Display>(d: D) -> StyledObject<D> {
    style(d).cyan()
}

pub fn version<D: Display>(d: D) -> StyledObject<D> {
    style(d).dim()
}

pub fn added<D: Display>(d: D) -> StyledObject<D> {
    style(d).green().bold()
}

pub fn removed<D: Display>(d: D) -> StyledObject<D> {
    style(d).red().bold()
}

pub fn upgraded<D: Display>(d: D) -> StyledObject<D> {
    style(d).magenta().bold()
}

pub fn bold<D: Display>(d: D) -> StyledObject<D> {
    style(d).bold()
}

/// Format a duration in a friendly way.
pub fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.2}s")
    } else {
        let mins = (secs / 60.0).floor() as u64;
        let rem = secs - (mins as f64 * 60.0);
        format!("{mins}m{rem:.0}s")
    }
}

/// Format a byte count in a friendly way.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
