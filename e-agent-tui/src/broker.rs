use crate::ui_protocol::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{mpsc, oneshot};

const QUEUE_CAPACITY: usize = 64;
const NOTIFICATION_CAPACITY: usize = 32;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiDiagnostic {
    NotificationsOverflowed,
    RawInputSubscriberDisabled(u64),
}
#[derive(Debug)]
struct Message(UiEnvelope);
#[derive(Debug, Clone)]
pub struct UiBrokerClient {
    tx: mpsc::Sender<Message>,
    input: tokio::sync::broadcast::Sender<crate::input::InputEvent>,
    state: Arc<Mutex<BrokerState>>,
}
pub struct UiBrokerServer {
    rx: mpsc::Receiver<Message>,
    input: tokio::sync::broadcast::Sender<crate::input::InputEvent>,
    state: Arc<Mutex<BrokerState>>,
    capabilities: UiCapabilities,
}
#[derive(Debug)]
struct Pending {
    extension: ExtensionId,
    modal: bool,
    reply: oneshot::Sender<UiReply>,
}
#[derive(Debug)]
struct BrokerState {
    next: u64,
    generation: Generation,
    pending: BTreeMap<RequestId, Pending>,
    modal: BTreeSet<ExtensionId>,
    owned: BTreeMap<ExtensionId, BTreeSet<RequestId>>,
    coalesced: BTreeMap<(ExtensionId, String), RequestId>,
    notifications: VecDeque<(ExtensionId, Notification)>,
    input_polls: VecDeque<RequestId>,
    raw_subscribers: BTreeMap<u64, mpsc::Sender<String>>,
    diagnostics: VecDeque<UiDiagnostic>,
    notification_overflow_reported: bool,
    closed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    Closed,
    QueueFull,
    Busy,
}

