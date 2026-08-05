//! AT-SPI bridge for compositor-owned shell UI.

use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, RwLock,
};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Live, Node,
    NodeId, Rect, Role, Toggled, Tree, TreeId, TreeUpdate,
};
use accesskit_unix::Adapter;
use focaldesk_ui::{accessibility::AccessibleRole, types::ElementId, uitree::UiTree};

const ROOT_ID: NodeId = NodeId(1);
const NODE_ID_OFFSET: u64 = 2;
const DIALOG_TAG: u64 = 1 << 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleDialogButton {
    pub label: String,
    /// x, y, width, height in output-local logical coordinates.
    pub bounds: [i32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleDialog {
    pub id: u32,
    pub title: String,
    pub message: String,
    pub modal: bool,
    pub bounds: [i32; 4],
    pub buttons: Vec<AccessibleDialogButton>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibilityAction {
    Focus(ElementId),
    Blur(ElementId),
    Click(ElementId),
    FocusDialogButton { dialog: u32, button: usize },
    BlurDialogButton { dialog: u32, button: usize },
    ClickDialogButton { dialog: u32, button: usize },
}

#[derive(Clone)]
struct Activation {
    latest: Arc<RwLock<TreeUpdate>>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.latest.read().ok().map(|update| update.clone())
    }
}

struct Actions {
    sender: Sender<AccessibilityAction>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        let action = if let Some((dialog, button)) = dialog_button(request.target_node) {
            match request.action {
                Action::Focus => AccessibilityAction::FocusDialogButton { dialog, button },
                Action::Blur => AccessibilityAction::BlurDialogButton { dialog, button },
                Action::Click => AccessibilityAction::ClickDialogButton { dialog, button },
                _ => return,
            }
        } else {
            let Some(id) = element_id(request.target_node) else {
                return;
            };
            match request.action {
                Action::Focus => AccessibilityAction::Focus(id),
                Action::Blur => AccessibilityAction::Blur(id),
                Action::Click => AccessibilityAction::Click(id),
                _ => return,
            }
        };
        let _ = self.sender.send(action);
    }
}

struct Deactivation;

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {}
}

/// Live platform adapter plus its compositor-thread action queue.
pub struct AccessibilityBridge {
    adapter: Adapter,
    latest: Arc<RwLock<TreeUpdate>>,
    actions: Receiver<AccessibilityAction>,
    dialog_focus: Option<NodeId>,
}

impl AccessibilityBridge {
    pub fn new() -> Self {
        let initial = tree_update(&UiTree::default(), None, None);
        let latest = Arc::new(RwLock::new(initial));
        let (sender, actions) = mpsc::channel();
        let adapter = Adapter::new(
            Activation {
                latest: Arc::clone(&latest),
            },
            Actions { sender },
            Deactivation,
        );
        Self {
            adapter,
            latest,
            actions,
            dialog_focus: None,
        }
    }

    /// Publish a complete atomic shell-tree update. AccessKit diffs it and
    /// emits the appropriate AT-SPI focus, property, and live-region events.
    pub fn update(&mut self, ui: &UiTree, dialog: Option<&AccessibleDialog>) {
        if self.dialog_focus.is_some_and(|focus| {
            dialog_button(focus).is_none_or(|(id, _)| dialog.is_none_or(|dialog| dialog.id != id))
        }) {
            self.dialog_focus = None;
        }
        let update = tree_update(ui, dialog, self.dialog_focus);
        if self.latest.read().is_ok_and(|latest| *latest == update) {
            return;
        }
        if let Ok(mut latest) = self.latest.write() {
            *latest = update.clone();
        }
        self.adapter
            .update_window_focus_state(ui.focused.is_some() || dialog.is_some());
        self.adapter.update_if_active(|| update);
    }

    pub fn focus_dialog_button(&mut self, dialog: u32, button: usize) {
        self.dialog_focus = Some(dialog_button_id(dialog, button));
    }

    pub fn blur_dialog_button(&mut self, dialog: u32, button: usize) {
        if self.dialog_focus == Some(dialog_button_id(dialog, button)) {
            self.dialog_focus = None;
        }
    }

