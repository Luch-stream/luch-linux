//! "May Living room TV control this computer?": a notification with three answers. No answer in time is a no.

use std::{collections::HashMap, time::Duration};

use futures_util::StreamExt;
use zbus::{zvariant, Connection, MatchRule, MessageStream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Once,
    Always,
    No,
}

impl Answer {
    /// The `grant` of the `input` message (PROTOCOL.md 4h).
    pub fn grant(self) -> &'static str {
        match self {
            Answer::Once => "once",
            Answer::Always => "always",
            Answer::No => "no",
        }
    }
}

pub const TIMEOUT: Duration = Duration::from_secs(60);
pub const CHOICES_FILE: &str = "input";

/// What a saved choice means for a new session: `always` grants at once, anything else asks.
pub fn remembered(saved: Option<&str>) -> Option<Answer> {
    (saved == Some("always")).then_some(Answer::Always)
}

/// Asks the person through the notification server and waits for a button (or the timeout).
pub async fn ask(
    conn: &Connection,
    receiver: &str,
    devices: &[String],
    timeout: Duration,
) -> Answer {
    let what = if devices.is_empty() {
        "input".to_string()
    } else {
        devices.join(", ")
    };
    let Ok(proxy) = zbus::Proxy::new(
        conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    else {
        return Answer::No;
    };
    // Listen before asking: a quick click must not be missed.
    let Ok(rule) = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.Notifications")
        .and_then(|b| b.path("/org/freedesktop/Notifications"))
        .map(|b| b.build())
    else {
        return Answer::No;
    };
    let Ok(mut signals) = MessageStream::for_match_rule(rule, conn, Some(16)).await else {
        return Answer::No;
    };
    let mut hints: HashMap<&str, zvariant::Value> = HashMap::new();
    hints.insert("urgency", zvariant::Value::U8(2));
    hints.insert("resident", zvariant::Value::Bool(true));
    let actions = vec![
        "once",
        "Allow once",
        "always",
        "Always allow",
        "no",
        "Don't allow",
    ];
    let summary = format!("May {receiver} control this computer?");
    let body = format!(
        "It wants to use its {what} on this desktop. You can take it back from the Luch menu."
    );
    let id: u32 = match proxy
        .call(
            "Notify",
            &(
                "Luch",
                0u32,
                "input-touchscreen",
                summary.as_str(),
                body.as_str(),
                actions,
                hints,
                0i32,
            ),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!("[luchd] input: notification: {e}");
            return Answer::No;
        }
    };
    let wait = async {
        while let Some(Ok(m)) = signals.next().await {
            let member = m
                .header()
                .member()
                .map(|m| m.to_string())
                .unwrap_or_default();
            match member.as_str() {
                "ActionInvoked" => {
                    if let Ok((nid, action)) = m.body().deserialize::<(u32, String)>() {
                        if nid == id {
                            return match action.as_str() {
                                "once" => Answer::Once,
                                "always" => Answer::Always,
                                _ => Answer::No,
                            };
                        }
                    }
                }
                // Dismissed without a button: no.
                "NotificationClosed"
                    if m.body()
                        .deserialize::<(u32, u32)>()
                        .is_ok_and(|(nid, _)| nid == id) =>
                {
                    return Answer::No;
                }
                _ => {}
            }
        }
        Answer::No
    };
    let answer = tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or(Answer::No);
    let _ = proxy.call::<_, _, ()>("CloseNotification", &(id,)).await;
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn only_always_is_remembered() {
        assert_eq!(remembered(Some("always")), Some(Answer::Always));
        assert_eq!(remembered(Some("ask")), None);
        assert_eq!(remembered(None), None);
    }

    type Seen = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    /// A notification server that clicks the given action on our question. Other tests' notifications may come
    /// to it too: they get their own ids and no click.
    struct FakeNotifications {
        click: Arc<Mutex<Option<&'static str>>>,
        seen: Seen,
        next: std::sync::atomic::AtomicU32,
    }

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl FakeNotifications {
        #[allow(clippy::too_many_arguments)]
        async fn notify(
            &self,
            _app: String,
            _replaces: u32,
            _icon: String,
            summary: String,
            _body: String,
            actions: Vec<String>,
            _hints: HashMap<String, zvariant::OwnedValue>,
            _timeout: i32,
            #[zbus(signal_emitter)] e: zbus::object_server::SignalEmitter<'_>,
        ) -> u32 {
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let ours = summary.starts_with("May ");
            if !ours {
                return id;
            }
            self.seen.lock().unwrap().push((summary, actions));
            let click = *self.click.lock().unwrap();
            if let Some(a) = click {
                let e = e.to_owned();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = Self::action_invoked(&e, id, a).await;
                });
            }
            id
        }

        async fn close_notification(&self, _id: u32) {}

        #[zbus(signal)]
        async fn action_invoked(
            e: &zbus::object_server::SignalEmitter<'_>,
            id: u32,
            action: &str,
        ) -> zbus::Result<()>;
    }

    async fn serve() -> (Connection, Seen, Arc<Mutex<Option<&'static str>>>) {
        let (seen, click) = (Seen::default(), Arc::new(Mutex::new(None)));
        let fake = FakeNotifications {
            click: click.clone(),
            seen: seen.clone(),
            next: Default::default(),
        };
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", fake)
            .unwrap()
            .build()
            .await
            .unwrap();
        (conn, seen, click)
    }

    /// Runs on the private session bus of `dbus-run-session` (see the README's test command).
    #[tokio::test]
    async fn the_person_is_asked_and_the_button_decides() {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            eprintln!("no session bus: skipped");
            return;
        }
        let (_server, seen, click) = serve().await;
        *click.lock().unwrap() = Some("always");
        let me = Connection::session().await.unwrap();
        let a = ask(
            &me,
            "Living room TV",
            &["touch".into(), "keyboard".into()],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(a, Answer::Always);
        let (summary, actions) = seen.lock().unwrap()[0].clone();
        assert_eq!(summary, "May Living room TV control this computer?");
        assert_eq!(
            actions
                .iter()
                .step_by(2)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["once", "always", "no"]
        );

        // Nobody answers: no.
        *click.lock().unwrap() = None;
        let a = ask(&me, "Living room TV", &[], Duration::from_millis(200)).await;
        assert_eq!(a, Answer::No);
    }
}
