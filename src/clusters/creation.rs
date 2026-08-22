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
    pub(crate) name_repeat: Option<NameRepeat>,
    pub(crate) draft: Option<DraftProposal>,
}

#[derive(Clone, Debug)]
pub(crate) struct DraftProposal {
    id: u64,
    app_launches: Vec<halley_ipc::ClusterDraftAppLaunch>,
}

#[derive(Clone, Debug)]
pub(crate) struct DraftBuild {
    pub id: u64,
    pub output: String,
    pub name: String,
    pub selected: HashSet<NodeId>,
    pub staged: HashSet<NodeId>,
    pub matched_app_ids: Vec<String>,
    pub app_launches: Vec<halley_ipc::ClusterDraftAppLaunch>,
    pub started_at: std::time::Duration,
}

pub(crate) struct DraftConfirmation {
    pub id: u64,
    pub launches: Vec<halley_ipc::ClusterDraftAppLaunch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameInput {
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    Character(char),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NameRepeat {
    keycode: u32,
    input: NameInput,
    next_at: std::time::Duration,
    interval: std::time::Duration,
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

fn next_default_name<'a>(
    output: &str,
    metadata: impl Iterator<Item = &'a ClusterMetadata>,
) -> String {
    let used = metadata
        .filter(|cluster| cluster.output == output)
        .filter_map(|cluster| cluster.name.strip_prefix("Cluster "))
        .filter_map(|slot| slot.parse::<usize>().ok())
        .collect::<HashSet<_>>();
    let slot = (1..).find(|slot| !used.contains(slot)).unwrap_or(1);
    format!("Cluster {slot}")
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
            name_repeat: None,
            draft: None,
        });
        true
    }

    pub fn cancel_creation(&mut self) -> bool {
        self.creation.take().is_some()
    }

    pub(crate) fn begin_draft(
        &mut self,
        field: &Field,
        output: String,
        name_hint: Option<String>,
        running_nodes: Vec<NodeId>,
        app_launches: Vec<halley_ipc::ClusterDraftAppLaunch>,
    ) -> Result<u64, String> {
        if self.creation.is_some() || self.pending_draft.is_some() {
            return Err("another cluster draft or creation modal is already active".into());
        }
        let mut selected = HashSet::new();
        for id in running_nodes {
            if field.node(id).is_none() {
                return Err(format!("node {} was not found", id.as_u64()));
            }
            if self.registry.is_cluster_member(id) {
                return Err(format!("node {} already belongs to a cluster", id.as_u64()));
            }
            selected.insert(id);
        }
        if selected.is_empty() && app_launches.is_empty() {
            return Err("a cluster draft needs at least one running node or app".into());
        }
        let default_name = next_default_name(&output, self.metadata.values());
        let name_buffer = name_hint
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or(default_name);
        let id = self.next_draft_id;
        self.next_draft_id = self.next_draft_id.saturating_add(1);
        let caret = char_len(&name_buffer);
        self.creation = Some(CreationState {
            output,
            selected,
            naming: true,
            name_buffer,
            caret_char: caret,
            selection_anchor_char: 0,
            selection_focus_char: caret,
            scroll_char: 0,
            dragging_selection: false,
            name_repeat: None,
            draft: Some(DraftProposal { id, app_launches }),
        });
        Ok(id)
    }

    pub(crate) fn creation_draft_id(&self) -> Option<u64> {
        self.creation.as_ref()?.draft.as_ref().map(|draft| draft.id)
    }

    pub(crate) fn confirm_draft(&mut self, now: std::time::Duration) -> Option<DraftConfirmation> {
        let creation = self.creation.as_ref()?;
        let draft = creation.draft.as_ref()?;
        if draft.app_launches.is_empty() {
            return None;
        }
        let creation = self.creation.take()?;
        let draft = creation.draft?;
        let confirmation = DraftConfirmation {
            id: draft.id,
            launches: draft.app_launches.clone(),
        };
        self.pending_draft = Some(DraftBuild {
            id: draft.id,
            output: creation.output,
            name: creation.name_buffer,
            selected: creation.selected,
            staged: HashSet::new(),
            matched_app_ids: Vec::new(),
            app_launches: draft.app_launches,
            started_at: now,
        });
        Some(confirmation)
    }

    pub(crate) fn match_pending_draft(&mut self, id: NodeId, app_id: &str) -> Option<bool> {
        let build = self.pending_draft.as_mut()?;
        let actual = normalize_app_id(app_id);
        if actual.is_empty() {
            return None;
        }
        let expected_count = build
            .app_launches
            .iter()
            .map(|launch| normalize_app_id(&launch.app_id))
            .filter(|candidate| app_ids_match(candidate, &actual))
            .count();
        let matched_count = build
            .matched_app_ids
            .iter()
            .filter(|candidate| app_ids_match(candidate, &actual))
            .count();
        if expected_count <= matched_count {
            return None;
        }
        build.matched_app_ids.push(actual);
        build.selected.insert(id);
        build.staged.insert(id);
        Some(build.matched_app_ids.len() == build.app_launches.len())
    }

    pub(crate) fn ready_draft_members(&self) -> Option<Vec<NodeId>> {
        let build = self.pending_draft.as_ref()?;
        (build.matched_app_ids.len() == build.app_launches.len())
            .then(|| build.selected.iter().copied().collect())
    }

    pub(crate) fn pending_draft_output(&self) -> Option<&str> {
        self.pending_draft
            .as_ref()
            .map(|build| build.output.as_str())
    }

    pub(crate) fn finish_pending_draft(
        &mut self,
        field: &mut Field,
    ) -> Result<(ClusterId, DraftBuild), String> {
        let build = self
            .pending_draft
            .take()
            .ok_or_else(|| "no cluster draft is ready".to_string())?;
        self.creation = Some(CreationState {
            output: build.output.clone(),
            selected: build.selected.clone(),
            naming: true,
            name_buffer: build.name.clone(),
            caret_char: char_len(&build.name),
            selection_anchor_char: 0,
            selection_focus_char: char_len(&build.name),
            scroll_char: 0,
            dragging_selection: false,
            name_repeat: None,
            draft: None,
        });
        match self.finish_creation(field) {
            Ok(id) => Ok((id, build)),
            Err(error) => {
                self.creation = None;
                self.pending_draft = Some(build);
                Err(error)
            }
        }
    }

    pub(crate) fn take_timed_out_draft(&mut self, now: std::time::Duration) -> Option<DraftBuild> {
        self.pending_draft.as_ref().filter(|build| {
            now.saturating_sub(build.started_at) >= std::time::Duration::from_secs(30)
        })?;
        self.pending_draft.take()
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
            creation.name_repeat = None;
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
        let Some(output) = self
            .creation
            .as_ref()
            .filter(|creation| !creation.selected.is_empty() && !creation.naming)
            .map(|creation| creation.output.clone())
        else {
            return false;
        };
        let default_name = next_default_name(&output, self.metadata.values());
        let Some(creation) = self.creation.as_mut() else {
            return false;
        };
        if creation.name_buffer.is_empty() {
            creation.name_buffer = default_name;
        }
        creation.naming = true;
        creation.caret_char = char_len(&creation.name_buffer);
        creation.selection_anchor_char = 0;
        creation.selection_focus_char = creation.caret_char;
        creation.scroll_char = 0;
        creation.dragging_selection = false;
        creation.name_repeat = None;
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

    pub fn start_name_repeat(
        &mut self,
        keycode: u32,
        input: NameInput,
        now: std::time::Duration,
        delay_ms: i32,
        rate: i32,
    ) {
        let Some(creation) = self.creation.as_mut().filter(|creation| creation.naming) else {
            return;
        };
        if rate <= 0 {
            creation.name_repeat = None;
            return;
        }
        let interval_ms = (1_000u64 / u64::try_from(rate).unwrap_or(1).max(1)).max(1);
        creation.name_repeat = Some(NameRepeat {
            keycode,
            input,
            next_at: now.saturating_add(std::time::Duration::from_millis(
                u64::try_from(delay_ms).unwrap_or(0),
            )),
            interval: std::time::Duration::from_millis(interval_ms),
        });
    }

    pub fn stop_name_repeat(&mut self, keycode: u32) {
        if let Some(creation) = self.creation.as_mut()
            && creation
                .name_repeat
                .is_some_and(|repeat| repeat.keycode == keycode)
        {
            creation.name_repeat = None;
        }
    }

    pub fn repeat_name_input_if_due(&mut self, now: std::time::Duration) -> bool {
        let Some(repeat) = self
            .creation
            .as_ref()
            .and_then(|creation| creation.name_repeat)
            .filter(|repeat| now >= repeat.next_at)
        else {
            return false;
        };
        let handled = self.edit_name(repeat.input);
        if let Some(creation) = self.creation.as_mut()
            && let Some(state) = creation.name_repeat.as_mut()
            && state.keycode == repeat.keycode
        {
            state.next_at = state.next_at.saturating_add(state.interval);
        }
        handled
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
        let core = self
            .registry
            .collapse_cluster(field, id)
            .ok_or_else(|| "could not create the cluster core".to_string())?;
        if let Some(metadata) = self.metadata.get_mut(&id) {
            metadata.core = Some(core);
        }
        Ok(id)
    }
}

