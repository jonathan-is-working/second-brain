//! Second Brain: an autonomous, token-efficient personal knowledge and
//! productivity system. See README.md for the architecture overview.

pub mod actions;
pub mod brain;
pub mod briefing;
pub mod calendar;
pub mod config;
pub mod connect;
pub mod consolidate;
pub mod daemon;
pub mod dash;
pub mod dates;
pub mod db;
pub mod gcal_admin;
pub mod graph;
pub mod gtasks;
pub mod inference;
pub mod ingest;
pub mod journal;
pub mod memory;
pub mod notes;
pub mod planner;
pub mod retrieval;
pub mod synth;
pub mod synth_page;
pub mod tasks;
pub mod theme;
pub mod ui;
pub mod util;

pub use brain::Brain;
