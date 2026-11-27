//! Pairing with receivers (PROTOCOL.md, section 3a, in luch-android): this sender's own certificate, the receivers'
//! certificates pinned once paired, TLS that trusts a receiver by its pinned fingerprint rather than a CA, and the
//! sender's half of the code check.
use std::{
    collections::HashMap,
    error::Error,
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, Mutex},
};

use ring::{digest, rand::SecureRandom};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio_rustls::rustls::{
    self,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

pub const DIGITS: usize = 6;
const NONCE: usize = 16;
const LABEL: &[u8] = b"luch-pair-1";
const SENDER: u8 = b'S';
const RECEIVER: u8 = b'R';

pub type Fingerprint = [u8; 32];

pub fn fingerprint(der: &[u8]) -> Fingerprint {
    let mut fp = [0u8; 32];
    fp.copy_from_slice(digest::digest(&digest::SHA256, der).as_ref());
    fp
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ---------- this sender's identity ----------

/// The certificate this sender shows every receiver: made once, kept in ~/.config/luch (the key readable only by
/// its owner). Its fingerprint is what a receiver pins.
pub struct Identity {
    cert: CertificateDer<'static>,
    key_pem: String,
    pub fp: Fingerprint,
}

impl Identity {
    pub fn load(dir: &Path) -> Result<Self, Box<dyn Error>> {
        let (cert_path, key_path) = (dir.join("sender-cert.pem"), dir.join("sender-key.pem"));
        if !cert_path.exists() || !key_path.exists() {
            let key = rcgen::KeyPair::generate()?;
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Luch sender");
            // Pinned, never checked for dates; far ahead so that no TLS library minds.
            params.not_after = rcgen::date_time_ymd(2055, 1, 1);
            let cert = params.self_signed(&key)?;
            let _ = fs::remove_file(&key_path);
            use std::io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&key_path)?
                .write_all(key.serialize_pem().as_bytes())?;
            fs::write(&cert_path, cert.pem())?;
        }
        let cert = CertificateDer::from_pem_file(&cert_path)?;
        Ok(Identity {
            fp: fingerprint(&cert),
            key_pem: fs::read_to_string(key_path)?,
            cert,
        })
    }

    fn key(&self) -> Result<PrivateKeyDer<'static>, rustls::pki_types::pem::Error> {
        PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes())
    }

    /// TLS to a receiver: this identity as the client certificate, the receiver's certificate taken only if it is
    /// `expected` (when there is one); its fingerprint is in the verifier afterwards.
    pub fn connector(
        &self,
        expected: Option<Fingerprint>,
    ) -> io::Result<(tokio_rustls::TlsConnector, Arc<Pin>)> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let pin = Arc::new(Pin {
            expected,
            seen: Mutex::new(None),
            provider: provider.clone(),
        });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .dangerous()
            .with_custom_certificate_verifier(pin.clone())
            .with_client_auth_cert(
                vec![self.cert.clone()],
                self.key().map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
        Ok((tokio_rustls::TlsConnector::from(Arc::new(config)), pin))
    }
}

/// A receiver's certificate is trusted for being the paired one, not for a CA's signature (there is none on a LAN).
/// The handshake signature is still checked: the receiver must hold that certificate's key.
#[derive(Debug)]
pub struct Pin {
    expected: Option<Fingerprint>,
    pub seen: Mutex<Option<Fingerprint>>,
    provider: Arc<CryptoProvider>,
}