fn normalize_app_id(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    value.strip_suffix(".desktop").unwrap_or(&value).to_string()
}

fn app_ids_match(expected: &str, actual: &str) -> bool {
    expected == actual
        || expected.rsplit('.').next() == Some(actual)
        || actual.rsplit('.').next() == Some(expected)
        || expected.rsplit('.').next() == actual.rsplit('.').next()
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
    fn draft_launches_wait_for_name_confirmation_and_match_desktop_ids() {
        let mut system = system();
        let field = Field::new();
        let id = system
            .begin_draft(
                &field,
                "DP-1".into(),
                Some("Work".into()),
                Vec::new(),
                vec![halley_ipc::ClusterDraftAppLaunch {
                    app_id: "org.mozilla.firefox.desktop".into(),
                    command: "firefox".into(),
                }],
            )
            .unwrap();
        assert_eq!(system.creation_draft_id(), Some(id));
        assert!(system.pending_draft.is_none());

        let confirmation = system
            .confirm_draft(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(confirmation.launches[0].command, "firefox");
        assert_eq!(
            system.match_pending_draft(NodeId::new(9), "firefox"),
            Some(true)
        );
    }

    #[test]
    fn pending_draft_times_out_after_thirty_seconds() {
        let mut system = system();
        let field = Field::new();
        system
            .begin_draft(
                &field,
                "DP-1".into(),
                None,
                Vec::new(),
                vec![halley_ipc::ClusterDraftAppLaunch {
                    app_id: "missing".into(),
                    command: "missing".into(),
                }],
            )
            .unwrap();
        system
            .confirm_draft(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            system
                .take_timed_out_draft(std::time::Duration::from_secs(34))
                .is_none()
        );
        assert!(
            system
                .take_timed_out_draft(std::time::Duration::from_secs(35))
                .is_some()
        );
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
        assert_eq!(
            system.creation().expect("creation").name_buffer,
            "Cluster 1"
        );
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

    #[test]
    fn finishing_creation_keeps_a_real_focusable_core_identity() {
        let mut field = Field::new();
        let first =
            field.spawn_surface("first", Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 100.0, y: 80.0 });
        let second = field.spawn_surface(
            "second",
            Vec2 { x: 120.0, y: 0.0 },
            Vec2 { x: 100.0, y: 80.0 },
        );
        let mut system = system();
        system.begin_creation("DP-1".into());
        system.toggle_creation_member(first, "DP-1");
        system.toggle_creation_member(second, "DP-1");
        system.begin_naming();

        let cluster = system.finish_creation(&mut field).expect("cluster");
        let core = system.core_node(cluster).expect("core identity");

        assert_eq!(
            field.node(core).expect("core node").state,
            halley_core::field::NodeState::Core
        );
        assert_eq!(system.metadata(cluster).expect("metadata").core, Some(core));
        assert_eq!(system.collapsed_core_landmarks()[0].1, core);
    }

    #[test]
    fn configured_repeat_waits_for_delay_and_advances_at_the_configured_rate() {
        let mut system = system();
        assert!(system.begin_creation("DP-1".into()));
        let creation = system.creation.as_mut().expect("creation");
        creation.selected.insert(NodeId::new(1));
        creation.name_buffer = "abcd".into();
        assert!(system.begin_naming());
        assert!(system.edit_name(NameInput::Backspace));
        system.start_name_repeat(14, NameInput::Backspace, std::time::Duration::ZERO, 300, 20);

        assert!(!system.repeat_name_input_if_due(std::time::Duration::from_millis(299)));
        assert_eq!(system.creation().expect("creation").name_buffer, "");
        assert!(system.repeat_name_input_if_due(std::time::Duration::from_millis(300)));
        assert!(!system.repeat_name_input_if_due(std::time::Duration::from_millis(349)));
        assert!(system.repeat_name_input_if_due(std::time::Duration::from_millis(350)));
        system.stop_name_repeat(14);
        assert!(!system.repeat_name_input_if_due(std::time::Duration::from_secs(2)));
    }

    #[test]
    fn naming_prefills_and_selects_the_first_unused_default_name() {
        let mut system = system();
        system.metadata.insert(
            ClusterId::new(1),
            ClusterMetadata {
                name: "Cluster 1".into(),
                output: "DP-1".into(),
                layout: ClusterWorkspaceLayoutKind::Tiling,
                core: None,
                core_position: Vec2 { x: 0.0, y: 0.0 },
            },
        );
        system.metadata.insert(
            ClusterId::new(2),
            ClusterMetadata {
                name: "Cluster 3".into(),
                output: "DP-1".into(),
                layout: ClusterWorkspaceLayoutKind::Tiling,
                core: None,
                core_position: Vec2 { x: 0.0, y: 0.0 },
            },
        );
        assert!(system.begin_creation("DP-1".into()));
        system
            .creation
            .as_mut()
            .expect("creation")
            .selected
            .insert(NodeId::new(7));

        assert!(system.begin_naming());
        let creation = system.creation().expect("creation");
        assert_eq!(creation.name_buffer, "Cluster 2");
        assert_eq!(
            selection_range(creation),
            Some((0, "Cluster 2".chars().count()))
        );
    }
}
