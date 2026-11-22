//! Avahi owns service lifetimes. Resolve on announcements, once a minute, or on user request.
use super::*;
use std::collections::HashSet;
use tokio::{
    sync::{oneshot, Notify},
    task::{AbortHandle, JoinSet},
};

const REFRESH_EVERY: Duration = Duration::from_secs(60);
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Service {
    iface: i32,
    proto: i32,
    name: String,
    domain: String,
}
type Answer = Result<Option<Receiver>, String>;
type Waiter = (String, oneshot::Sender<Result<Receiver, String>>);
struct Pending {
    generation: u64,
    abort: AbortHandle,
    waiters: Vec<Waiter>,
}

#[derive(Clone)]
pub struct Discovery {
    refresh: Arc<Notify>,
    requests: mpsc::Sender<Waiter>,
}
pub struct Worker {
    refresh: Arc<Notify>,
    requests: mpsc::Receiver<Waiter>,
}
impl Discovery {
    pub fn new() -> (Self, Worker) {
        let refresh = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(8);
        (
            Self {
                refresh: refresh.clone(),
                requests: tx,
            },
            Worker {
                refresh,
                requests: rx,
            },
        )
    }
    pub fn refresh(&self) {
        self.refresh.notify_one();
    }
    pub async fn receiver(&self, id: String) -> Result<Receiver, String> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send((id, tx))
            .await
            .map_err(|_| "discovery stopped")?;
        rx.await.map_err(|_| "discovery stopped")?
    }
}

