use std::collections::HashSet;

use halley_core::cluster::ClusterId;
use halley_core::cluster::layout::ClusterWorkspaceLayoutKind;
use halley_core::field::{Field, NodeId, Vec2};

use super::{ClusterMetadata, ClusterSystem};

#[derive(Clone, Debug)]
pub struct CreationState {
    pub output: String,
    pub selected: HashSet<NodeId>,
    pub naming: bool,
    pub name_buffer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameInput {
    Backspace,
    Character(char),
}

impl ClusterSystem {
    pub fn creation(&self) -> Option<&CreationState> {
        self.creation.as_ref()
    }

    pub fn accepts_modal_input(&self) -> bool {
        self.creation.is_some()
    }

    pub fn begin_creation(&mut self, output: String) -> bool {
        if self.creation.is_some() {
            return false;
        }
        self.creation = Some(CreationState {
            output,
            selected: HashSet::new(),
            naming: false,
            name_buffer: String::new(),
        });
        true
    }

    pub fn cancel_creation(&mut self) -> bool {
        self.creation.take().is_some()
    }

    pub fn toggle_creation_member(&mut self, id: NodeId, output: &str) -> bool {
        let Some(creation) = self.creation.as_mut() else {
            return false;
        };
        if creation.output != output || creation.naming || self.registry.is_cluster_member(id) {
            return false;
        }
        if !creation.selected.remove(&id) {
            creation.selected.insert(id);
        }
        true
    }

    pub fn begin_naming(&mut self) -> bool {
        let Some(creation) = self.creation.as_mut() else {
            return false;
        };
        if creation.selected.is_empty() || creation.naming {
            return false;
        }
        creation.naming = true;
        true
    }

    pub fn edit_name(&mut self, input: NameInput) -> bool {
        let Some(creation) = self.creation.as_mut().filter(|creation| creation.naming) else {
            return false;
        };
        match input {
            NameInput::Backspace => {
                creation.name_buffer.pop();
            }
            NameInput::Character(ch)
                if !ch.is_control() && creation.name_buffer.chars().count() < 64 =>
            {
                creation.name_buffer.push(ch);
            }
            NameInput::Character(_) => return false,
        }
        true
    }

    pub fn finish_creation(&mut self, field: &mut Field) -> Result<ClusterId, String> {
        let Some(creation) = self.creation.take() else {
            return Err("cluster creation mode is not active".into());
        };
        if !creation.naming || creation.selected.is_empty() {
            self.creation = Some(creation);
            return Err("select at least one window and enter a name first".into());
        }
        let mut members = creation.selected.into_iter().collect::<Vec<_>>();
        members.sort_by_key(|id| id.as_u64());
        let positions = members
            .iter()
            .filter_map(|id| field.node(*id).map(|node| node.pos))
            .collect::<Vec<_>>();
        if positions.len() != members.len() {
            return Err("a selected window disappeared before the cluster was created".into());
        }
        let id = self
            .registry
            .create_cluster(field, members)
            .map_err(|error| format!("could not create cluster: {error:?}"))?;
        let count = positions.len() as f32;
        let sum = positions
            .into_iter()
            .fold(Vec2 { x: 0.0, y: 0.0 }, |sum, position| Vec2 {
                x: sum.x + position.x,
                y: sum.y + position.y,
            });
        let core_position = Vec2 {
            x: sum.x / count,
            y: sum.y / count,
        };
        let slots = self.slots.entry(creation.output.clone()).or_default();
        if slots.len() >= 10 {
            self.registry.dissolve_cluster(field, id);
            return Err(format!(
                "output {} already has all 10 cluster slots assigned",
                creation.output
            ));
        }
        let slot = slots.len() + 1;
        slots.push(id);
        let name = creation.name_buffer.trim();
        self.metadata.insert(
            id,
            ClusterMetadata {
                name: if name.is_empty() {
                    format!("Cluster {slot}")
                } else {
                    name.to_string()
                },
                output: creation.output,
                layout: match self.config.default_layout {
                    halley_config::ClusterLayout::Tiling => ClusterWorkspaceLayoutKind::Tiling,
                    halley_config::ClusterLayout::Stacking => ClusterWorkspaceLayoutKind::Stacking,
                },
                core: None,
                core_position,
            },
        );
        if let Some(cluster) = self.registry.cluster_mut(id) {
            cluster.set_collapsed(true);
        }
        Ok(id)
    }
}