/// What a refused certificate says: a TLS error carries no more than a string.
pub const PIN_MISMATCH: &str = "not the certificate this receiver was paired with";

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        *self.seen.lock().unwrap() = Some(fp);
        match self.expected {
            Some(e) if e != fp => Err(rustls::Error::General(PIN_MISMATCH.into())),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------- receivers this sender paired with ----------

/// ~/.config/luch/paired.json: receiver id -> {"fp", "name", "paired" (unix time)}.
pub fn pinned(dir: &Path) -> HashMap<String, Value> {
    fs::read(dir.join("paired.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn pinned_fp(dir: &Path, id: &str) -> Option<Fingerprint> {
    let all = pinned(dir);
    let v = unhex(all.get(id)?["fp"].as_str()?)?;
    v.try_into().ok()
}

pub fn pin(dir: &Path, id: &str, fp: &Fingerprint, name: &str) -> io::Result<()> {
    let mut all = pinned(dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    all.insert(
        id.to_string(),
        json!({"fp": hex(fp), "name": name, "paired": now}),
    );
    fs::write(dir.join("paired.json"), serde_json::to_vec_pretty(&all)?)
}

pub fn forget(dir: &Path, id: &str) -> io::Result<bool> {
    let mut all = pinned(dir);
    let was = all.remove(id).is_some();
    fs::write(dir.join("paired.json"), serde_json::to_vec_pretty(&all)?)?;
    Ok(was)
}

// ---------- the code check ----------

/// SHA-256 of "luch-pair-1" | role | round | digit (ASCII) | nonce (16) | sender fp (32) | receiver fp (32): the same
/// bytes as the receiver's Pairing.hash.
fn commitment(
    role: u8,
    round: usize,
    digit: u8,
    nonce: &[u8],
    sender: &Fingerprint,
    receiver: &Fingerprint,
) -> [u8; 32] {
    let mut d = digest::Context::new(&digest::SHA256);
    d.update(LABEL);
    d.update(&[role, round as u8, digit]);
    d.update(nonce);
    d.update(sender);
    d.update(receiver);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.finish().as_ref());
    out
}

/// The code as typed: six digits; spaces and dashes are let through ("123 456").
pub fn clean_code(typed: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = typed
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'-'))
        .collect();
    if digits.len() != DIGITS || !digits.iter().all(u8::is_ascii_digit) {
        return Err(format!("the code is {DIGITS} digits, as shown on the TV"));
    }
    Ok(digits)
}

/// Why pairing did not end with both sides pinned.
#[derive(Debug, PartialEq)]
pub enum Failed {
    /// The receiver said no: `pair_refused` with its reason and wait.
    Refused(String, u64),
    /// The receiver's opening did not match: a wrong digit on its side, or someone in between.
    BadReceiver,
    Io(String),
}

/// The sender's half of the code check: one round per digit, the sender committing and opening first (PROTOCOL.md
/// 3a). Ends after the receiver's `paired`.
pub async fn code_check<R, W>(
    rd: &mut R,
    wr: &mut W,
    code: &[u8],
    me: &Fingerprint,
    receiver: &Fingerprint,
) -> Result<(), Failed>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let rng = ring::rand::SystemRandom::new();
    let io = |e: io::Error| Failed::Io(e.to_string());
    for (round, &digit) in code.iter().enumerate() {
        let mut nonce = [0u8; NONCE];
        rng.fill(&mut nonce)
            .map_err(|_| Failed::Io("no randomness".into()))?;
        let c = commitment(SENDER, round, digit, &nonce, me, receiver);
        crate::send(
            wr,
            &json!({"t": "pair_commit", "round": round, "c": hex(&c)}),
        )
        .await
        .map_err(io)?;
        let theirs = expect(rd, "pair_commit", round).await?;
        crate::send(
            wr,
            &json!({"t": "pair_open", "round": round, "n": hex(&nonce)}),
        )
        .await
        .map_err(io)?;
        let open = expect(rd, "pair_open", round).await?;
        let (Some(tc), Some(tn)) = (
            theirs["c"].as_str().and_then(unhex),
            open["n"].as_str().and_then(unhex),
        ) else {
            return Err(Failed::BadReceiver);
        };
        if tn.len() != NONCE || commitment(RECEIVER, round, digit, &tn, me, receiver)[..] != tc[..]
        {
            return Err(Failed::BadReceiver);
        }
    }
    expect(rd, "paired", 0).await.map(|_| ())
}

/// The next message, which must be `t` (for `round`); `pair_refused` ends it with the receiver's reason.
async fn expect<R: AsyncBufRead + Unpin>(
    rd: &mut R,
    t: &str,
    round: usize,
) -> Result<Value, Failed> {
    loop {
        let v = crate::read_msg(rd)
            .await
            .map_err(|e| Failed::Io(e.to_string()))?
            .ok_or_else(|| Failed::Io("the receiver closed the connection".into()))?;
        match v["t"].as_str().unwrap_or("") {
            "ping" | "pong" => continue,
            "pair_refused" => return Err(refused(&v)),
            x if x == t && (t == "paired" || v["round"].as_u64() == Some(round as u64)) => {
                return Ok(v)
            }
            other => return Err(Failed::Io(format!("expected {t}, got {other}"))),
        }
    }
}

pub fn refused(v: &Value) -> Failed {
    Failed::Refused(
        v["reason"].as_str().unwrap_or("refused").to_string(),
        v["retry_s"].as_u64().unwrap_or(0),
    )
}