pub fn channel(capabilities: UiCapabilities) -> (UiBrokerClient, UiBrokerServer) {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let (input, _) = tokio::sync::broadcast::channel(64);
    let state = Arc::new(Mutex::new(BrokerState {
        next: 0,
        generation: Generation(0),
        pending: BTreeMap::new(),
        modal: BTreeSet::new(),
        owned: BTreeMap::new(),
        coalesced: BTreeMap::new(),
        notifications: VecDeque::new(),
        input_polls: VecDeque::new(),
        raw_subscribers: BTreeMap::new(),
        diagnostics: VecDeque::new(),
        notification_overflow_reported: false,
        closed: false,
    }));
    (
        UiBrokerClient {
            tx,
            input: input.clone(),
            state: state.clone(),
        },
        UiBrokerServer {
            rx,
            input,
            state,
            capabilities,
        },
    )
}
impl UiBrokerClient {
    pub fn subscribe_input(&self) -> tokio::sync::broadcast::Receiver<crate::input::InputEvent> {
        self.input.subscribe()
    }
    pub async fn request(
        &self,
        extension: ExtensionId,
        operation: UiOperation,
    ) -> Result<UiReply, BrokerError> {
        self.request_until(extension, operation, None).await
    }
    pub async fn request_until(
        &self,
        extension: ExtensionId,
        operation: UiOperation,
        deadline: Option<Instant>,
    ) -> Result<UiReply, BrokerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (request, generation) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(BrokerError::Closed);
            }
            let modal = matches!(operation, UiOperation::Dialog(_));
            if modal && !state.modal.insert(extension.clone()) {
                return Err(BrokerError::Busy);
            }
            let request = RequestId(state.next);
            state.next += 1;
            if let Some(key) = operation.coalesce_key() {
                let key = (extension.clone(), key);
                if let Some(old) = state.coalesced.insert(key, request) {
                    cancel_locked(&mut state, old, UiReply::Cancelled);
                }
            }
            let generation = state.generation;
            state.pending.insert(
                request,
                Pending {
                    extension: extension.clone(),
                    modal,
                    reply: reply_tx,
                },
            );
            state
                .owned
                .entry(extension.clone())
                .or_default()
                .insert(request);
            (request, generation)
        };
        let envelope = UiEnvelope {
            version: UiProtocolVersion::CURRENT,
            extension,
            request,
            generation,
            deadline,
            operation,
        };
        if let Err(error) = self.tx.try_send(Message(envelope)) {
            self.cancel(request);
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => BrokerError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => BrokerError::Closed,
            });
        }
        match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline.into(), reply_rx).await {
                Ok(Ok(reply)) => Ok(reply),
                Ok(Err(_)) => Err(BrokerError::Closed),
                Err(_) => {
                    self.cancel(request);
                    Ok(UiReply::Cancelled)
                }
            },
            None => reply_rx.await.map_err(|_| BrokerError::Closed),
        }
    }
    pub fn notify(&self, extension: ExtensionId, notification: Notification) {
        if let Ok(mut state) = self.state.lock() {
            if state.notifications.len() == NOTIFICATION_CAPACITY {
                let position = state
                    .notifications
                    .iter()
                    .position(|(_, item)| !matches!(item.level, NotificationLevel::Error))
                    .unwrap_or(0);
                state.notifications.remove(position);
                if !state.notification_overflow_reported {
                    state
                        .diagnostics
                        .push_back(UiDiagnostic::NotificationsOverflowed);
                    state.notification_overflow_reported = true;
                }
            }
            state.notifications.push_back((extension, notification));
        }
    }
    pub fn cancel(&self, request: RequestId) {
        if let Ok(mut state) = self.state.lock() {
            cancel_locked(&mut state, request, UiReply::Cancelled);
        }
    }
    pub fn unload(&self, extension: &ExtensionId) {
        if let Ok(mut state) = self.state.lock() {
            cancel_extension(&mut state, extension);
        }
    }
    pub fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            let ids: Vec<_> = state.pending.keys().copied().collect();
            for id in ids {
                cancel_locked(&mut state, id, UiReply::Cancelled);
            }
        }
    }
}
impl UiBrokerServer {
    pub async fn recv(&mut self) -> Option<UiEnvelope> {
        self.rx.recv().await.map(|message| message.0)
    }
    pub fn capabilities(&self) -> &UiCapabilities {
        &self.capabilities
    }
    pub fn default_reply(&self, envelope: &UiEnvelope) -> UiReply {
        if envelope.version.major != self.capabilities.protocol.major {
            return UiReply::Failed(format!(
                "unsupported UI protocol major {}",
                envelope.version.major
            ));
        }
        if envelope.generation != self.state.lock().unwrap().generation {
            return UiReply::StaleHandle;
        }
        if matches!(envelope.operation, UiOperation::Capabilities) {
            return UiReply::Capabilities(self.capabilities.clone());
        }
        if matches!(envelope.operation, UiOperation::Unknown(_)) {
            return UiReply::Unsupported {
                operation: UiOperationKind::Unknown,
                reason: "unknown operation".into(),
            };
        }
        match supported(&self.capabilities, envelope.operation.kind()) {
            Ok(()) => UiReply::Ack,
            Err(reply) => reply,
        }
    }
    pub fn reply(&self, request: RequestId, reply: UiReply) -> bool {
        self.state
            .lock()
            .is_ok_and(|mut state| complete_locked(&mut state, request, reply))
    }
    pub fn take_notification(&self) -> Option<(ExtensionId, Notification)> {
        let mut state = self.state.lock().ok()?;
        let item = state.notifications.pop_front();
        if state.notifications.is_empty() {
            state.notification_overflow_reported = false;
        }
        item
    }
    pub fn queue_input_poll(&self, request: RequestId) -> bool {
        self.state.lock().is_ok_and(|mut state| {
            if state.pending.contains_key(&request) && !state.input_polls.contains(&request) {
                state.input_polls.push_back(request);
                true
            } else {
                false
            }
        })
    }
    pub fn reply_input(&self, event: crate::input::InputEvent) -> bool {
        self.state.lock().is_ok_and(|mut state| {
            while let Some(request) = state.input_polls.pop_back() {
                if complete_locked(&mut state, request, UiReply::Input(event.clone())) {
                    return true;
                }
            }
            false
        })
    }
    pub fn publish_input(&self, event: crate::input::InputEvent) -> bool {
        self.input.send(event).is_ok()
    }
    pub fn subscribe_raw_input(&self, id: u64, capacity: usize) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel(capacity);
        if let Ok(mut state) = self.state.lock() {
            state.raw_subscribers.insert(id, tx);
        }
        rx
    }
    pub fn deliver_raw_input(&self, input: String) {
        if let Ok(mut state) = self.state.lock() {
            let disabled: Vec<_> = state
                .raw_subscribers
                .iter()
                .filter_map(|(id, tx)| tx.try_send(input.clone()).err().map(|_| *id))
                .collect();
            for id in disabled {
                state.raw_subscribers.remove(&id);
                state
                    .diagnostics
                    .push_back(UiDiagnostic::RawInputSubscriberDisabled(id));
            }
        }
    }
    pub fn take_diagnostic(&self) -> Option<UiDiagnostic> {
        self.state.lock().ok()?.diagnostics.pop_front()
    }
    pub fn invalidate_handles(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation.0 += 1;
        }
    }
    pub fn unload(&self, extension: &ExtensionId) {
        if let Ok(mut state) = self.state.lock() {
            cancel_extension(&mut state, extension);
        }
    }
}
impl Drop for UiBrokerServer {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            let ids: Vec<_> = state.pending.keys().copied().collect();
            for id in ids {
                cancel_locked(&mut state, id, UiReply::Cancelled);
            }
        }
    }
}
fn cancel_extension(state: &mut BrokerState, extension: &ExtensionId) {
    let ids: Vec<_> = state
        .owned
        .get(extension)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .collect();
    for id in ids {
        cancel_locked(state, id, UiReply::Cancelled);
    }
}
fn complete_locked(state: &mut BrokerState, id: RequestId, reply: UiReply) -> bool {
    let Some(pending) = state.pending.remove(&id) else {
        return false;
    };
    if pending.modal {
        state.modal.remove(&pending.extension);
    }
    if let Some(ids) = state.owned.get_mut(&pending.extension) {
        ids.remove(&id);
    }
    state.coalesced.retain(|_, request| *request != id);
    state.input_polls.retain(|request| *request != id);
    let _ = pending.reply.send(reply);
    true
}
fn cancel_locked(state: &mut BrokerState, id: RequestId, reply: UiReply) {
    complete_locked(state, id, reply);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ext() -> ExtensionId {
        ExtensionId("test".into())
    }
    #[tokio::test]
    async fn correlates_and_serializes_modals() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        ext(),
                        UiOperation::Dialog(DialogRequest::Confirm {
                            title: "x".into(),
                            message: "y".into(),
                        }),
                    )
                    .await
            }
        });
        let e = server.recv().await.unwrap();
        assert_eq!(
            client
                .request(
                    ext(),
                    UiOperation::Dialog(DialogRequest::Input {
                        title: "x".into(),
                        placeholder: "".into()
                    })
                )
                .await,
            Err(BrokerError::Busy)
        );
        server.reply(e.request, UiReply::Dialog(DialogResult::Confirmed(true)));
        assert_eq!(
            task.await.unwrap().unwrap(),
            UiReply::Dialog(DialogResult::Confirmed(true))
        );
    }
    #[tokio::test]
    async fn cancellation_and_unload_settle_and_discard_late_replies() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        ext(),
                        UiOperation::Dialog(DialogRequest::Input {
                            title: "x".into(),
                            placeholder: "".into(),
                        }),
                    )
                    .await
            }
        });
        let e = server.recv().await.unwrap();
        client.unload(&ext());
        assert_eq!(task.await.unwrap().unwrap(), UiReply::Cancelled);
        assert!(!server.reply(e.request, UiReply::Ack));
    }
    #[tokio::test]
    async fn coalescing_cancels_replaced_key() {
        let (client, mut server) = channel(UiCapabilities::default());
        let first = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        ext(),
                        UiOperation::Render {
                            key: "row".into(),
                            content: "old".into(),
                        },
                    )
                    .await
            }
        });
        let _ = server.recv().await.unwrap();
        let second = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        ext(),
                        UiOperation::Render {
                            key: "row".into(),
                            content: "new".into(),
                        },
                    )
                    .await
            }
        });
        let e = server.recv().await.unwrap();
        assert_eq!(first.await.unwrap().unwrap(), UiReply::Cancelled);
        server.reply(e.request, UiReply::Ack);
        assert_eq!(second.await.unwrap().unwrap(), UiReply::Ack);
    }
    #[tokio::test]
    async fn timeout_settles_and_late_reply_is_ignored() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request_until(
                        ext(),
                        UiOperation::Dialog(DialogRequest::Input {
                            title: "x".into(),
                            placeholder: "".into(),
                        }),
                        Some(Instant::now() + std::time::Duration::from_millis(5)),
                    )
                    .await
            }
        });
        let e = server.recv().await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), UiReply::Cancelled);
        assert!(!server.reply(e.request, UiReply::Ack));
    }
    #[tokio::test]
    async fn input_poll_replies_to_the_most_recent_live_request() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn(async move {
            client
                .request(
                    ext(),
                    UiOperation::TerminalInput {
                        subscription: 7,
                        enabled: true,
                    },
                )
                .await
        });
        let envelope = server.recv().await.unwrap();
        assert!(server.queue_input_poll(envelope.request));
        assert!(server.reply_input(crate::input::InputEvent::Text("x".into())));
        assert_eq!(
            task.await.unwrap().unwrap(),
            UiReply::Input(crate::input::InputEvent::Text("x".into()))
        );
    }

    #[test]
    fn notification_overflow_and_slow_raw_input_are_bounded_and_diagnosed_once() {
        let (client, server) = channel(UiCapabilities::default());
        for index in 0..40 {
            client.notify(
                ext(),
                Notification {
                    message: index.to_string(),
                    level: NotificationLevel::Info,
                },
            );
        }
        assert_eq!(
            server.state.lock().unwrap().notifications.len(),
            NOTIFICATION_CAPACITY
        );
        assert_eq!(
            server.take_diagnostic(),
            Some(UiDiagnostic::NotificationsOverflowed)
        );
        assert_eq!(server.take_diagnostic(), None);
        let _receiver = server.subscribe_raw_input(7, 1);
        server.deliver_raw_input("a".into());
        server.deliver_raw_input("b".into());
        assert_eq!(
            server.take_diagnostic(),
            Some(UiDiagnostic::RawInputSubscriberDisabled(7))
        );
    }

    #[tokio::test]
    async fn stale_generation_is_rejected() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn(async move {
            client
                .request(
                    ext(),
                    UiOperation::Overlay {
                        id: OverlayId(1),
                        generation: Generation(0),
                        action: OverlayAction::SetHidden(false),
                    },
                )
                .await
        });
        let envelope = server.recv().await.unwrap();
        server.invalidate_handles();
        assert_eq!(server.default_reply(&envelope), UiReply::StaleHandle);
        server.reply(envelope.request, UiReply::StaleHandle);
        assert_eq!(task.await.unwrap().unwrap(), UiReply::StaleHandle);
    }

    #[tokio::test]
    async fn unsupported_version_capabilities_and_operations_are_typed() {
        let (client, mut server) = channel(UiCapabilities::default());
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .request(
                        ext(),
                        UiOperation::Notify(Notification {
                            message: "x".into(),
                            level: NotificationLevel::Info,
                        }),
                    )
                    .await
            }
        });
        let e = server.recv().await.unwrap();
        let reply = server.default_reply(&e);
        assert!(matches!(
            reply,
            UiReply::Unsupported {
                operation: UiOperationKind::Notification,
                ..
            }
        ));
        server.reply(e.request, reply);
        assert!(matches!(
            task.await.unwrap().unwrap(),
            UiReply::Unsupported { .. }
        ));
        let mut bad = e;
        bad.version.major += 1;
        assert!(matches!(server.default_reply(&bad), UiReply::Failed(_)));
        bad.version = UiProtocolVersion::CURRENT;
        bad.operation = UiOperation::Unknown("future".into());
        assert!(matches!(
            server.default_reply(&bad),
            UiReply::Unsupported {
                operation: UiOperationKind::Unknown,
                ..
            }
        ));
    }
}