#[derive(Default)]
struct Lookups {
    jobs: JoinSet<(Service, u64, Answer)>,
    pending: HashMap<Service, Pending>,
    generation: u64,
}
impl Lookups {
    fn start(
        &mut self,
        key: Service,
        lookup: impl std::future::Future<Output = Answer> + Send + 'static,
    ) {
        if self.pending.contains_key(&key) {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let service = key.clone();
        let abort = self
            .jobs
            .spawn(async move { (service, generation, lookup.await) });
        self.pending.insert(
            key,
            Pending {
                generation,
                abort,
                waiters: Vec::new(),
            },
        );
    }
    fn resolve(&mut self, sys: &Connection, key: &Service) {
        let (sys, service) = (sys.clone(), key.clone());
        self.start(key.clone(), async move {
            resolve(
                &sys,
                service.iface,
                service.proto,
                &service.name,
                p::RECEIVER_TYPE,
                &service.domain,
            )
            .await
            .map_err(|e| e.to_string())
        });
    }
    fn remove(&mut self, key: &Service) {
        if let Some(pending) = self.pending.remove(key) {
            pending.abort.abort();
            for (_, reply) in pending.waiters {
                let _ = reply.send(Err("receiver went away".into()));
            }
        }
    }
    fn finish(&mut self, key: &Service, generation: u64) -> Option<Pending> {
        // A removed/re-announced service must not receive a previous lookup's late result.
        if self.pending.get(key)?.generation != generation {
            return None;
        }
        self.pending.remove(key)
    }
}

async fn browser(sys: &Connection) -> zbus::Result<OwnedObjectPath> {
    let args = (
        AVAHI_IF_UNSPEC,
        AVAHI_PROTO_INET,
        p::RECEIVER_TYPE,
        "",
        0u32,
    );
    sys.call_method(
        Some(AVAHI),
        "/",
        Some(AVAHI_SERVER),
        "ServiceBrowserNew",
        &args,
    )
    .await?
    .body()
    .deserialize()
}
async fn free_browser(sys: &Connection, path: &OwnedObjectPath) {
    let _ = sys
        .call_method(
            Some(AVAHI),
            path,
            Some("org.freedesktop.Avahi.ServiceBrowser"),
            "Free",
            &(),
        )
        .await;
}

/// Publish changed metadata only; a failed resolution leaves the last known receiver intact.
async fn remember(st: &Shared, conn: &Connection, r: &Receiver) {
    if is_tunnel(&r.ifname) {
        return;
    }
    let mut s = st.lock().await;
    if let Some(old) = s.found.get(&r.id) {
        if old == r
            || (old.ifindex != r.ifindex && (old.ifname == lan_iface() || r.ifname != lan_iface()))
        {
            return;
        }
    }
    eprintln!(
        "[luchd] found: {} id={} {}:{} via {}",
        r.name, r.id, r.address, r.port, r.ifname
    );
    s.found.insert(r.id.clone(), r.clone());
    drop(s);
    changed(conn).await;
}

impl Worker {
    pub async fn run(mut self, sys: Connection, st: Shared, conn: Connection) -> zbus::Result<()> {
        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .sender(AVAHI)?
            .interface("org.freedesktop.Avahi.ServiceBrowser")?
            .build();
        let mut signals = MessageStream::for_match_rule(rule, &sys, None).await?;
        // Keep this browser alive: it is authoritative for ItemRemove throughout rescans.
        let primary = browser(&sys).await?;
        let mut scan: Option<(OwnedObjectPath, Instant)> = None;
        let mut services = HashSet::new();
        let mut removed_during_scan = HashSet::new();
        let mut lookups = Lookups::default();
        let mut minute = tokio::time::interval(REFRESH_EVERY);
        minute.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        minute.tick().await;
        loop {
            tokio::select! {
                _ = async {
                    if let Some((_, until)) = &scan { tokio::time::sleep_until(*until).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    if let Some((path, _)) = scan.take() { free_browser(&sys, &path).await; }
                }
                _ = async { tokio::select! { _ = minute.tick() => {}, _ = self.refresh.notified() => {} } } => {
                    // Enumerate PTRs again, including services whose first resolution timed out.
                    // An overlapping refresh shares the scan and all in-flight lookups.
                    if scan.is_none() {
                        match browser(&sys).await {
                            Ok(path) => {
                                removed_during_scan.clear();
                                scan = Some((path, Instant::now() + HANDSHAKE_TIMEOUT));
                            }
                            Err(e) => eprintln!("[luchd] avahi refresh failed: {e}"),
                        }
                    }
                    for key in &services { lookups.resolve(&sys, key); }
                }
                Some((id, reply)) = self.requests.recv() => {
                    let receiver = st.lock().await.found.get(&id).cloned();
                    let Some(r) = receiver else { let _ = reply.send(Err(format!("unknown receiver: {id}"))); continue; };
                    if r.service.is_empty() { let _ = reply.send(Ok(r)); continue; }
                    let key = services.iter().find(|s| s.iface == r.ifindex && s.name == r.service).cloned();
                    let Some(key) = key else { let _ = reply.send(Err("receiver is no longer announced".into())); continue; };
                    lookups.resolve(&sys, &key);
                    lookups.pending.get_mut(&key).unwrap().waiters.push((id, reply));
                }
                Some(result) = lookups.jobs.join_next(), if !lookups.jobs.is_empty() => {
                    let Ok((key, generation, result)) = result else { continue; };
                    let Some(pending) = lookups.finish(&key, generation) else { continue; };
                    match &result {
                        Ok(Some(r)) => remember(&st, &conn, r).await,
                        Ok(None) => eprintln!("[luchd] ignored {}: no id or unsupported v", key.name),
                        Err(e) => eprintln!("[luchd] resolve {} failed: {e}; will retry on refresh", key.name),
                    }
                    for (id, reply) in pending.waiters {
                        let answer = match &result {
                            Ok(Some(r)) if r.id == id => Ok(r.clone()),
                            Ok(_) => Err("receiver identity or protocol changed".into()),
                            Err(e) => Err(e.clone()),
                        };
                        let _ = reply.send(answer);
                    }
                }
                msg = signals.next() => {
                    let Some(msg) = msg else { break; };
                    let msg = msg?;
                    let header = msg.header();
                    let path = header.path().map(|p| p.as_str());
                    let is_primary = path == Some(primary.as_str());
                    if !is_primary && path != scan.as_ref().map(|(p, _)| p.as_str()) { continue; }
                    let member = header.member().map(|m| m.as_str()).unwrap_or_default();
                    if !is_primary && matches!(member, "AllForNow" | "Failure") {
                        if let Some((path, _)) = scan.take() { free_browser(&sys, &path).await; }
                        continue;
                    }
                    if member == "Failure" { return Err(zbus::Error::Failure("avahi browser failed".into())); }
                    if member != "ItemNew" && member != "ItemRemove" { continue; }
                    let (iface, proto, name, ty, domain, _): (i32, i32, String, String, String, u32) = msg.body().deserialize()?;
                    if ty != p::RECEIVER_TYPE || is_tunnel(&ifname(iface)) { continue; }
                    let key = Service { iface, proto, name, domain };
                    if member == "ItemRemove" {
                        if !is_primary { continue; }
                        services.remove(&key);
                        if scan.is_some() { removed_during_scan.insert(key.clone()); }
                        lookups.remove(&key);
                        let mut s = st.lock().await;
                        let before = s.found.len();
                        s.found.retain(|_, r| !(r.ifindex == key.iface && r.service == key.name));
                        if s.found.len() != before {
                            drop(s);
                            eprintln!("[luchd] gone: {} ({})", key.name, ifname(key.iface));
                            changed(&conn).await;
                        }
                    } else {
                        if !is_primary && removed_during_scan.contains(&key) { continue; }
                        if is_primary { removed_during_scan.remove(&key); }
                        services.insert(key.clone());
                        lookups.resolve(&sys, &key);
                    }
                }
            }
        }
        if let Some((path, _)) = scan {
            free_browser(&sys, &path).await;
        }
        free_browser(&sys, &primary).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Service {
        Service {
            iface: 1,
            proto: 0,
            name: "receiver".into(),
            domain: "local".into(),
        }
    }

    #[tokio::test]
    async fn overlapping_refreshes_share_lookup_and_failure_can_be_retried() {
        let mut lookups = Lookups::default();
        let key = service();
        let (tx, rx) = oneshot::channel();
        lookups.start(key.clone(), async { Err("timeout".into()) });
        lookups
            .pending
            .get_mut(&key)
            .unwrap()
            .waiters
            .push(("id".into(), tx));
        lookups.start(key.clone(), async {
            panic!("duplicate lookup must never run")
        });
        assert_eq!(lookups.jobs.len(), 1);
        let (completed, generation, result) = lookups.jobs.join_next().await.unwrap().unwrap();
        assert_eq!(result.unwrap_err(), "timeout");
        let pending = lookups.finish(&completed, generation).unwrap();
        assert_eq!(pending.waiters.len(), 1);
        for (_, reply) in pending.waiters {
            let _ = reply.send(Err("timeout".into()));
        }
        assert_eq!(rx.await.unwrap().unwrap_err(), "timeout");
        lookups.start(key.clone(), async { Ok(None) });
        let (_, generation, result) = lookups.jobs.join_next().await.unwrap().unwrap();
        assert!(result.unwrap().is_none());
        assert!(lookups.finish(&key, generation).is_some());
    }

    #[tokio::test]
    async fn removal_cancels_waiters_and_rejects_late_results() {
        let mut lookups = Lookups::default();
        let key = service();
        let (tx, rx) = oneshot::channel();
        lookups.start(key.clone(), std::future::pending());
        let old_generation = lookups.pending[&key].generation;
        lookups
            .pending
            .get_mut(&key)
            .unwrap()
            .waiters
            .push(("id".into(), tx));
        lookups.remove(&key);
        assert_eq!(rx.await.unwrap().unwrap_err(), "receiver went away");
        lookups.start(key.clone(), std::future::pending());
        assert!(lookups.finish(&key, old_generation).is_none());
        assert!(lookups.pending.contains_key(&key));
    }
}
