//! A20 — the work board (Chapter 6, Factory): goals, projects and tasks the
//! operator plans and assigns, worked by role runs and team missions.

pub mod model;
pub mod runner;
pub mod service;
pub mod store;
pub mod tools;

pub use service::{BoardDeps, BoardService};
