//! The RemoteDesktop portal session that also carries the screencast: the person approves the picture and the
//! control in one dialog, once (the restore token), and touch lands on the stream it is shown in.

use std::os::fd::OwnedFd;

use ashpd::desktop::{
    remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions},
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    PersistMode, Session,
};

/// A started RemoteDesktop session with its one stream.
pub struct Remote {
    pub rd: RemoteDesktop,
    pub session: Session<RemoteDesktop>,
    /// What the person allowed: keyboard, pointer, touch screen (any of them, or none).
    pub devices: DeviceType,
    /// The PipeWire node of the stream: NotifyTouch* and NotifyPointerMotionAbsolute name it.
    pub node: u32,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

fn token_name(source: SourceType) -> &'static str {
    if source == SourceType::VIRTUAL {
        "restore-token-remote-virtual"
    } else if source == SourceType::WINDOW {
        "restore-token-remote-window"
    } else {
        "restore-token-remote"
    }
}

/// Opens the combined session and the PipeWire remote of its stream. Its own restore token: a screencast
/// token does not restore a RemoteDesktop session.
pub async fn open(source: SourceType, dir: &std::path::Path) -> Result<(Remote, OwnedFd), String> {
    let e = |e: ashpd::Error| format!("remote desktop portal: {e}");
    let token_path = dir.join(token_name(source));
    let token = std::fs::read_to_string(&token_path)
        .ok()
        .filter(|t| !t.trim().is_empty());
    let rd = RemoteDesktop::new().await.map_err(e)?;
    let sc = Screencast::new().await.map_err(e)?;
    let offered = rd
        .available_device_types()
        .await
        .unwrap_or(DeviceType::KEYBOARD | DeviceType::POINTER);
    // A token of another source type restores that one: then ask again, as mirror::portal does.
    for restore in [token.as_deref().map(str::trim), None] {
        let session = rd.create_session(Default::default()).await.map_err(e)?;
        rd.select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(
                    offered
                        & (DeviceType::KEYBOARD | DeviceType::POINTER | DeviceType::TOUCHSCREEN),
                )
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(restore),
        )
        .await
        .map_err(e)?;
        sc.select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::EMBEDDED)
                .set_sources(source)
                .set_multiple(false),
        )
        .await
        .map_err(e)?;
        let started = rd
            .start(&session, None, Default::default())
            .await
            .map_err(e)?
            .response()
            .map_err(e)?;
        let Some(stream) = started.streams().first() else {
            let _ = session.close().await;
            return Err("remote desktop portal: no stream selected".into());
        };
        if stream.source_type().is_some_and(|actual| actual != source) {
            let _ = session.close().await;
            continue;
        }
        if let Some(t) = started.restore_token() {
            let _ = std::fs::write(&token_path, t);
        }
        let (node, position, size) = (stream.pipe_wire_node_id(), stream.position(), stream.size());
        let devices = started.devices();
        let fd = sc
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(e)?;
        return Ok((
            Remote {
                rd,
                session,
                devices,
                node,
                position,
                size,
            },
            fd,
        ));
    }
    Err("remote desktop portal returned a different source type".into())
}
