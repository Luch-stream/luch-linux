//! The receiver's Bluetooth LE channel (an L2CAP connection-oriented channel on the PSM of its card, PROTOCOL.md
//! 1g): a byte stream that the same TLS as on its control port runs over. The kernel's L2CAP socket does the LE
//! connection and the credit-based flow control.

use std::time::Duration;

use bluer::{
    l2cap::{SocketAddr, Stream},
    Address, AddressType,
};

/// An LE connection and the channel: a few seconds normally, longer while the TV's radio is busy.
pub const OPEN_WITHIN: Duration = Duration::from_secs(20);

/// Opens the channel to `address` (as BlueZ says it, with its type "public" or "random") on `psm`.
pub async fn open(address: &str, kind: &str, psm: u16) -> Result<Stream, String> {
    let addr: Address = address
        .parse()
        .map_err(|_| format!("not a Bluetooth address: {address}"))?;
    let addr_type = match kind {
        "random" => AddressType::LeRandom,
        _ => AddressType::LePublic,
    };
    tokio::time::timeout(
        OPEN_WITHIN,
        Stream::connect(SocketAddr::new(addr, addr_type, psm)),
    )
    .await
    .map_err(|_| "the TV did not open its Bluetooth channel in time".to_string())?
    .map_err(|e| format!("cannot open the TV's Bluetooth channel: {e}"))
}
