pub mod bearings;
// pub mod cluster; // TODO: step 1h — out while field.rs/cluster.rs/world.rs are reworked (step 1)
pub mod cluster_layout;
// pub mod cluster_policy; // TODO: step 1h
pub mod decay;
pub mod field;
pub mod focus;
pub mod overlap_physics;
pub mod stacking;
pub mod tiling;
pub mod trail;
pub mod viewport;
pub mod visual;
pub mod world;

// pub use cluster_policy::{ClusterFormationState, ClusterPolicy, tick_cluster_formation}; // TODO: step 1h
pub use decay::{DecayLevel, DecayPolicy, tick_decay};
pub use visual::{NodeVisual, VisualParams, build_visuals, build_visuals_in_view};
