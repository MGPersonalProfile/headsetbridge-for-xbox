//! Lo que comparten los bucles de sesión para no quedarse mudos: temporizadores sin ráfagas, sockets UDP a los que un
//! ICMP no tumba y recepción que aguanta errores sueltos.

use std::time::Duration;
use tokio::net::UdpSocket;

/// `tokio::time::interval` sin ráfaga: los ticks perdidos (el hilo único se
/// retrasó) no se disparan todos de golpe, se sigue desde ahora. Todos los
/// temporizadores de las sesiones salen de aquí (lo vigila tools/guardia.mjs).
pub fn intervalo(cada: Duration) -> tokio::time::Interval {
    let mut i = tokio::time::interval(cada);
    i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    i
}

/// Socket UDP para la consola. En Windows, un ICMP «puerto inalcanzable» (una
/// comprobación a un candidato muerto) deja el socket marcado y el siguiente
/// `recv_from` falla con WSAECONNRESET; `SIO_UDP_CONNRESET` lo desactiva.
pub async fn socket_udp() -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind("0.0.0.0:0").await?;
    sin_reset_icmp(&s);
    Ok(s)
}

#[cfg(windows)]
fn sin_reset_icmp(s: &UdpSocket) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{WSAIoctl, SIO_UDP_CONNRESET, SOCKET};
    let no: u32 = 0;
    let mut devueltos = 0u32;
    // SAFETY: el socket vive mientras dura la llamada; entrada de 4 bytes, sin salida ni E/S solapada.
    unsafe {
        WSAIoctl(SOCKET(s.as_raw_socket() as usize), SIO_UDP_CONNRESET, Some(&no as *const u32 as *const _), 4, None, 0, &mut devueltos, None, None);
    }
}

#[cfg(not(windows))]
fn sin_reset_icmp(_: &UdpSocket) {}

/// Errores de `recv_from`: uno suelto (ICMP, un datagrama roto) se registra y
/// se sigue; sólo muchos seguidos, sin ningún paquete bueno entre medias, son
/// un socket roto de verdad.
#[derive(Default)]
pub struct Tolerancia { seguidos: u32, total: u64 }

impl Tolerancia {
    pub const MAXIMO_SEGUIDOS: u32 = 200;
    pub fn bien(&mut self) { self.seguidos = 0 }
    /// Devuelve el texto a registrar (los 5 primeros y uno de cada 100), o el
    /// error si ya son demasiados seguidos.
    pub fn error(&mut self, e: std::io::Error) -> Result<Option<String>, std::io::Error> {
        self.seguidos += 1;
        self.total += 1;
        if self.seguidos >= Self::MAXIMO_SEGUIDOS { return Err(e) }
        Ok((self.total <= 5 || self.total % 100 == 0).then(|| format!("recepción UDP: error ignorado ({e}; {} en total)", self.total)))
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn tolerancia_aguanta_sueltos_y_corta_seguidos() {
        let mut t = Tolerancia::default();
        let reset = || std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        for _ in 0..1000 { assert!(t.error(reset()).is_ok()); t.bien() }
        for _ in 0..Tolerancia::MAXIMO_SEGUIDOS - 1 { assert!(t.error(reset()).is_ok()) }
        assert!(t.error(reset()).is_err());
    }

    #[tokio::test]
    async fn intervalo_sin_rafaga() {
        let mut i = intervalo(Duration::from_millis(10));
        i.tick().await;
        std::thread::sleep(Duration::from_millis(100));
        i.tick().await;
        // Con ráfaga, el siguiente tick sería inmediato; sin ella, 10 ms después.
        let antes = std::time::Instant::now();
        i.tick().await;
        assert!(antes.elapsed() >= Duration::from_millis(5));
    }
}
