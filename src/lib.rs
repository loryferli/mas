//! A multi-agent simulator for on-demand shared mobility on low-flow networks.
//!
//! The engine is deterministic and offline: a run is a pure function of its scenario, its seed
//! and the committed route cache, makes no network calls, and terminates on its own.

pub mod agents;
pub mod data;
pub mod events;
pub mod import;
pub mod incentives;
pub mod llm;
pub mod metrics;
pub mod network;
pub mod operator;
pub mod policy;
pub mod routing;
pub mod scenario;
pub mod sim;
pub mod sweep;
pub mod world;
