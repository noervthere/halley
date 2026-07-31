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
    pub caret_char: usize,
    pub selection_anchor_char: usize,
    pub selection_focus_char: usize,
    pub scroll_char: usize,
    pub dragging_selection: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameInput {
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    Character(char),
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

fn char_to_byte(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn selection_range(creation: &CreationState) -> Option<(usize, usize)> {
    (creation.selection_anchor_char != creation.selection_focus_char).then(|| {
        (
            creation
                .selection_anchor_char
                .min(creation.selection_focus_char),
            creation
                .selection_anchor_char
                .max(creation.selection_focus_char),
        )
    })
}

fn replace_selection(creation: &mut CreationState, replacement: &str) {
    let (start, end) =
        selection_range(creation).unwrap_or((creation.caret_char, creation.caret_char));
    let start_byte = char_to_byte(&creation.name_buffer, start);
    let end_byte = char_to_byte(&creation.name_buffer, end);
    creation
        .name_buffer
        .replace_range(start_byte..end_byte, replacement);
    creation.caret_char = start + char_len(replacement);
    creation.selection_anchor_char = creation.caret_char;
    creation.selection_focus_char = creation.caret_char;
    creation.scroll_char = creation.scroll_char.min(creation.caret_char);
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
            caret_char: 0,
            selection_anchor_char: 0,
            selection_focus_char: 0,
            scroll_char: 0,
            dragging_selection: false,
        });
        true
    }

    pub fn cancel_creation(&mut self) -> bool {
        self.creation.take().is_some()
    }

    /// Escape backs out of the naming page before it exits selection mode,
    /// matching old Halley's two-stage modal.
    pub fn back_or_cancel_creation(&mut self) -> bool {
        let Some(creation) = self.creation.as_mut() else {
            return false;
        };
        if creation.naming {
            creation.naming = false;
            creation.dragging_selection = false;
            return true;
        }
        self.creation = None;
        true
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
        creation.caret_char = char_len(&creation.name_buffer);
        creation.selection_anchor_char = creation.caret_char;
        creation.selection_focus_char = creation.caret_char;
        creation.scroll_char = 0;
        creation.dragging_selection = false;
        true
    }

    pub fn edit_name(&mut self, input: NameInput) -> bool {
        let Some(creation) = self.creation.as_mut().filter(|creation| creation.naming) else {
            return false;
        };
        match input {
            NameInput::Backspace => {
                if selection_range(creation).is_some() {
                    replace_selection(creation, "");
                } else if creation.caret_char > 0 {
                    let start = creation.caret_char - 1;
                    let start_byte = char_to_byte(&creation.name_buffer, start);
                    let end_byte = char_to_byte(&creation.name_buffer, creation.caret_char);
                    creation.name_buffer.replace_range(start_byte..end_byte, "");
                    creation.caret_char = start;
                    creation.selection_anchor_char = start;
                    creation.selection_focus_char = start;
                    creation.scroll_char = creation.scroll_char.min(start);
                }
            }
            NameInput::Delete => {
                if selection_range(creation).is_some() {
                    replace_selection(creation, "");
                } else if creation.caret_char < char_len(&creation.name_buffer) {
                    let start_byte = char_to_byte(&creation.name_buffer, creation.caret_char);
                    let end_byte = char_to_byte(&creation.name_buffer, creation.caret_char + 1);
                    creation.name_buffer.replace_range(start_byte..end_byte, "");
                    creation.selection_anchor_char = creation.caret_char;
                    creation.selection_focus_char = creation.caret_char;
                }
            }
            NameInput::MoveLeft => {
                creation.caret_char = selection_range(creation)
                    .map_or_else(|| creation.caret_char.saturating_sub(1), |(start, _)| start);
                creation.selection_anchor_char = creation.caret_char;
                creation.selection_focus_char = creation.caret_char;
            }
            NameInput::MoveRight => {
                creation.caret_char = selection_range(creation).map_or_else(
                    || (creation.caret_char + 1).min(char_len(&creation.name_buffer)),
                    |(_, end)| end,
                );
                creation.selection_anchor_char = creation.caret_char;
                creation.selection_focus_char = creation.caret_char;
            }
            NameInput::Character(ch)
                if !ch.is_control() && creation.name_buffer.chars().count() < 64 =>
            {
                replace_selection(creation, ch.encode_utf8(&mut [0; 4]));
            }
            NameInput::Character(_) => return false,
        }
        true
    }

    pub fn begin_name_selection(&mut self, caret_char: usize) -> bool {
        let Some(creation) = self.creation.as_mut().filter(|creation| creation.naming) else {
            return false;
        };
        creation.caret_char = caret_char.min(char_len(&creation.name_buffer));
        creation.selection_anchor_char = creation.caret_char;
        creation.selection_focus_char = creation.caret_char;
        creation.dragging_selection = true;
        true
    }

    pub fn drag_name_selection(&mut self, caret_char: usize) -> bool {
        let Some(creation) = self
            .creation
            .as_mut()
            .filter(|creation| creation.naming && creation.dragging_selection)
        else {
            return false;
        };
        creation.caret_char = caret_char.min(char_len(&creation.name_buffer));
        creation.selection_focus_char = creation.caret_char;
        true
    }

    pub fn end_name_selection(&mut self) -> bool {
        let Some(creation) = self
            .creation
            .as_mut()
            .filter(|creation| creation.dragging_selection)
        else {
            return false;
        };
        creation.dragging_selection = false;
        true
    }

    pub fn set_name_scroll(&mut self, scroll_char: usize) -> bool {
        let Some(creation) = self.creation.as_mut().filter(|creation| creation.naming) else {
            return false;
        };
        let scroll_char = scroll_char.min(char_len(&creation.name_buffer));
        if creation.scroll_char == scroll_char {
            return false;
        }
        creation.scroll_char = scroll_char;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> ClusterSystem {
        ClusterSystem::new(
            halley_config::Clusters::default(),
            halley_config::ClusterAnimation::default(),
        )
    }

    #[test]
    fn escape_from_naming_returns_to_the_existing_selection() {
        let mut system = system();
        let selected = NodeId::new(7);
        assert!(system.begin_creation("DP-1".into()));
        system
            .creation
            .as_mut()
            .expect("creation")
            .selected
            .insert(selected);
        assert!(system.begin_naming());
        assert!(system.back_or_cancel_creation());

        let creation = system.creation().expect("selection remains active");
        assert!(!creation.naming);
        assert!(creation.selected.contains(&selected));
        assert!(system.back_or_cancel_creation());
        assert!(system.creation().is_none());
    }

    #[test]
    fn editor_moves_and_deletes_by_unicode_character() {
        let mut system = system();
        assert!(system.begin_creation("DP-1".into()));
        system
            .creation
            .as_mut()
            .expect("creation")
            .selected
            .insert(NodeId::new(1));
        assert!(system.begin_naming());
        for ch in "aλ界".chars() {
            assert!(system.edit_name(NameInput::Character(ch)));
        }
        assert!(system.edit_name(NameInput::MoveLeft));
        assert!(system.edit_name(NameInput::Backspace));
        assert_eq!(system.creation().expect("creation").name_buffer, "a界");
        assert!(system.edit_name(NameInput::Delete));
        assert_eq!(system.creation().expect("creation").name_buffer, "a");
    }

    #[test]
    fn typed_character_replaces_the_pointer_selection() {
        let mut system = system();
        assert!(system.begin_creation("DP-1".into()));
        let creation = system.creation.as_mut().expect("creation");
        creation.selected.insert(NodeId::new(1));
        creation.name_buffer = "cluster".into();
        assert!(system.begin_naming());
        assert!(system.begin_name_selection(1));
        assert!(system.drag_name_selection(6));
        assert!(system.end_name_selection());
        assert!(system.edit_name(NameInput::Character('X')));
        assert_eq!(system.creation().expect("creation").name_buffer, "cXr");
    }
}