/// A refusal in words, for the person at the sender.
pub fn explain(f: &Failed, tv: &str) -> String {
    let wait = |s: &u64| {
        if *s >= 120 {
            format!("{} min", s.div_ceil(60))
        } else {
            format!("{s} s")
        }
    };
    match f {
        Failed::Refused(r, s) => match r.as_str() {
            "denied" => format!("{tv} refused the connection"),
            "banned" => format!(
                "{tv} was refused too often from here and ignores this computer for {}",
                wait(s)
            ),
            "busy" => format!(
                "{tv} is already asking about another device; try again in {}",
                wait(s)
            ),
            "wrong_code" => format!("wrong code: try again, {tv} shows a new one"),
            "timeout" => format!("nobody answered on {tv} in time"),
            "dismissed" => format!("the request was closed on {tv}"),
            other => format!("{tv} refused: {other}"),
        },
        Failed::BadReceiver => format!(
            "{tv} failed the code check: the code was mistyped, or another device is in between; nothing was paired"
        ),
        Failed::Io(e) => format!("pairing with {tv} broke off: {e}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The same bytes as the receiver's PairingTest.commitmentIsTheSameOnEveryImplementation (luch-android).
    #[test]
    fn commitment_matches_the_receiver() {
        let nonce: Vec<u8> = (0..16).collect();
        let c = commitment(SENDER, 0, b'4', &nonce, &[0x11; 32], &[0x22; 32]);
        assert_eq!(
            hex(&c),
            "48826a53f4d157eb5596913e25946f39730c10aeafabc935e98c7ac732544373"
        );
    }

    #[test]
    fn codes_are_six_digits_spaces_allowed() {
        assert_eq!(clean_code("123 456").unwrap(), b"123456");
        assert_eq!(clean_code("123-456").unwrap(), b"123456");
        assert!(clean_code("12345").is_err());
        assert!(clean_code("12345a").is_err());
    }

    #[test]
    fn identity_is_kept_with_a_private_key() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("luch-id-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let a = Identity::load(&dir).unwrap();
        let b = Identity::load(&dir).unwrap();
        assert_eq!(a.fp, b.fp);
        let mode = fs::metadata(dir.join("sender-key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pins_are_kept_per_receiver_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("luch-pin-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        assert_eq!(pinned_fp(&dir, "tv"), None);
        pin(&dir, "tv", &[7; 32], "Living room TV").unwrap();
        assert_eq!(pinned_fp(&dir, "tv"), Some([7; 32]));
        assert_eq!(pinned(&dir)["tv"]["name"], "Living room TV");
        assert!(forget(&dir, "tv").unwrap());
        assert!(!forget(&dir, "tv").unwrap());
        assert_eq!(pinned_fp(&dir, "tv"), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refusals_read_well() {
        let tv = "Living room TV";
        assert_eq!(
            explain(&Failed::Refused("banned".into(), 3500), tv),
            "Living room TV was refused too often from here and ignores this computer for 59 min"
        );
        assert_eq!(
            explain(&Failed::Refused("busy".into(), 30), tv),
            "Living room TV is already asking about another device; try again in 30 s"
        );
        assert!(explain(&Failed::BadReceiver, tv).contains("in between"));
    }

    /// The receiver's half, as ControlServer runs it, for tests of luchd against a TV made of Rust.
    pub(crate) async fn receiver_side<R, W>(
        rd: &mut R,
        wr: &mut W,
        code: &[u8],
        sender: &Fingerprint,
        me: &Fingerprint,
    ) -> bool
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        for (round, &digit) in code.iter().enumerate() {
            let c = crate::read_msg(rd).await.unwrap().unwrap();
            let nonce = [round as u8 + 1; NONCE];
            let mine = commitment(RECEIVER, round, digit, &nonce, sender, me);
            crate::send(
                wr,
                &json!({"t": "pair_commit", "round": round, "c": hex(&mine)}),
            )
            .await
            .unwrap();
            let o = crate::read_msg(rd).await.unwrap().unwrap();
            let n = unhex(o["n"].as_str().unwrap()).unwrap();
            let theirs = unhex(c["c"].as_str().unwrap()).unwrap();
            if commitment(SENDER, round, digit, &n, sender, me)[..] != theirs[..] {
                crate::send(wr, &json!({"t": "pair_refused", "reason": "wrong_code"}))
                    .await
                    .unwrap();
                return false;
            }
            crate::send(
                wr,
                &json!({"t": "pair_open", "round": round, "n": hex(&nonce)}),
            )
            .await
            .unwrap();
        }
        crate::send(wr, &json!({"t": "paired"})).await.unwrap();
        true
    }
}
