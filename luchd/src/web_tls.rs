//! Local HTTPS identity. Keep the certificate across restarts; renew it when the LAN IP changes.
use std::{error::Error, fs, net::IpAddr, os::unix::fs::OpenOptionsExt, path::Path};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    },
    TlsAcceptor,
};

pub struct Identity {
    pub acceptor: TlsAcceptor,
    names: Vec<String>,
    dir: std::path::PathBuf,
}

impl Identity {
    pub fn load(dir: &Path, ip: Option<IpAddr>) -> Result<Self, Box<dyn Error>> {
        let mut names = vec!["localhost".into(), "127.0.0.1".into()];
        if let Some(ip) = ip {
            names.push(ip.to_string());
        }
        let saved = fs::read(dir.join("web-names.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok());
        let previous = saved.clone();
        if let Some(saved) = saved {
            let missing: Vec<_> = names
                .iter()
                .filter(|name| !saved.contains(name))
                .cloned()
                .collect();
            names = saved;
            names.extend(missing);
        }
        if previous.as_ref() != Some(&names) || !dir.join("web-cert.pem").exists() {
            Self::issue(dir, &names)?;
        }
        Ok(Self {
            acceptor: Self::acceptor(dir)?,
            names,
            dir: dir.to_path_buf(),
        })
    }

    /// A laptop may join a different network without restarting the daemon.
    pub fn for_ip(&mut self, ip: IpAddr) -> Result<TlsAcceptor, Box<dyn Error>> {
        if !self.names.contains(&ip.to_string()) {
            let mut names = self.names.clone();
            names.push(ip.to_string());
            Self::issue(&self.dir, &names)?;
            self.acceptor = Self::acceptor(&self.dir)?;
            self.names = names;
        }
        Ok(self.acceptor.clone())
    }

    fn issue(dir: &Path, names: &[String]) -> Result<(), Box<dyn Error>> {
        use std::io::Write;
        let path = dir.join("web-key.pem");
        let key = match fs::read_to_string(&path) {
            Ok(pem) => rcgen::KeyPair::from_pem(&pem)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = rcgen::KeyPair::generate()?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
                file.write_all(key.serialize_pem().as_bytes())?;
                key
            }
            Err(e) => return Err(e.into()),
        };
        let mut params = rcgen::CertificateParams::new(names.to_vec())?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Luch local receiver");
        let cert = params.self_signed(&key)?;
        // Write the names last: an interrupted update is retried on the next start.
        fs::write(dir.join("web-cert.pem"), cert.pem())?;
        fs::write(dir.join("web-names.json"), serde_json::to_vec(names)?)?;
        Ok(())
    }

    fn acceptor(dir: &Path) -> Result<TlsAcceptor, Box<dyn Error>> {
        let certs = CertificateDer::pem_file_iter(dir.join("web-cert.pem"))?
            .collect::<Result<Vec<_>, _>>()?;
        let key = PrivateKeyDer::from_pem_file(dir.join("web-key.pem"))?;
        let provider = rustls::crypto::ring::default_provider();
        let config = rustls::ServerConfig::builder_with_provider(provider.into())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        Ok(TlsAcceptor::from(std::sync::Arc::new(config)))
    }
}