    pub fn pending_actions(&self) -> impl Iterator<Item = AccessibilityAction> + '_ {
        self.actions.try_iter()
    }
}

impl Default for AccessibilityBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn node_id(id: ElementId) -> NodeId {
    NodeId(u64::from(id) + NODE_ID_OFFSET)
}

fn element_id(id: NodeId) -> Option<ElementId> {
    let raw = u64::from(id);
    if raw & DIALOG_TAG != 0 {
        return None;
    }
    raw.checked_sub(NODE_ID_OFFSET)?.try_into().ok()
}

fn dialog_id(id: u32) -> NodeId {
    NodeId(DIALOG_TAG | (u64::from(id) << 32))
}

fn dialog_button_id(id: u32, button: usize) -> NodeId {
    NodeId(DIALOG_TAG | (u64::from(id) << 32) | (button as u64 + 1))
}

fn dialog_button(id: NodeId) -> Option<(u32, usize)> {
    let raw = u64::from(id);
    let child = raw & 0xffff_ffff;
    if raw & DIALOG_TAG == 0 || child == 0 {
        return None;
    }
    let dialog = ((raw & !DIALOG_TAG) >> 32) as u32;
    Some((dialog, (child - 1).try_into().ok()?))
}

fn role(role: AccessibleRole) -> Role {
    match role {
        AccessibleRole::Button | AccessibleRole::ToggleButton => Role::Button,
        AccessibleRole::Slider => Role::Slider,
        AccessibleRole::Menu => Role::Menu,
        AccessibleRole::MenuItem => Role::MenuItem,
        AccessibleRole::Dialog => Role::Dialog,
        AccessibleRole::Status => Role::Status,
        AccessibleRole::Label => Role::Label,
        AccessibleRole::List => Role::List,
        AccessibleRole::ListItem => Role::ListItem,
        AccessibleRole::Tab => Role::Tab,
    }
}

fn tree_update(
    ui: &UiTree,
    dialog: Option<&AccessibleDialog>,
    requested_dialog_focus: Option<NodeId>,
) -> TreeUpdate {
    let chrome_children: Vec<NodeId> = ui
        .accessible_elements()
        .map(|element| node_id(element.id))
        .collect();
    let children = match dialog {
        Some(dialog) if dialog.modal => vec![dialog_id(dialog.id)],
        Some(dialog) => chrome_children
            .iter()
            .copied()
            .chain([dialog_id(dialog.id)])
            .collect(),
        None => chrome_children.clone(),
    };
    let mut root = Node::new(Role::Window);
    root.set_label("FocalDesk shell");
    root.set_children(children);

    let mut nodes = Vec::with_capacity(chrome_children.len() + 3);
    nodes.push((ROOT_ID, root));
    if !dialog.is_some_and(|dialog| dialog.modal) {
        append_chrome_nodes(&mut nodes, ui);
    }
    if let Some(dialog) = dialog {
        append_dialog_nodes(&mut nodes, dialog);
    }

    let dialog_focus = dialog.map(|dialog| {
        requested_dialog_focus
            .filter(|focus| {
                dialog_button(*focus)
                    .is_some_and(|(id, button)| id == dialog.id && button < dialog.buttons.len())
            })
            .unwrap_or_else(|| {
                if dialog.buttons.is_empty() {
                    dialog_id(dialog.id)
                } else {
                    dialog_button_id(dialog.id, 0)
                }
            })
    });
    let mut tree = Tree::new(ROOT_ID);
    tree.toolkit_name = Some("FocalDesk UI".into());
    tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
    TreeUpdate {
        nodes,
        tree: Some(tree),
        tree_id: TreeId::ROOT,
        focus: dialog_focus
            .or_else(|| ui.focused.map(node_id))
            .unwrap_or(ROOT_ID),
    }
}

