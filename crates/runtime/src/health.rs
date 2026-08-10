use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

pub trait HealthProbe: Send + Sync {
    fn is_ready(&self) -> Result<bool, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopbackHealthProbe {
    address: SocketAddr,
    path: String,
    timeout: Duration,
}

impl LoopbackHealthProbe {
    pub fn new(
        address: SocketAddr,
        path: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        if !address.ip().is_loopback() {
            return Err("worker health endpoint must use a loopback address".to_owned());
        }
        let path = path.into();
        if !path.starts_with('/') || path.contains(['\r', '\n']) {
            return Err("worker health path must be an absolute HTTP path".to_owned());
        }
        Ok(Self {
            address,
            path,
            timeout,
        })
    }

    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl HealthProbe for LoopbackHealthProbe {
    fn is_ready(&self) -> Result<bool, String> {
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        write!(
            stream,
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.path, self.address
        )
        .map_err(|error| error.to_string())?;
        let mut response = [0u8; 256];
        let read = stream
            .read(&mut response)
            .map_err(|error| error.to_string())?;
        Ok(response[..read].starts_with(b"HTTP/1.1 200")
            || response[..read].starts_with(b"HTTP/1.0 200"))
    }
}

pub(crate) fn validate_loopback(address: IpAddr) -> Result<(), String> {
    address
        .is_loopback()
        .then_some(())
        .ok_or_else(|| "worker endpoint must remain on loopback".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_health_endpoint() {
        let result = LoopbackHealthProbe::new(
            "0.0.0.0:18080".parse().unwrap(),
            "/health",
            Duration::from_secs(1),
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_header_injection_in_path() {
        let result = LoopbackHealthProbe::new(
            "127.0.0.1:18080".parse().unwrap(),
            "/health\r\nInjected: yes",
            Duration::from_secs(1),
        );
        assert!(result.is_err());
    }
}
