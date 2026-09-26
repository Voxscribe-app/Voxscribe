use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use zbus::zvariant::Value;
use zbus::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    fn as_u8(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;
}

pub struct Notifier {
    proxy: Option<NotificationsProxy<'static>>,
    last_id: AtomicU32,
    enabled: bool,
}

impl Notifier {
    pub async fn new(enabled: bool) -> Self {
        if !enabled {
            return Self {
                proxy: None,
                last_id: AtomicU32::new(0),
                enabled: false,
            };
        }
        let proxy = match Connection::session().await {
            Ok(connection) => match NotificationsProxy::new(&connection).await {
                Ok(proxy) => Some(proxy),
                Err(err) => {
                    tracing::info!("desktop notifications unavailable: {err}");
                    None
                }
            },
            Err(err) => {
                tracing::info!("no session bus, notifications disabled: {err}");
                None
            }
        };
        let enabled = proxy.is_some();
        Self {
            proxy,
            last_id: AtomicU32::new(0),
            enabled,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub async fn notify(&self, summary: &str, body: &str, urgency: Urgency) {
        let Some(proxy) = &self.proxy else { return };

        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(urgency.as_u8()));
        if urgency != Urgency::Critical {
            hints.insert("transient", Value::Bool(true));
        }

        let replaces = if urgency == Urgency::Critical {
            0
        } else {
            self.last_id.load(Ordering::Relaxed)
        };
        let timeout = if urgency == Urgency::Critical {
            0
        } else {
            4000
        };

        match proxy
            .notify(
                "Voxscribe",
                replaces,
                "audio-input-microphone",
                summary,
                body,
                &[],
                hints,
                timeout,
            )
            .await
        {
            Ok(id) if urgency != Urgency::Critical => {
                self.last_id.store(id, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(err) => tracing::debug!("notification failed: {err}"),
        }
    }

    pub async fn error(&self, message: &str) {
        self.notify("Voxscribe", message, Urgency::Critical).await;
    }

    pub async fn clear(&self) {
        let Some(proxy) = &self.proxy else { return };
        let id = self.last_id.swap(0, Ordering::Relaxed);
        if id != 0 {
            let _ = proxy.close_notification(id).await;
        }
    }
}

pub async fn watch_suspend<F>(mut on_sleep: F) -> Result<()>
where
    F: FnMut(bool) + Send + 'static,
{
    use futures_util::StreamExt;

    let connection = Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;

    let mut stream = proxy.receive_signal("PrepareForSleep").await?;
    while let Some(message) = stream.next().await {
        if let Ok(sleeping) = message.body().deserialize::<bool>() {
            on_sleep(sleeping);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_maps_onto_the_freedesktop_hint_values() {
        assert_eq!(Urgency::Low.as_u8(), 0);
        assert_eq!(Urgency::Normal.as_u8(), 1);
        assert_eq!(Urgency::Critical.as_u8(), 2);
    }

    #[tokio::test]
    async fn a_disabled_notifier_never_touches_the_bus() {
        let notifier = Notifier::new(false).await;
        assert!(!notifier.is_enabled());
        notifier.notify("Voxscribe", "hello", Urgency::Normal).await;
        notifier.clear().await;
    }
}