fn append_chrome_nodes(nodes: &mut Vec<(NodeId, Node)>, ui: &UiTree) {
    for element in ui.accessible_elements() {
        let info = element.accessible.as_ref().expect("filtered above");
        let mut node = Node::new(role(info.role));
        node.set_label(info.name.clone());
        if let Some(description) = &info.description {
            node.set_description(description.clone());
        }
        if let Some(value) = &info.value_text {
            node.set_value(value.clone());
        }
        if let Some(shortcut) = &info.key_shortcut {
            node.set_keyboard_shortcut(shortcut.clone());
        }
        if let Some(checked) = info.checked {
            node.set_toggled(Toggled::from(checked));
        }
        if let Some(expanded) = info.expanded {
            node.set_expanded(expanded);
        }
        if info.live {
            node.set_live(Live::Polite);
            node.set_live_atomic();
        }
        if !element.enabled {
            node.set_disabled();
        }
        if matches!(info.role, AccessibleRole::Tab | AccessibleRole::ListItem) {
            node.set_selected(element.selected);
        }
        node.set_bounds(Rect {
            x0: f64::from(element.bounds.x),
            y0: f64::from(element.bounds.y),
            x1: f64::from(element.bounds.x + element.bounds.w),
            y1: f64::from(element.bounds.y + element.bounds.h),
        });
        if element.is_accessibility_focusable() {
            node.add_action(Action::Focus);
            node.add_action(Action::Blur);
            node.add_action(Action::Click);
        }
        nodes.push((node_id(element.id), node));
    }
}

fn append_dialog_nodes(nodes: &mut Vec<(NodeId, Node)>, dialog: &AccessibleDialog) {
    let button_ids: Vec<_> = (0..dialog.buttons.len())
        .map(|index| dialog_button_id(dialog.id, index))
        .collect();
    let mut dialog_node = Node::new(Role::Dialog);
    dialog_node.set_label(dialog.title.clone());
    dialog_node.set_description(dialog.message.clone());
    dialog_node.set_bounds(rect(dialog.bounds));
    dialog_node.set_children(button_ids.clone());
    if dialog.modal {
        dialog_node.set_modal();
    }
    nodes.push((dialog_id(dialog.id), dialog_node));

    for (index, button) in dialog.buttons.iter().enumerate() {
        let mut node = Node::new(Role::Button);
        node.set_label(button.label.clone());
        node.set_bounds(rect(button.bounds));
        node.add_action(Action::Focus);
        node.add_action(Action::Blur);
        node.add_action(Action::Click);
        nodes.push((button_ids[index], node));
    }
}

fn rect([x, y, width, height]: [i32; 4]) -> Rect {
    Rect {
        x0: f64::from(x),
        y0: f64::from(y),
        x1: f64::from(x + width),
        y1: f64::from(y + height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focaldesk_ui::{
        accessibility::AccessibleInfo,
        atlas::IconId,
        element::UiElement,
        types::{UiAction, UiElementKind},
    };

    #[test]
    fn maps_visible_semantics_focus_and_actions() {
        let mut button = UiElement::new(
            7,
            Default::default(),
            UiElementKind::SidebarButton,
            Some(IconId::Launcher),
            Some(UiAction::Custom(7)),
        )
        .with_accessible(AccessibleInfo::new(AccessibleRole::Button, "Launcher"));
        button.bounds.w = 40;
        button.bounds.h = 40;
        let mut tree = UiTree {
            elements: vec![button],
            ..UiTree::default()
        };
        tree.set_focus(7);

        let update = tree_update(&tree, None, None);
        assert_eq!(update.nodes.len(), 2);
        assert_eq!(update.focus, node_id(7));
        assert!(update.nodes[1].1.supports_action(Action::Click));
        assert_eq!(update.nodes[1].1.label(), Some("Launcher"));
    }

    #[test]
    fn modal_dialog_hides_chrome_and_focuses_first_action() {
        let dialog = AccessibleDialog {
            id: 9,
            title: "Delete workspace?".into(),
            message: "This cannot be undone".into(),
            modal: true,
            bounds: [10, 20, 300, 160],
            buttons: vec![AccessibleDialogButton {
                label: "Cancel".into(),
                bounds: [20, 120, 80, 32],
            }],
        };
        let update = tree_update(&UiTree::default(), Some(&dialog), None);
        assert_eq!(update.nodes.len(), 3);
        assert_eq!(update.focus, dialog_button_id(9, 0));
        assert_eq!(update.nodes[0].1.children(), &[dialog_id(9)]);
        assert!(update.nodes[1].1.is_modal());
    }
}
