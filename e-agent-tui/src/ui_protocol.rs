use std::{collections::BTreeMap, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UiProtocolVersion {
    pub major: u16,
    pub minor: u16,
}
impl UiProtocolVersion {
    pub const CURRENT: Self = Self { major: 1, minor: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtensionId(pub String);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupportLevel {
    Native,
    Adapted,
    Degraded(String),
    Unsupported(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiCapabilities {
    pub protocol: UiProtocolVersion,
    pub operations: BTreeMap<UiOperationKind, SupportLevel>,
}
impl Default for UiCapabilities {
    fn default() -> Self {
        let mut operations = BTreeMap::new();
        for kind in UiOperationKind::ALL {
            operations.insert(
                kind,
                SupportLevel::Unsupported("not enabled by this frontend".into()),
            );
        }
        Self {
            protocol: UiProtocolVersion::CURRENT,
            operations,
        }
    }
}
impl UiCapabilities {
    pub fn support(&self, kind: UiOperationKind) -> SupportLevel {
        self.operations
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| SupportLevel::Unsupported("unknown operation".into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UiOperationKind {
    Dialog,
    Notification,
    Contribution,
    Editor,
    Overlay,
    Theme,
    Keybindings,
    TerminalInput,
    Clipboard,
    Render,
    Capabilities,
    Unknown,
}
impl UiOperationKind {
    pub const ALL: [Self; 11] = [
        Self::Dialog,
        Self::Notification,
        Self::Contribution,
        Self::Editor,
        Self::Overlay,
        Self::Theme,
        Self::Keybindings,
        Self::TerminalInput,
        Self::Clipboard,
        Self::Render,
        Self::Capabilities,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogRequest {
    Select { title: String, options: Vec<String> },
    Confirm { title: String, message: String },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    Selected(Option<String>),
    Confirmed(bool),
    Input(Option<String>),
    Edited(Option<String>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub message: String,
    pub level: NotificationLevel,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contribution {
    Set {
        slot: String,
        key: String,
        content: String,
    },
    Remove {
        slot: String,
        key: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    Show { content: String, capturing: bool },
    Hide,
    SetHidden(bool),
    Focus,
    Unfocus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiOperation {
    Dialog(DialogRequest),
    Notify(Notification),
    Contribution(Contribution),
    Editor {
        text: Option<String>,
    },
    Paste {
        text: String,
    },
    CustomEditor {
        content: Option<String>,
    },
    Overlay {
        id: OverlayId,
        generation: Generation,
        action: OverlayAction,
    },
    Theme {
        generation: u64,
    },
    Keybindings {
        entries: Vec<(String, String)>,
    },
    TerminalInput {
        subscription: u64,
        enabled: bool,
    },
    Clipboard {
        text: String,
    },
    Render {
        key: String,
        content: String,
    },
    Frame {
        key: String,
        frame: crate::render::SemanticFrame,
        cursor: Option<crate::component::CursorAnchor>,
    },
    Capabilities,
    Unknown(String),
}
impl UiOperation {
    pub fn kind(&self) -> UiOperationKind {
        match self {
            Self::Dialog(_) => UiOperationKind::Dialog,
            Self::Notify(_) => UiOperationKind::Notification,
            Self::Contribution(_) => UiOperationKind::Contribution,
            Self::Editor { .. } | Self::Paste { .. } | Self::CustomEditor { .. } => {
                UiOperationKind::Editor
            }
            Self::Overlay { .. } => UiOperationKind::Overlay,
            Self::Theme { .. } => UiOperationKind::Theme,
            Self::Keybindings { .. } => UiOperationKind::Keybindings,
            Self::TerminalInput { .. } => UiOperationKind::TerminalInput,
            Self::Clipboard { .. } => UiOperationKind::Clipboard,
            Self::Render { .. } | Self::Frame { .. } => UiOperationKind::Render,
            Self::Capabilities => UiOperationKind::Capabilities,
            Self::Unknown(_) => UiOperationKind::Unknown,
        }
    }
    pub fn coalesce_key(&self) -> Option<String> {
        match self {
            Self::Contribution(Contribution::Set { slot, key, .. })
            | Self::Contribution(Contribution::Remove { slot, key }) => {
                Some(format!("contribution:{slot}:{key}"))
            }
            Self::Render { key, .. } | Self::Frame { key, .. } => Some(format!("render:{key}")),
            Self::Editor { .. } => Some("editor".into()),
            Self::CustomEditor { .. } => Some("custom-editor".into()),
            Self::Theme { .. } => Some("theme".into()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiEnvelope {
    pub version: UiProtocolVersion,
    pub extension: ExtensionId,
    pub request: RequestId,
    pub generation: Generation,
    pub deadline: Option<Instant>,
    pub operation: UiOperation,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiReply {
    Dialog(DialogResult),
    Input(crate::input::InputEvent),
    Value(String),
    Ack,
    Capabilities(UiCapabilities),
    Unsupported {
        operation: UiOperationKind,
        reason: String,
    },
    Busy,
    Cancelled,
    StaleHandle,
    Failed(String),
}

pub fn native_capabilities() -> UiCapabilities {
    let mut capabilities = UiCapabilities::default();
    for kind in [
        UiOperationKind::Dialog,
        UiOperationKind::Notification,
        UiOperationKind::Contribution,
        UiOperationKind::Editor,
        UiOperationKind::Overlay,
        UiOperationKind::Theme,
        UiOperationKind::Keybindings,
        UiOperationKind::TerminalInput,
        UiOperationKind::Render,
    ] {
        capabilities.operations.insert(kind, SupportLevel::Native);
    }
    capabilities.operations.insert(
        UiOperationKind::Clipboard,
        SupportLevel::Degraded("paste is native; copy depends on terminal selection".into()),
    );
    capabilities
}

pub fn supported(capabilities: &UiCapabilities, operation: UiOperationKind) -> Result<(), UiReply> {
    match capabilities.support(operation) {
        SupportLevel::Unsupported(reason) => Err(UiReply::Unsupported { operation, reason }),
        _ => Ok(()),
    }
}
