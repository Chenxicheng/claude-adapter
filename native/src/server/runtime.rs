use std::{env, io, path::PathBuf};

use tokio::{io::AsyncReadExt, net::TcpListener};

pub(super) async fn bind_available(preferred: u16) -> Result<(TcpListener, u16), String> {
    for port in preferred..=u16::MAX {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                let actual_port = listener
                    .local_addr()
                    .map_err(|error| format!("Failed to inspect listener: {error}"))?
                    .port();
                return Ok((listener, actual_port));
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(error) => return Err(format!("Failed to bind port {port}: {error}")),
        }
    }
    Err(format!("No available port at or above {preferred}"))
}

pub(super) async fn shutdown_signal(stdin_enabled: bool) {
    let stdin = async move {
        if stdin_enabled {
            let _ = tokio::io::stdin().read_u8().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
            _ = stdin => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = stdin => {},
        }
    }
}

pub(super) struct Arguments {
    pub(super) config: PathBuf,
    pub(super) port: u16,
}

impl Arguments {
    pub(super) fn parse() -> Result<Self, String> {
        let mut values = env::args().skip(1);
        let mut config = None;
        let mut port = 3080;
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--config" => config = values.next().map(PathBuf::from),
                "--port" => {
                    let value = values.next().ok_or("--port requires a value")?;
                    port = value
                        .parse()
                        .map_err(|_| format!("Invalid port: {value}"))?;
                }
                "--version" | "-V" => {
                    println!("claude-adapter-native {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                other => return Err(format!("Unknown argument: {other}")),
            }
        }
        Ok(Self {
            config: config.ok_or("--config is required")?,
            port,
        })
    }
}
