//! Chromecasts and Google TVs announce `_googlecast._tcp` over mDNS; Avahi browses and resolves them for us, as it
//! does the Luch receivers. The TXT record names the device: `id` (stable), `fn` (its name), `md` (its model) and
//! `ca`, a bit set of what it can do (1: video out).

use std::{collections::HashMap, sync::Arc};

use futures_util::StreamExt;
use zbus::{
    message::Type as MsgType, zvariant::OwnedObjectPath, Connection, MatchRule, MessageStream,
};

use super::{Control, Device, Kind, Tvs};
use crate::{AVAHI, AVAHI_IF_UNSPEC, AVAHI_PROTO_INET, AVAHI_SERVER};

pub const CAST_TYPE: &str = "_googlecast._tcp";
/// `ca` bit: the device shows video (speakers and speaker groups do not).
const VIDEO_OUT: u32 = 1;

/// A Chromecast from its resolved address, port and TXT; None for one without an id or without a screen.
pub fn device(address: &str, port: u16, txt: &HashMap<String, String>) -> Option<Device> {
    let id = txt.get("id").filter(|s| !s.is_empty())?;
    let ca: u32 = txt
        .get("ca")
        .and_then(|c| c.parse().ok())
        .unwrap_or(VIDEO_OUT);
    if ca & VIDEO_OUT == 0 {
        return None;
    }
    Some(Device {
        id: format!("cast:{id}"),
        kind: Kind::Cast,
        name: txt
            .get("fn")
            .cloned()
            .unwrap_or_else(|| "Chromecast".into()),
        model: txt.get("md").cloned().unwrap_or_default(),
        address: address.parse().ok()?,
        port,
        control: Control::Cast,
    })
}

async fn resolve(
    sys: &Connection,
    iface: i32,
    proto: i32,
    name: &str,
    domain: &str,
) -> zbus::Result<Option<Device>> {
    let args = (
        iface,
        proto,
        name,
        CAST_TYPE,
        domain,
        AVAHI_PROTO_INET,
        0u32,
    );
    let reply = sys
        .call_method(
            Some(AVAHI),
            "/",
            Some(AVAHI_SERVER),
            "ResolveService",
            &args,
        )
        .await?;
    #[allow(clippy::type_complexity)]
    let (_, _, _, _, _, _, _, address, port, txt, _): (
        i32,
        i32,
        String,
        String,
        String,
        String,
        i32,
        String,
        u16,
        Vec<Vec<u8>>,
        u32,
    ) = reply.body().deserialize()?;
    Ok(device(&address, port, &crate::txt_map(&txt)))
}

/// Follows `_googlecast._tcp` on the LAN for as long as Avahi runs; `on_change` after every change to the list.
pub async fn browse(
    sys: Connection,
    tvs: Arc<Tvs>,
    on_change: impl Fn() + Send + Sync,
) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(AVAHI)?
        .interface("org.freedesktop.Avahi.ServiceBrowser")?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &sys, None).await?;
    let browser: OwnedObjectPath = sys
        .call_method(
            Some(AVAHI),
            "/",
            Some(AVAHI_SERVER),
            "ServiceBrowserNew",
            &(AVAHI_IF_UNSPEC, AVAHI_PROTO_INET, CAST_TYPE, "", 0u32),
        )
        .await?
        .body()
        .deserialize()?;
    // Avahi's instance name -> our id, for ItemRemove.
    let mut names: HashMap<String, String> = HashMap::new();
    while let Some(msg) = signals.next().await {
        let msg = msg?;
        let header = msg.header();
        if header.path().map(|p| p.as_str()) != Some(browser.as_str()) {
            continue;
        }
        let member = header
            .member()
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if member == "Failure" {
            return Err(zbus::Error::Failure("avahi browser failed".into()));
        }
        if member != "ItemNew" && member != "ItemRemove" {
            continue;
        }
        let (iface, proto, name, _, domain, _): (i32, i32, String, String, String, u32) =
            msg.body().deserialize()?;
        if crate::is_tunnel(&crate::ifname(iface)) {
            continue;
        }
        if member == "ItemRemove" {
            if let Some(id) = names.remove(&name) {
                if tvs.remove(&id) {
                    on_change();
                }
            }
            continue;
        }
        match tokio::time::timeout(
            crate::HANDSHAKE_TIMEOUT,
            resolve(&sys, iface, proto, &name, &domain),
        )
        .await
        {
            Ok(Ok(Some(d))) => {
                names.insert(name, d.id.clone());
                if tvs.saw(d) {
                    on_change();
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => eprintln!("[luchd] resolve {name}: {e}"),
            Err(_) => eprintln!("[luchd] resolve {name}: timed out"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_txt_record_makes_a_chromecast_and_speakers_are_left_out() {
        let txt = crate::txt_map(&[
            b"id=1c4f3a2b9d8e7f60a1b2c3d4e5f60718".to_vec(),
            b"cd=0A1B2C3D4E5F".to_vec(),
            b"md=Chromecast".to_vec(),
            b"fn=Living room TV".to_vec(),
            b"ca=465413".to_vec(),
            b"ve=05".to_vec(),
        ]);
        let d = device("192.168.1.41", 8009, &txt).unwrap();
        assert_eq!(d.id, "cast:1c4f3a2b9d8e7f60a1b2c3d4e5f60718");
        assert_eq!(d.name, "Living room TV");
        assert_eq!(d.model, "Chromecast");
        assert_eq!(
            (d.address.to_string().as_str(), d.port),
            ("192.168.1.41", 8009)
        );
        let mut speaker = txt.clone();
        speaker.insert("ca".into(), "199172".into()); // audio, no video bit
        assert!(device("192.168.1.42", 8009, &speaker).is_none());
        let mut no_id = txt;
        no_id.remove("id");
        assert!(device("192.168.1.41", 8009, &no_id).is_none());
    }
}
